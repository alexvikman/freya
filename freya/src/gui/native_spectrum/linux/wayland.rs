//! Wayland: a subsurface of the player's surface, showing a small see-through image of the
//! bars.
//!
//! Freya shares winit's Wayland connection, with its own event queue. Each frame is painted
//! into one of two shared-memory buffers and handed to the compositor; the other is used
//! while the compositor still shows the first. The subsurface takes no input, so clicks reach
//! the player.

use std::fs::File;
use std::os::fd::AsFd;
use std::ptr::NonNull;

use memmap2::MmapMut;
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_subcompositor,
    wl_subsurface, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};

use super::super::{Area, Rect};
use super::{Error, area_size, pixels};

/// What Freya's event queue keeps track of.
#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    shm: Option<wl_shm::WlShm>,
    /// Buffers the compositor gave back since the last frame.
    released: Vec<usize>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), queue, ()));
                }
                "wl_subcompositor" => state.subcompositor = Some(registry.bind(name, 1, queue, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, 1, queue, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.released.push(*index);
        }
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_subsurface::WlSubsurface);
delegate_noop!(State: ignore wl_region::WlRegion);

/// Two buffers of one size in shared memory.
struct Buffers {
    width: u32,
    height: u32,
    memory: MmapMut,
    pool: wl_shm_pool::WlShmPool,
    buffers: [wl_buffer::WlBuffer; 2],
    /// Buffers the compositor is still showing, which must not be painted.
    busy: [bool; 2],
}

impl Buffers {
    fn bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

impl Drop for Buffers {
    fn drop(&mut self) {
        for buffer in &self.buffers {
            buffer.destroy();
        }
        self.pool.destroy();
    }
}

pub struct Layer {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    surface: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    buffers: Option<Buffers>,
    /// Whole-number scale for the buffers; the compositor scales down for fractional scales.
    scale: i32,
    shown: bool,
}

impl Layer {
    pub fn new(
        display: NonNull<std::ffi::c_void>,
        parent: NonNull<std::ffi::c_void>,
    ) -> Result<Self, Error> {
        let connection = Connection::from_backend(foreign_backend(display));
        let mut queue = connection.new_event_queue();
        let handle = queue.handle();
        connection.display().get_registry(&handle, ());
        let mut state = State::default();
        queue.roundtrip(&mut state)?;
        let compositor = state.compositor.clone().ok_or("no wl_compositor")?;
        let subcompositor = state.subcompositor.clone().ok_or("no wl_subcompositor")?;
        state.shm.as_ref().ok_or("no wl_shm")?;

        let parent = wl_surface::WlSurface::from_id(&connection, surface_id(parent)?)?;
        let surface = compositor.create_surface(&handle, ());
        let subsurface = subcompositor.get_subsurface(&surface, &parent, &handle, ());
        // Frames show as soon as they are committed, without waiting for the player's.
        subsurface.set_desync();
        // An empty input region: clicks go to the player underneath.
        let region = compositor.create_region(&handle, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        surface.commit();
        connection.flush()?;
        Ok(Self {
            connection,
            queue,
            state,
            surface,
            subsurface,
            buffers: None,
            scale: 1,
            shown: false,
        })
    }

    pub fn place(&mut self, area: Option<&Area>, scale: f32) -> Result<(), Error> {
        self.scale = (scale.ceil() as i32).max(1);
        match area {
            // Positions are in the player surface's logical pixels. They take effect with the
            // player's next frame, which Slint draws at least twice a second while playing.
            Some(area) => {
                self.subsurface
                    .set_position(area.x.round() as i32, area.y.round() as i32);
                self.shown = true;
            }
            None => {
                self.surface.attach(None, 0, 0);
                self.surface.commit();
                self.shown = false;
            }
        }
        Ok(self.connection.flush()?)
    }

    pub fn refresh(&mut self) -> Result<(), Error> {
        self.queue.dispatch_pending(&mut self.state)?;
        if let Some(buffers) = &mut self.buffers {
            for index in self.state.released.drain(..) {
                buffers.busy[index] = false;
            }
        }
        Ok(())
    }

    pub fn draw(
        &mut self,
        area: &Area,
        bars: &[Rect],
        peaks: &[Option<Rect>],
    ) -> Result<(), Error> {
        if !self.shown {
            return Ok(());
        }
        self.refresh()?;
        let scale = f64::from(self.scale);
        let (width, height) = area_size(area, scale);
        let buffers = self.buffers(width, height)?;
        // Both buffers still on screen: skip this frame rather than wait.
        let Some(index) = buffers.busy.iter().position(|busy| !busy) else {
            return Ok(());
        };
        let size = buffers.bytes();
        let pixels_of = &mut buffers.memory[index * size..(index + 1) * size];
        pixels_of.fill(0);
        let mut paint = |r: &Rect, color: [f64; 4]| {
            let p = pixels(area, r, scale);
            let argb = premultiplied(color).to_le_bytes();
            for y in p.y.max(0)..(p.y + p.height as i32).min(height as i32) {
                for x in p.x.max(0)..(p.x + p.width as i32).min(width as i32) {
                    let at = (y as usize * width as usize + x as usize) * 4;
                    pixels_of[at..at + 4].copy_from_slice(&argb);
                }
            }
        };
        for r in bars {
            paint(r, area.bar_color);
        }
        for r in peaks.iter().flatten() {
            paint(r, area.peak_color);
        }
        buffers.busy[index] = true;
        let buffer = buffers.buffers[index].clone();
        self.surface.set_buffer_scale(self.scale);
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface
            .damage_buffer(0, 0, width as i32, height as i32);
        self.surface.commit();
        Ok(self.connection.flush()?)
    }

    /// The two buffers, made again when the size changed.
    fn buffers(&mut self, width: u32, height: u32) -> Result<&mut Buffers, Error> {
        let fits = self
            .buffers
            .as_ref()
            .is_some_and(|b| (b.width, b.height) == (width, height));
        if !fits {
            self.buffers = None;
            let size = width as usize * height as usize * 4;
            let file = File::from(rustix::fs::memfd_create(
                "freya-spectrum",
                rustix::fs::MemfdFlags::CLOEXEC,
            )?);
            file.set_len((size * 2) as u64)?;
            let memory = map(&file)?;
            let shm = self.state.shm.as_ref().ok_or("no wl_shm")?;
            let handle = self.queue.handle();
            let pool = shm.create_pool(file.as_fd(), (size * 2) as i32, &handle, ());
            let buffer = |index: usize| {
                pool.create_buffer(
                    (index * size) as i32,
                    width as i32,
                    height as i32,
                    width as i32 * 4,
                    wl_shm::Format::Argb8888,
                    &handle,
                    index,
                )
            };
            let buffers = [buffer(0), buffer(1)];
            self.buffers = Some(Buffers {
                width,
                height,
                memory,
                pool,
                buffers,
                busy: [false; 2],
            });
        }
        self.buffers.as_mut().ok_or_else(|| "no buffers".into())
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        self.buffers = None;
        self.subsurface.destroy();
        self.surface.destroy();
        let _ = self.connection.flush();
    }
}

/// ARGB with the color multiplied by its alpha, as Wayland expects.
fn premultiplied([r, g, b, a]: [f64; 4]) -> u32 {
    let a = a.clamp(0.0, 1.0);
    let byte = |v: f64| (v.clamp(0.0, 1.0) * a * 255.0).round() as u32;
    ((a * 255.0).round() as u32) << 24 | byte(r) << 16 | byte(g) << 8 | byte(b)
}

#[allow(unsafe_code, reason = "sharing winit's live Wayland connection")]
fn foreign_backend(display: NonNull<std::ffi::c_void>) -> Backend {
    // SAFETY: the display is winit's open connection, which outlives the player window and so
    // this layer.
    unsafe { Backend::from_foreign_display(display.as_ptr().cast()) }
}

#[allow(
    unsafe_code,
    reason = "naming winit's surface on the shared connection"
)]
fn surface_id(surface: NonNull<std::ffi::c_void>) -> Result<ObjectId, Error> {
    // SAFETY: the pointer is the player window's live wl_surface on the same connection.
    Ok(unsafe { ObjectId::from_ptr(wl_surface::WlSurface::interface(), surface.as_ptr().cast()) }?)
}

#[allow(
    unsafe_code,
    reason = "mapping a memory file only this layer and the compositor use"
)]
fn map(file: &File) -> std::io::Result<MmapMut> {
    // SAFETY: the file is a fresh memfd that nothing else resizes or writes to the same bytes
    // while the compositor is not showing them.
    unsafe { MmapMut::map_mut(file) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_premultiplied() {
        assert_eq!(premultiplied([1.0, 0.5, 0.0, 1.0]), 0xffff8000);
        assert_eq!(premultiplied([1.0, 1.0, 1.0, 0.5]), 0x80808080);
    }
}
