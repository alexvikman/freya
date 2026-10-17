//! macOS: the demo drawn with Metal into a CAMetalLayer on the window's view layer. A thread of
//! its own draws it: waiting for the layer's next drawable paces it to the display's refresh,
//! and while the demo is hidden the thread sleeps.

#![allow(
    unsafe_code,
    reason = "Metal's bindings mark drawing calls unsafe, as the GPU runs what they set up; \
              each is given data of the size and layout the shader declares"
)]

use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSView;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice,
    MTLLibrary, MTLLoadAction, MTLPixelFormat, MTLPrimitiveType, MTLRenderCommandEncoder,
    MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLStoreAction,
};
use objc2_quartz_core::{CALayer, CAMetalDrawable, CAMetalLayer, CATransaction};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;

use super::Area;

pub const NAME: &str = "Metal";
pub const SUPPORTED: bool = true;

const SHADERS: &str = include_str!("demo.metal");

/// What the shader gets each frame; laid out like `Uniforms` in demo.metal.
#[repr(C)]
struct Uniforms {
    time: f32,
    pulse: f32,
    size: [f32; 2],
}

/// What the drawing thread is told.
#[derive(Default)]
struct State {
    shown: bool,
    quit: bool,
    /// The drawable's size, in pixels.
    size: (f64, f64),
    pulse: f32,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The layer, handed to the drawing thread: a CAMetalLayer's drawables are meant to be
/// fetched and presented from a thread of the app's own.
struct ThreadLayer(Retained<CAMetalLayer>);

// SAFETY: the drawing thread only asks the layer for drawables and sets its drawable size,
// which Metal supports from any thread; everything else is done on the main thread.
unsafe impl Send for ThreadLayer {}

/// What the drawing thread draws with.
struct Gpu {
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
}

pub struct Renderer {
    parent: Retained<CALayer>,
    layer: Retained<CAMetalLayer>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Renderer {
    pub fn new(window: &slint::Window) -> Option<Self> {
        let parent = view(window)?.layer()?;
        let device = MTLCreateSystemDefaultDevice()?;
        let gpu = match Gpu::new(&device) {
            Ok(gpu) => gpu,
            Err(e) => {
                log::warn!("the demo cannot use the graphics card: {e}");
                return None;
            }
        };

        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);
        layer.setDisplaySyncEnabled(true);
        layer.setOpaque(true);
        layer.setMasksToBounds(true);
        layer.setHidden(true);
        parent.addSublayer(&layer);

        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let thread = {
            let shared = Arc::clone(&shared);
            let layer = ThreadLayer(layer.clone());
            std::thread::Builder::new()
                .name("freya-demo".into())
                .spawn(move || draw(&shared, &layer, &gpu))
                .ok()?
        };
        Some(Self {
            parent,
            layer,
            shared,
            thread: Some(thread),
        })
    }

    pub fn place(&mut self, area: Option<&Area>) {
        let scale = self.parent.contentsScale();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if let Some(area) = area {
            // The view's layer may count y from the top or from the bottom.
            let y = if self.parent.isGeometryFlipped() {
                area.y
            } else {
                self.parent.bounds().size.height - area.y - area.height
            };
            self.layer.setFrame(CGRect::new(
                CGPoint::new(area.x, y),
                CGSize::new(area.width, area.height),
            ));
            self.layer.setContentsScale(scale);
            self.layer.setCornerRadius(area.corner_radius);
        }
        self.layer.setHidden(area.is_none());
        CATransaction::commit();

        let mut state = self.shared.state();
        state.shown = area.is_some();
        if let Some(area) = area {
            state.size = (
                (area.width * scale).round().max(1.0),
                (area.height * scale).round().max(1.0),
            );
        }
        drop(state);
        self.shared.wake.notify_all();
    }

    pub fn set_pulse(&self, pulse: f32) {
        self.shared.state().pulse = pulse;
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.shared.state().quit = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.layer.removeFromSuperlayer();
    }
}

impl Gpu {
    fn new(device: &ProtocolObject<dyn MTLDevice>) -> Result<Self, String> {
        let queue = device.newCommandQueue().ok_or("no command queue")?;
        let library = device
            .newLibraryWithSource_options_error(&NSString::from_str(SHADERS), None)
            .map_err(|e| format!("the shaders do not compile: {e}"))?;
        let function = |name: &str| {
            library
                .newFunctionWithName(&NSString::from_str(name))
                .ok_or_else(|| format!("no shader {name}"))
        };
        let descriptor = MTLRenderPipelineDescriptor::new();
        let (vertex, fragment) = (function("demo_vertex")?, function("demo_fragment")?);
        descriptor.setVertexFunction(Some(&vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        // SAFETY: attachment 0 exists on every render pipeline descriptor.
        unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }
            .setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|e| format!("no render pipeline: {e}"))?;
        Ok(Self { queue, pipeline })
    }

    /// Draws one frame into `drawable`.
    fn frame(&self, drawable: &ProtocolObject<dyn CAMetalDrawable>, uniforms: &Uniforms) {
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: attachment 0 exists on every render pass descriptor.
        let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setTexture(Some(&drawable.texture()));
        attachment.setLoadAction(MTLLoadAction::DontCare);
        attachment.setStoreAction(MTLStoreAction::Store);
        let Some(commands) = self.queue.commandBuffer() else {
            return;
        };
        let Some(encoder) = commands.renderCommandEncoderWithDescriptor(&pass) else {
            return;
        };
        encoder.setRenderPipelineState(&self.pipeline);
        // SAFETY: `uniforms` has the layout of the shader's `Uniforms` at buffer 0, and Metal
        // copies it before this call returns.
        unsafe {
            encoder.setFragmentBytes_length_atIndex(
                NonNull::from(uniforms).cast(),
                size_of::<Uniforms>(),
                0,
            );
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        encoder.endEncoding();
        commands.presentDrawable(ProtocolObject::from_ref(drawable));
        commands.commit();
    }
}

/// The drawing thread: a frame whenever the layer has a drawable free, which happens at the
/// display's refresh rate; asleep while the demo is hidden.
fn draw(shared: &Shared, layer: &ThreadLayer, gpu: &Gpu) {
    let started = Instant::now();
    let mut drawable_size = (0.0, 0.0);
    let mut frames = 0u32;
    let mut counted = Instant::now();
    let mut cpu_counted = thread_cpu_time();
    loop {
        let (size, pulse) = {
            let mut state = shared.state();
            while !state.shown && !state.quit {
                state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                counted = Instant::now();
                cpu_counted = thread_cpu_time();
                frames = 0;
            }
            if state.quit {
                return;
            }
            (state.size, state.pulse)
        };
        autoreleasepool(|_| {
            if size != drawable_size {
                layer.0.setDrawableSize(CGSize::new(size.0, size.1));
                drawable_size = size;
            }
            if let Some(drawable) = layer.0.nextDrawable() {
                let uniforms = Uniforms {
                    time: started.elapsed().as_secs_f32(),
                    pulse,
                    size: [size.0 as f32, size.1 as f32],
                };
                gpu.frame(&drawable, &uniforms);
            }
        });
        frames += 1;
        if counted.elapsed() >= Duration::from_secs(5) {
            let seconds = counted.elapsed().as_secs_f64();
            let cpu = thread_cpu_time().saturating_sub(cpu_counted).as_secs_f64();
            log::debug!(
                "the demo drew {:.1} frames per second, using {:.2} % of a core",
                f64::from(frames) / seconds,
                cpu * 100.0 / seconds
            );
            frames = 0;
            counted = Instant::now();
            cpu_counted = thread_cpu_time();
        }
    }
}

/// The CPU time this thread has used.
fn thread_cpu_time() -> Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes the time into the timespec it is given.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } != 0 {
        return Duration::ZERO;
    }
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

/// The window's view, from the pointer winit hands out.
fn view(window: &slint::Window) -> Option<Retained<NSView>> {
    let pointer = window
        .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
            RawWindowHandle::AppKit(handle) => Some(handle.ns_view),
            _ => None,
        })
        .flatten()?;
    // SAFETY: the pointer is the live NSView of a window that is open, on the main thread.
    unsafe { Retained::retain(pointer.as_ptr().cast::<NSView>()) }
}
