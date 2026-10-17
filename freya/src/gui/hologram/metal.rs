//! macOS: the hologram drawn with Metal into a CAMetalLayer on the window's view layer. A thread
//! of its own draws it: waiting for the layer's next drawable paces it to the display's
//! refresh, and while the hologram is hidden the thread sleeps. Her poses are decoded on that
//! thread too, so the window never waits for them.

#![allow(
    unsafe_code,
    reason = "Metal's bindings mark drawing and texture calls unsafe, as the GPU runs what they \
              set up; each is given data of the size and layout the shader or texture declares"
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
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLSize, MTLStoreAction, MTLTexture,
    MTLTextureDescriptor,
};
use objc2_quartz_core::{CALayer, CAMetalDrawable, CAMetalLayer, CATransaction};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;

use super::{Area, poses_at};

pub const NAME: &str = "Metal";
pub const SUPPORTED: bool = true;

const SHADERS: &str = include_str!("hologram.metal");
/// Her poses, as textures: tall enough to stay sharp in a large window, small enough to decode
/// in moments.
const POSE_HEIGHT: u32 = 1024;

/// What the shader gets each frame; laid out like `Uniforms` in hologram.metal.
#[repr(C)]
struct Uniforms {
    time: f32,
    pulse: f32,
    size: [f32; 2],
    /// How far the glitch from the current pose to the next has come, 0 to 1.
    transition: f32,
    /// The poses' width over their height.
    aspect: f32,
    /// Changes with each pose, so each glitch looks different.
    seed: f32,
    /// Whether there are poses at all.
    has_poses: f32,
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

type Texture = Retained<ProtocolObject<dyn MTLTexture>>;

/// What the drawing thread draws with.
struct Gpu {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
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
    pub fn new(window: &slint::Window, poses: &'static [&'static [u8]]) -> Option<Self> {
        let parent = view(window)?.layer()?;
        let device = MTLCreateSystemDefaultDevice()?;
        let gpu = match Gpu::new(device.clone()) {
            Ok(gpu) => gpu,
            Err(e) => {
                log::warn!("the hologram cannot use the graphics card: {e}");
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
                .name("freya-hologram".into())
                .spawn(move || {
                    let (textures, count) = gpu.poses(poses);
                    draw(&shared, &layer, &gpu, &textures, count > 0);
                })
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
    fn new(device: Retained<ProtocolObject<dyn MTLDevice>>) -> Result<Self, String> {
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
        let (vertex, fragment) = (function("hologram_vertex")?, function("hologram_fragment")?);
        descriptor.setVertexFunction(Some(&vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        // SAFETY: attachment 0 exists on every render pipeline descriptor.
        unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }
            .setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .map_err(|e| format!("no render pipeline: {e}"))?;
        Ok(Self {
            device,
            queue,
            pipeline,
        })
    }

    /// Her poses as textures with mipmaps (the shader's glow reads the small ones), all the
    /// same size, and how many there are. Poses that cannot be read are left out; with none,
    /// one black texture stands in.
    fn poses(&self, files: &[&[u8]]) -> (Vec<Texture>, usize) {
        let images: Vec<image::RgbaImage> = files
            .iter()
            .filter_map(|bytes| match image::load_from_memory(bytes) {
                Ok(image) => Some(image.into_rgba8()),
                Err(e) => {
                    log::warn!("a hologram pose cannot be read: {e}");
                    None
                }
            })
            .collect();
        // The first pose sets the shape; the others are fitted to it.
        let (width, height) = images.first().map_or((1, 1), |first| {
            let (w, h) = first.dimensions();
            let height = POSE_HEIGHT.min(h);
            ((w * height / h).max(1), height)
        });
        let mut textures: Vec<Texture> = images
            .iter()
            .filter_map(|image| {
                let image = image::imageops::resize(
                    image,
                    width,
                    height,
                    image::imageops::FilterType::CatmullRom,
                );
                self.texture(image.as_raw(), width, height)
            })
            .collect();
        let count = textures.len();
        if textures.is_empty()
            && let Some(black) = self.texture(&[0, 0, 0, 255], 1, 1)
        {
            textures.push(black);
        }
        self.mipmaps(&textures);
        (textures, count)
    }

    fn texture(&self, rgba: &[u8], width: u32, height: u32) -> Option<Texture> {
        let levels = 32 - width.max(height).leading_zeros();
        // SAFETY: a plain descriptor of a 2D texture of the image's size.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                width as usize,
                height as usize,
                levels > 1,
            )
        };
        let texture = self.device.newTextureWithDescriptor(&descriptor)?;
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: width as usize,
                height: height as usize,
                depth: 1,
            },
        };
        // SAFETY: `rgba` holds width × height RGBA pixels, 4 bytes each, row by row.
        unsafe {
            texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                NonNull::from(rgba).cast(),
                width as usize * 4,
            );
        }
        Some(texture)
    }

    fn mipmaps(&self, textures: &[Texture]) {
        let Some(commands) = self.queue.commandBuffer() else {
            return;
        };
        let Some(blit) = commands.blitCommandEncoder() else {
            return;
        };
        for texture in textures {
            if texture.mipmapLevelCount() > 1 {
                blit.generateMipmapsForTexture(texture);
            }
        }
        blit.endEncoding();
        commands.commit();
        commands.waitUntilCompleted();
    }

    /// Draws one frame into `drawable`.
    fn frame(
        &self,
        drawable: &ProtocolObject<dyn CAMetalDrawable>,
        uniforms: &Uniforms,
        current: &Texture,
        next: &Texture,
    ) {
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
        // copies it before this call returns; the textures are the shader's textures 0 and 1.
        unsafe {
            encoder.setFragmentBytes_length_atIndex(
                NonNull::from(uniforms).cast(),
                size_of::<Uniforms>(),
                0,
            );
            encoder.setFragmentTexture_atIndex(Some(current), 0);
            encoder.setFragmentTexture_atIndex(Some(next), 1);
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        encoder.endEncoding();
        commands.presentDrawable(ProtocolObject::from_ref(drawable));
        commands.commit();
    }
}

/// The drawing thread: a frame whenever the layer has a drawable free, which happens at the
/// display's refresh rate; asleep while the hologram is hidden.
fn draw(shared: &Shared, layer: &ThreadLayer, gpu: &Gpu, poses: &[Texture], has_poses: bool) {
    let started = Instant::now();
    let mut drawable_size = (0.0, 0.0);
    let mut frames = 0u32;
    let mut counted = Instant::now();
    let mut cpu_counted = thread_cpu_time();
    let aspect = poses
        .first()
        .map_or(0.75, |t| t.width() as f32 / t.height().max(1) as f32);
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
            let Some(drawable) = layer.0.nextDrawable() else {
                return;
            };
            let time = started.elapsed().as_secs_f32();
            let (current, next, transition) = poses_at(time, poses.len());
            let uniforms = Uniforms {
                time,
                pulse,
                size: [size.0 as f32, size.1 as f32],
                transition,
                aspect,
                seed: current as f32,
                has_poses: if has_poses { 1.0 } else { 0.0 },
            };
            gpu.frame(&drawable, &uniforms, &poses[current], &poses[next]);
        });
        frames += 1;
        if counted.elapsed() >= Duration::from_secs(5) {
            let seconds = counted.elapsed().as_secs_f64();
            let cpu = thread_cpu_time().saturating_sub(cpu_counted).as_secs_f64();
            log::debug!(
                "the hologram drew {:.1} frames per second, using {:.2} % of a core",
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
