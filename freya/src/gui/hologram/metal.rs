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
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLSize, MTLStorageMode, MTLStoreAction,
    MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use objc2_quartz_core::{CALayer, CAMetalDrawable, CAMetalLayer, CATransaction};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;

use super::{Area, poses_at};

pub const NAME: &str = "Metal";
pub const SUPPORTED: bool = true;

const SHADERS: &str = include_str!("hologram.metal");

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
    /// The graphics card could not be set up: nothing will be drawn.
    failed: bool,
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

/// The city is drawn at this fraction of the drawable's size: the fog blurs it anyway, and it
/// is the most work per pixel. Drawn full size it took 16 ms a frame in a large window.
const CITY_SCALE: usize = 4;
/// The city is drawn again this often: it changes slowly (fog, flicker, ripples), while she and
/// the rain are drawn every frame.
const CITY_INTERVAL: f32 = 1.0 / 30.0;
/// Each frame shows at least this long: 60 frames a second look as smooth for rain and
/// glitches as the 120 a ProMotion display could take, at half the graphics card's work.
const FRAME_SECONDS: f64 = 1.0 / 60.0;
/// The city's texture: half floats, so its fog has no visible steps when scaled up.
const CITY_FORMAT: MTLPixelFormat = MTLPixelFormat::RGBA16Float;

/// What the drawing thread draws with.
/// The shader's clock starts over after this long. Its noise multiplies the time by hundreds
/// in 32-bit floats, which after a few minutes have too few digits left for it: the glitches
/// stopped after about five minutes and everything froze after twenty. Starting over is a
/// jump in the rain and the sway, once every two minutes, among the glitches.
const SHADER_TIME_WRAP: f64 = 120.0;

/// The time handed to the shader, `elapsed` since the hologram started.
fn shader_time(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f64() % SHADER_TIME_WRAP) as f32
}

struct Gpu {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    city_pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
}

pub struct Renderer {
    parent: Retained<CALayer>,
    layer: Retained<CAMetalLayer>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Renderer {
    /// Adds the layer, and starts the drawing thread, which sets up the graphics card and her
    /// poses: that takes moments (Metal itself up to a second and a half the first time), so
    /// none of it is done on the window's thread.
    pub fn new(window: &slint::Window, poses: &'static [&'static [u8]]) -> Option<Self> {
        let parent = view(window)?.layer()?;
        let layer = CAMetalLayer::new();
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
                    let Some((gpu, textures, count)) = set_up(&layer, poses) else {
                        shared.state().failed = true;
                        return;
                    };
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

    /// Whether the graphics card could not be set up, so nothing will be drawn.
    pub fn failed(&self) -> bool {
        self.shared.state().failed
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

/// The graphics card, the shaders and her poses, ready to draw into `layer`.
fn set_up(layer: &ThreadLayer, poses: &[&[u8]]) -> Option<(Gpu, Vec<Texture>, usize)> {
    let started = Instant::now();
    let Some(device) = MTLCreateSystemDefaultDevice() else {
        log::warn!("the hologram finds no graphics card");
        return None;
    };
    let metal = started.elapsed();
    let gpu = match Gpu::new(device) {
        Ok(gpu) => gpu,
        Err(e) => {
            log::warn!("the hologram cannot use the graphics card: {e}");
            return None;
        }
    };
    CATransaction::begin();
    layer.0.setDevice(Some(&gpu.device));
    CATransaction::commit();
    let shaders = started.elapsed() - metal;
    let (textures, count) = gpu.poses(poses);
    log::info!(
        "the hologram was ready in {:?} (Metal {metal:?}, shaders {shaders:?}, {count} poses {:?})",
        started.elapsed(),
        started.elapsed() - metal - shaders
    );
    Some((gpu, textures, count))
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
        let vertex = function("hologram_vertex")?;
        let pipeline = |fragment: &str, format: MTLPixelFormat| {
            let descriptor = MTLRenderPipelineDescriptor::new();
            let fragment = function(fragment)?;
            descriptor.setVertexFunction(Some(&vertex));
            descriptor.setFragmentFunction(Some(&fragment));
            // SAFETY: attachment 0 exists on every render pipeline descriptor.
            unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }
                .setPixelFormat(format);
            device
                .newRenderPipelineStateWithDescriptor_error(&descriptor)
                .map_err(|e| format!("no render pipeline: {e}"))
        };
        let main = pipeline("hologram_fragment", MTLPixelFormat::BGRA8Unorm)?;
        let city_pipeline = pipeline("city_fragment", CITY_FORMAT)?;
        Ok(Self {
            device,
            queue,
            pipeline: main,
            city_pipeline,
        })
    }

    /// Her poses as textures with mipmaps (the shader's glow reads the small ones), and how
    /// many there are. Poses that cannot be read are left out; with none,
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
        // Each at its own size: the graphics card scales them as it draws. (Scaling them here
        // took four seconds.) Poses of another shape than the first are stretched to it.
        let mut textures: Vec<Texture> = images
            .iter()
            .filter_map(|image| {
                let (width, height) = image.dimensions();
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

    /// The texture the city is drawn into for a drawable of `size` pixels.
    fn city_texture(&self, size: (f64, f64)) -> Option<Texture> {
        let width = (size.0 as usize / CITY_SCALE).max(1);
        let height = (size.1 as usize / CITY_SCALE).max(1);
        // SAFETY: a plain descriptor of a 2D texture.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                CITY_FORMAT,
                width,
                height,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        descriptor.setStorageMode(MTLStorageMode::Private);
        self.device.newTextureWithDescriptor(&descriptor)
    }

    /// Draws one frame into `drawable` and shows it.
    fn frame(&self, drawable: &ProtocolObject<dyn CAMetalDrawable>, scene: &Scene<'_>) {
        if let Some(commands) = self.encode(&drawable.texture(), scene) {
            commands.presentDrawable_afterMinimumDuration(
                ProtocolObject::from_ref(drawable),
                FRAME_SECONDS,
            );
            commands.commit();
        }
    }

    /// The commands that draw one frame into `target`, not yet committed: the city into its
    /// texture, then the full picture from it.
    fn encode(
        &self,
        target: &ProtocolObject<dyn MTLTexture>,
        scene: &Scene<'_>,
    ) -> Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>> {
        let commands = self.queue.commandBuffer()?;
        let pass = |into: &ProtocolObject<dyn MTLTexture>,
                    pipeline: &ProtocolObject<dyn MTLRenderPipelineState>| {
            let descriptor = MTLRenderPassDescriptor::new();
            // SAFETY: attachment 0 exists on every render pass descriptor.
            let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
            attachment.setTexture(Some(into));
            attachment.setLoadAction(MTLLoadAction::DontCare);
            attachment.setStoreAction(MTLStoreAction::Store);
            let encoder = commands.renderCommandEncoderWithDescriptor(&descriptor)?;
            encoder.setRenderPipelineState(pipeline);
            // SAFETY: `uniforms` has the layout of the shader's `Uniforms` at buffer 0, and
            // Metal copies it before this call returns.
            unsafe {
                encoder.setFragmentBytes_length_atIndex(
                    NonNull::from(scene.uniforms).cast(),
                    size_of::<Uniforms>(),
                    0,
                );
            }
            Some(encoder)
        };

        if scene.draw_city {
            let city = pass(scene.city, &self.city_pipeline)?;
            // SAFETY: a triangle covering the target, as the vertex shader makes it.
            unsafe {
                city.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3)
            };
            city.endEncoding();
        }

        let main = pass(target, &self.pipeline)?;
        // SAFETY: the shader's textures 0, 1 and 2; then a triangle covering the target.
        unsafe {
            main.setFragmentTexture_atIndex(Some(scene.current), 0);
            main.setFragmentTexture_atIndex(Some(scene.next), 1);
            main.setFragmentTexture_atIndex(Some(scene.city), 2);
            main.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        main.endEncoding();
        Some(commands)
    }
}

/// One frame's inputs: the shader's values, her poses and the city's texture, and whether the
/// city is drawn again this frame.
struct Scene<'a> {
    uniforms: &'a Uniforms,
    current: &'a Texture,
    next: &'a Texture,
    city: &'a Texture,
    draw_city: bool,
}

/// The drawing thread: a frame whenever the layer has a drawable free, which happens at the
/// display's refresh rate, at most 60 times a second; asleep while the hologram is hidden.
fn draw(shared: &Shared, layer: &ThreadLayer, gpu: &Gpu, poses: &[Texture], has_poses: bool) {
    let started = Instant::now();
    let mut drawable_size = (0.0, 0.0);
    let mut city: Option<Texture> = None;
    let mut city_drawn = f32::NEG_INFINITY;
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
            if size != drawable_size || city.is_none() {
                layer.0.setDrawableSize(CGSize::new(size.0, size.1));
                drawable_size = size;
                city = gpu.city_texture(size);
                city_drawn = f32::NEG_INFINITY;
            }
            let Some(city) = &city else {
                return;
            };
            let Some(drawable) = layer.0.nextDrawable() else {
                return;
            };
            let time = started.elapsed().as_secs_f32();
            let draw_city = time - city_drawn >= CITY_INTERVAL;
            if draw_city {
                city_drawn = time;
            }
            let (current, next, transition) = poses_at(time, poses.len());
            let uniforms = Uniforms {
                time: shader_time(started.elapsed()),
                pulse,
                size: [size.0 as f32, size.1 as f32],
                transition,
                aspect,
                seed: current as f32,
                has_poses: if has_poses { 1.0 } else { 0.0 },
            };
            gpu.frame(
                &drawable,
                &Scene {
                    uniforms: &uniforms,
                    current: &poses[current],
                    next: &poses[next],
                    city,
                    draw_city,
                },
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The poses must be ready in moments: they were once scaled on the CPU, which took four
    /// seconds before the hologram showed.
    #[test]
    fn poses_are_ready_quickly() {
        let Some(device) = MTLCreateSystemDefaultDevice() else {
            return;
        };
        let gpu = Gpu::new(device).expect("the shaders compile");
        let started = Instant::now();
        let (textures, count) = gpu.poses(super::super::poses::POSES);
        let took = started.elapsed();
        println!("{count} poses in {took:?}");
        assert_eq!(textures.len(), count.max(1));
        assert!(took < Duration::from_millis(500), "{took:?}");
    }

    /// What a frame costs, drawn into a texture the size of a large window: the CPU's time on
    /// this thread, and the graphics card's. Run with `--release --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn frame_cost() {
        const FRAMES: u32 = 600;
        let device = MTLCreateSystemDefaultDevice().expect("a Metal device");
        let gpu = Gpu::new(device).expect("the shaders compile");
        let (poses, _) = gpu.poses(super::super::poses::POSES);
        // A 1400 × 900 point view on a Retina display.
        let (width, height) = (2800usize, 1800usize);
        // SAFETY: a plain descriptor of a 2D texture.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                width,
                height,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::RenderTarget);
        descriptor.setStorageMode(MTLStorageMode::Private);
        let target = gpu.device.newTextureWithDescriptor(&descriptor).unwrap();
        let city = gpu.city_texture((width as f64, height as f64)).unwrap();

        let cpu_start = thread_cpu_time();
        let started = Instant::now();
        let mut gpu_seconds = 0.0;
        for frame in 0..FRAMES {
            let time = frame as f32 / 120.0;
            let (current, next, transition) = poses_at(time, poses.len());
            let uniforms = Uniforms {
                time,
                pulse: 0.5,
                size: [width as f32, height as f32],
                transition,
                aspect: 0.75,
                seed: current as f32,
                has_poses: 1.0,
            };
            autoreleasepool(|_| {
                let scene = Scene {
                    uniforms: &uniforms,
                    current: &poses[current],
                    next: &poses[next],
                    city: &city,
                    draw_city: frame % 4 == 0,
                };
                let commands = gpu.encode(&target, &scene).unwrap();
                commands.commit();
                commands.waitUntilCompleted();
                gpu_seconds += commands.GPUEndTime() - commands.GPUStartTime();
            });
        }
        let cpu = (thread_cpu_time() - cpu_start).as_secs_f64() / f64::from(FRAMES);
        let gpu_time = gpu_seconds / f64::from(FRAMES);
        println!(
            "per frame: CPU {:.0} µs, graphics card {:.2} ms ({:?} for {FRAMES} frames)",
            cpu * 1e6,
            gpu_time * 1e3,
            started.elapsed()
        );
        println!(
            "at 120 frames a second: {:.2} % of a CPU core, {:.0} % of the graphics card's time",
            cpu * 120.0 * 100.0,
            gpu_time * 120.0 * 100.0
        );
    }
}
