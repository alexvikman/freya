//! X11: a child window over the spectrum's box, drawn by the X server.
//!
//! X11 windows are not see-through without a compositing manager, so the box is filled with
//! the skin's background color. Each frame is drawn into a pixmap and copied to the window in
//! one go, so it never flickers. The window lets clicks through to the player.

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    ChangeGCAux, ClipOrdering, ConfigureWindowAux, ConnectionExt as _, CreateGCAux,
    CreateWindowAux, EventMask, Gcontext, Pixmap, Rectangle, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use super::super::{Area, Rect};
use super::{Error, Pixels, area_size, blend, pixels};

pub struct Layer {
    /// Freya's own connection; windows live in the server, so this one can add a child to
    /// winit's window.
    connection: RustConnection,
    window: Window,
    gc: Gcontext,
    depth: u8,
    /// The off-screen picture each frame is drawn into, and its size.
    pixmap: Option<(Pixmap, u32, u32)>,
    scale: f64,
    shown: bool,
    /// The last frame, drawn again when the window is uncovered.
    last: Option<(Area, Vec<Rect>, Vec<Option<Rect>>)>,
}

impl Layer {
    pub fn new(parent: Window) -> Result<Self, Error> {
        let (connection, _) = x11rb::connect(None)?;
        let window = connection.generate_id()?;
        connection.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            parent,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::EXPOSURE),
        )?;
        // An empty input shape: clicks go to the player window underneath.
        connection.shape_rectangles(
            SO::SET,
            SK::INPUT,
            ClipOrdering::UNSORTED,
            window,
            0,
            0,
            &[],
        )?;
        let gc = connection.generate_id()?;
        connection.create_gc(gc, window, &CreateGCAux::new())?;
        let depth = connection.get_geometry(window)?.reply()?.depth;
        connection.flush()?;
        Ok(Self {
            connection,
            window,
            gc,
            depth,
            pixmap: None,
            scale: 1.0,
            shown: false,
            last: None,
        })
    }

    pub fn place(&mut self, area: Option<&Area>, scale: f32) -> Result<(), Error> {
        self.scale = f64::from(scale);
        let Some(area) = area else {
            if self.shown {
                self.connection.unmap_window(self.window)?;
                self.shown = false;
            }
            return Ok(self.connection.flush()?);
        };
        let (width, height) = area_size(area, self.scale);
        self.connection.configure_window(
            self.window,
            &ConfigureWindowAux::new()
                .x((area.x * self.scale).round() as i32)
                .y((area.y * self.scale).round() as i32)
                .width(width)
                .height(height),
        )?;
        if !self.shown {
            self.connection.map_window(self.window)?;
            self.shown = true;
        }
        // Colors or size may have changed: draw the last frame again in the new place, or
        // before the first frame, the empty box.
        let (bars, peaks) = self
            .last
            .take()
            .map(|(_, bars, peaks)| (bars, peaks))
            .unwrap_or_default();
        self.draw(area, &bars, &peaks)?;
        Ok(self.connection.flush()?)
    }

    pub fn refresh(&mut self) -> Result<(), Error> {
        let mut uncovered = false;
        while let Some(event) = self.connection.poll_for_event()? {
            uncovered |= matches!(event, Event::Expose(_));
        }
        if uncovered && let Some((area, bars, peaks)) = self.last.take() {
            self.draw(&area, &bars, &peaks)?;
        }
        Ok(())
    }

    pub fn draw(
        &mut self,
        area: &Area,
        bars: &[Rect],
        peaks: &[Option<Rect>],
    ) -> Result<(), Error> {
        let (width, height) = area_size(area, self.scale);
        let pixmap = self.pixmap(width, height)?;
        let rects = |rects: &mut dyn Iterator<Item = Pixels>| -> Vec<Rectangle> {
            rects
                .map(|p| Rectangle {
                    x: p.x as i16,
                    y: p.y as i16,
                    width: p.width as u16,
                    height: p.height as u16,
                })
                .collect()
        };
        let background = Rectangle {
            x: 0,
            y: 0,
            width: width as u16,
            height: height as u16,
        };
        self.fill(pixmap, area.background, area.background, &[background])?;
        let bar_rects = rects(&mut bars.iter().map(|r| pixels(area, r, self.scale)));
        self.fill(pixmap, area.bar_color, area.background, &bar_rects)?;
        let peak_rects = rects(&mut peaks.iter().flatten().map(|r| pixels(area, r, self.scale)));
        self.fill(pixmap, area.peak_color, area.background, &peak_rects)?;
        self.connection.copy_area(
            pixmap,
            self.window,
            self.gc,
            0,
            0,
            0,
            0,
            width as u16,
            height as u16,
        )?;
        self.connection.flush()?;
        self.last = Some((area.clone(), bars.to_vec(), peaks.to_vec()));
        Ok(())
    }

    /// The off-screen picture, made again when the size changed.
    fn pixmap(&mut self, width: u32, height: u32) -> Result<Pixmap, Error> {
        if let Some((pixmap, w, h)) = self.pixmap
            && (w, h) == (width, height)
        {
            return Ok(pixmap);
        }
        if let Some((old, _, _)) = self.pixmap.take() {
            self.connection.free_pixmap(old)?;
        }
        let pixmap = self.connection.generate_id()?;
        self.connection.create_pixmap(
            self.depth,
            pixmap,
            self.window,
            width as u16,
            height as u16,
        )?;
        self.pixmap = Some((pixmap, width, height));
        Ok(pixmap)
    }

    fn fill(
        &self,
        pixmap: Pixmap,
        color: [f64; 4],
        background: [f64; 4],
        rects: &[Rectangle],
    ) -> Result<(), Error> {
        let pixel = pixel(blend(color, background), self.depth);
        self.connection
            .change_gc(self.gc, &ChangeGCAux::new().foreground(pixel))?;
        self.connection
            .poly_fill_rectangle(pixmap, self.gc, rects)?;
        Ok(())
    }
}

/// A pixel of TrueColor, as every current X server uses: 0xRRGGBB, and opaque in the top byte
/// at depth 32. The window takes its parent's visual, which is ARGB where Slint asks for a
/// window that can be see-through, and a compositor shows alpha 0 as a hole.
fn pixel([r, g, b]: [u8; 3], depth: u8) -> u32 {
    let alpha = if depth == 32 { 0xff00_0000 } else { 0 };
    alpha | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
}

impl Drop for Layer {
    fn drop(&mut self) {
        if let Some((pixmap, _, _)) = self.pixmap.take() {
            let _ = self.connection.free_pixmap(pixmap);
        }
        let _ = self.connection.free_gc(self.gc);
        let _ = self.connection.destroy_window(self.window);
        let _ = self.connection.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_are_opaque_in_windows_with_alpha() {
        assert_eq!(pixel([0x12, 0x34, 0x56], 24), 0x0012_3456);
        assert_eq!(pixel([0x12, 0x34, 0x56], 32), 0xff12_3456);
    }
}
