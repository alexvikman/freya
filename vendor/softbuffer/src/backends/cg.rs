//! Softbuffer implementation using Core Animation and IOSurface.
//!
//! Freya's patch (see vendor/README.md): the pixels are kept between frames, so the renderer
//! redraws only what changed, and they reach the window server through IOSurfaces it shows
//! directly, copying only the changed rectangles. The upstream version handed Core Animation
//! a new CGImage in device RGB every frame, which it copied and color-converted on the CPU:
//! about 26 ms per frame for a full-screen window on a Retina MacBook, however little changed.
use crate::backend_interface::*;
use crate::error::InitError;
use crate::{Rect, SoftBufferError};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, MainThreadMarker, Message};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint};
use objc2_core_graphics::kCGColorSpaceSRGB;
use objc2_io_surface::{
    kIOSurfaceBytesPerElement, kIOSurfaceColorSpace, kIOSurfaceHeight, kIOSurfacePixelFormat,
    kIOSurfaceWidth, IOSurfaceLockOptions, IOSurfaceRef,
};
use objc2_foundation::{
    ns_string, NSDictionary, NSKeyValueChangeKey, NSKeyValueChangeNewKey,
    NSKeyValueObservingOptions, NSNumber, NSObject, NSObjectNSKeyValueObserverRegistration,
    NSString, NSValue,
};
use objc2_quartz_core::{kCAGravityTopLeft, CALayer, CATransaction};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};

use std::ffi::c_void;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ops::Deref;
use std::ptr;

/// The window server may hold this many surfaces; beyond that the oldest is reused.
const MAX_SURFACES: usize = 3;
/// Beyond this many changed rectangles waiting for a surface, it is copied whole instead.
const MAX_STALE_RECTS: usize = 16;
/// `kCVPixelFormatType_32BGRA`: softbuffer's `0RGB` pixels as they lie in memory.
const PIXEL_FORMAT_BGRA: i32 = 0x4247_5241;

/// One surface the window server shows, and what changed in the picture since it was filled.
#[derive(Debug)]
struct Surface {
    surface: SendSurface,
    /// The rectangles to copy before it is shown again; `None` for all of it.
    stale: Option<Vec<Rect>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SoftbufferObserver"]
    #[ivars = SendCALayer]
    #[derive(Debug)]
    struct Observer;

    /// NSKeyValueObserving
    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut c_void,
        ) {
            self.update(key_path, change);
        }
    }
);

impl Observer {
    fn new(layer: &CALayer) -> Retained<Self> {
        let this = Self::alloc().set_ivars(SendCALayer(layer.retain()));
        unsafe { msg_send![super(this), init] }
    }

    fn update(
        &self,
        key_path: Option<&NSString>,
        change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
    ) {
        let layer = self.ivars();

        let change =
            change.expect("requested a change dictionary in `addObserver`, but none was provided");
        let new = change
            .objectForKey(unsafe { NSKeyValueChangeNewKey })
            .expect("requested change dictionary did not contain `NSKeyValueChangeNewKey`");

        // NOTE: Setting these values usually causes a quarter second animation to occur, which is
        // undesirable.
        //
        // However, since we're setting them inside an observer, there already is a transaction
        // ongoing, and as such we don't need to wrap this in a `CATransaction` ourselves.

        if key_path == Some(ns_string!("contentsScale")) {
            let new = new.downcast::<NSNumber>().unwrap();
            let scale_factor = new.as_cgfloat();

            // Set the scale factor of the layer to match the root layer when it changes (e.g. if
            // moved to a different monitor, or monitor settings changed).
            layer.setContentsScale(scale_factor);
        } else if key_path == Some(ns_string!("bounds")) {
            let new = new.downcast::<NSValue>().unwrap();
            let bounds = new.get_rect().expect("new bounds value was not CGRect");

            // Set `bounds` and `position` so that the new layer is inside the superlayer.
            //
            // This differs from just setting the `bounds`, as it also takes into account any
            // translation that the superlayer may have that we'd want to preserve.
            layer.setFrame(bounds);
        } else {
            panic!("unknown observed keypath {key_path:?}");
        }
    }
}

#[derive(Debug)]
pub struct CGImpl<D, W> {
    /// Our layer.
    layer: SendCALayer,
    /// The layer that our layer was created from.
    ///
    /// Can also be retrieved from `layer.superlayer()`.
    root_layer: SendCALayer,
    observer: Retained<Observer>,
    /// The picture, kept between frames so that only what changed is drawn again.
    pixels: Vec<u32>,
    /// 0 when `pixels` hold nothing yet (new, or resized), else 1: they hold the last frame.
    age: u8,
    /// The surfaces, the one shown last at the end.
    surfaces: Vec<Surface>,
    /// The width of the underlying buffer.
    width: usize,
    /// The height of the underlying buffer.
    height: usize,
    window_handle: W,
    _display: PhantomData<D>,
}

impl<D, W> Drop for CGImpl<D, W> {
    fn drop(&mut self) {
        // SAFETY: Registered in `new`, must be removed before the observer is deallocated.
        unsafe {
            self.root_layer
                .removeObserver_forKeyPath(&self.observer, ns_string!("contentsScale"));
            self.root_layer
                .removeObserver_forKeyPath(&self.observer, ns_string!("bounds"));
        }
    }
}

impl<D: HasDisplayHandle, W: HasWindowHandle> SurfaceInterface<D, W> for CGImpl<D, W> {
    type Context = D;
    type Buffer<'a>
        = BufferImpl<'a, D, W>
    where
        Self: 'a;

    fn new(window_src: W, _display: &D) -> Result<Self, InitError<W>> {
        // `NSView`/`UIView` can only be accessed from the main thread.
        let _mtm = MainThreadMarker::new().ok_or(SoftBufferError::PlatformError(
            Some("can only access Core Graphics handles from the main thread".to_string()),
            None,
        ))?;

        let root_layer = match window_src.window_handle()?.as_raw() {
            RawWindowHandle::AppKit(handle) => {
                // SAFETY: The pointer came from `WindowHandle`, which ensures that the
                // `AppKitWindowHandle` contains a valid pointer to an `NSView`.
                //
                // We use `NSObject` here to avoid importing `objc2-app-kit`.
                let view: &NSObject = unsafe { handle.ns_view.cast().as_ref() };

                // Force the view to become layer backed
                let _: () = unsafe { msg_send![view, setWantsLayer: Bool::YES] };

                // SAFETY: `-[NSView layer]` returns an optional `CALayer`
                let layer: Option<Retained<CALayer>> = unsafe { msg_send![view, layer] };
                layer.expect("failed making the view layer-backed")
            }
            RawWindowHandle::UiKit(handle) => {
                // SAFETY: The pointer came from `WindowHandle`, which ensures that the
                // `UiKitWindowHandle` contains a valid pointer to an `UIView`.
                //
                // We use `NSObject` here to avoid importing `objc2-ui-kit`.
                let view: &NSObject = unsafe { handle.ui_view.cast().as_ref() };

                // SAFETY: `-[UIView layer]` returns `CALayer`
                let layer: Retained<CALayer> = unsafe { msg_send![view, layer] };
                layer
            }
            _ => return Err(InitError::Unsupported(window_src)),
        };

        // Add a sublayer, to avoid interfering with the root layer, since setting the contents of
        // e.g. a view-controlled layer is brittle.
        let layer = CALayer::new();
        root_layer.addSublayer(&layer);

        // Set the anchor point and geometry. Softbuffer's uses a coordinate system with the origin
        // in the top-left corner.
        //
        // NOTE: This doesn't really matter unless we start modifying the `position` of our layer
        // ourselves, but it's nice to have in place.
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        layer.setGeometryFlipped(true);

        // Do not use auto-resizing mask.
        //
        // This is done to work around a bug in macOS 14 and above, where views using auto layout
        // may end up setting fractional values as the bounds, and that in turn doesn't propagate
        // properly through the auto-resizing mask and with contents gravity.
        //
        // Instead, we keep the bounds of the layer in sync with the root layer using an observer,
        // see below.
        //
        // layer.setAutoresizingMask(kCALayerHeightSizable | kCALayerWidthSizable);

        let observer = Observer::new(&layer);
        // Observe changes to the root layer's bounds and scale factor, and apply them to our layer.
        //
        // The previous implementation updated the scale factor inside `resize`, but this works
        // poorly with transactions, and is generally inefficient. Instead, we update the scale
        // factor only when needed because the super layer's scale factor changed.
        //
        // Note that inherent in this is an explicit design decision: We control the `bounds` and
        // `contentsScale` of the layer directly, and instead let the `resize` call that the user
        // controls only be the size of the underlying buffer.
        //
        // SAFETY: Observer deregistered in `Drop` before the observer object is deallocated.
        unsafe {
            root_layer.addObserver_forKeyPath_options_context(
                &observer,
                ns_string!("contentsScale"),
                NSKeyValueObservingOptions::New | NSKeyValueObservingOptions::Initial,
                ptr::null_mut(),
            );
            root_layer.addObserver_forKeyPath_options_context(
                &observer,
                ns_string!("bounds"),
                NSKeyValueObservingOptions::New | NSKeyValueObservingOptions::Initial,
                ptr::null_mut(),
            );
        }

        // Set the content so that it is placed in the top-left corner if it does not have the same
        // size as the surface itself.
        //
        // TODO(madsmtm): Consider changing this to `kCAGravityResize` to stretch the content if
        // resized to something that doesn't fit, see #177.
        layer.setContentsGravity(unsafe { kCAGravityTopLeft });

        // Grab initial width and height from the layer (whose properties have just been initialized
        // by the observer using `NSKeyValueObservingOptionInitial`).
        let size = layer.bounds().size;
        let scale_factor = layer.contentsScale();
        let width = (size.width * scale_factor) as usize;
        let height = (size.height * scale_factor) as usize;

        Ok(Self {
            layer: SendCALayer(layer),
            root_layer: SendCALayer(root_layer),
            observer,
            pixels: vec![0; width * height],
            age: 0,
            surfaces: Vec::new(),
            width,
            height,
            _display: PhantomData,
            window_handle: window_src,
        })
    }

    #[inline]
    fn window(&self) -> &W {
        &self.window_handle
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), SoftBufferError> {
        let (width, height) = (width.get() as usize, height.get() as usize);
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.pixels = vec![0; width * height];
            self.age = 0;
            self.surfaces.clear();
        }
        Ok(())
    }

    fn buffer_mut(&mut self) -> Result<BufferImpl<'_, D, W>, SoftBufferError> {
        Ok(BufferImpl { imp: self })
    }
}

#[derive(Debug)]
pub struct BufferImpl<'a, D, W> {
    imp: &'a mut CGImpl<D, W>,
}

impl<D: HasDisplayHandle, W: HasWindowHandle> BufferInterface for BufferImpl<'_, D, W> {
    fn width(&self) -> NonZeroU32 {
        NonZeroU32::new(self.imp.width as u32).unwrap()
    }

    fn height(&self) -> NonZeroU32 {
        NonZeroU32::new(self.imp.height as u32).unwrap()
    }

    #[inline]
    fn pixels(&self) -> &[u32] {
        &self.imp.pixels
    }

    #[inline]
    fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.imp.pixels
    }

    fn age(&self) -> u8 {
        self.imp.age
    }

    fn present(self) -> Result<(), SoftBufferError> {
        let all = Rect {
            x: 0,
            y: 0,
            width: self.width(),
            height: self.height(),
        };
        self.present_with_damage(&[all])
    }

    fn present_with_damage(self, damage: &[Rect]) -> Result<(), SoftBufferError> {
        let imp = self.imp;
        let (width, height) = (imp.width, imp.height);
        let changed: Vec<Rect> = damage
            .iter()
            .filter_map(|r| clip(r, width, height))
            .collect();

        // Every surface now lacks these changes.
        for surface in &mut imp.surfaces {
            if let Some(stale) = &mut surface.stale {
                stale.extend_from_slice(&changed);
                if stale.len() > MAX_STALE_RECTS {
                    surface.stale = None;
                }
            }
        }

        // The oldest surface the window server is done with. The one shown last is never
        // written to: it may not have reached the screen yet.
        let shown_last = imp.surfaces.len().saturating_sub(1);
        let free = imp.surfaces[..shown_last]
            .iter()
            .position(|s| !s.surface.is_in_use());
        let index = match free {
            Some(i) => i,
            None if imp.surfaces.len() < MAX_SURFACES => {
                imp.surfaces.push(Surface {
                    surface: SendSurface(new_surface(width, height)?),
                    stale: None,
                });
                imp.surfaces.len() - 1
            }
            // All held. The window server usually lets the oldest go within a frame: wait a
            // little for it before drawing into it anyway, which could show a torn frame.
            None => {
                let until = std::time::Instant::now() + std::time::Duration::from_millis(4);
                while imp.surfaces[0].surface.is_in_use() && std::time::Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_micros(250));
                }
                0
            }
        };
        let mut surface = imp.surfaces.remove(index);
        fill(
            &surface.surface,
            &imp.pixels,
            width,
            height,
            surface.stale.as_deref(),
        )?;
        surface.stale = Some(Vec::new());

        // The CALayer has a default action associated with a change in the layer contents, causing
        // a quarter second fade transition to happen every time a new buffer is applied. This can
        // be avoided by wrapping the operation in a transaction and disabling all actions.
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let contents: &AnyObject = surface.surface.0.as_ref();
        // SAFETY: An IOSurface is a valid class for `contents`.
        unsafe { imp.layer.setContents(Some(contents)) };
        CATransaction::commit();

        imp.surfaces.push(surface);
        imp.age = 1;
        Ok(())
    }
}

/// `rect` within a `width` × `height` picture, or `None` when nothing of it is inside.
fn clip(rect: &Rect, width: usize, height: usize) -> Option<Rect> {
    let x = (rect.x as usize).min(width);
    let y = (rect.y as usize).min(height);
    let right = (rect.x as usize + rect.width.get() as usize).min(width);
    let bottom = (rect.y as usize + rect.height.get() as usize).min(height);
    Some(Rect {
        x: x as u32,
        y: y as u32,
        width: NonZeroU32::new((right - x) as u32)?,
        height: NonZeroU32::new((bottom - y) as u32)?,
    })
}

/// A surface for a `width` × `height` picture, in sRGB like the colors renderers draw with: the
/// window server converts it to the display's colors on the graphics card.
fn new_surface(width: usize, height: usize) -> Result<CFRetained<IOSurfaceRef>, SoftBufferError> {
    let number = |n: usize| CFNumber::new_i64(n as i64);
    let (width_value, height_value, bytes) = (number(width), number(height), number(4));
    let format = CFNumber::new_i32(PIXEL_FORMAT_BGRA);
    // SAFETY: The keys are constants IOSurface exports.
    let keys: [&CFString; 4] = unsafe {
        [
            kIOSurfaceWidth,
            kIOSurfaceHeight,
            kIOSurfaceBytesPerElement,
            kIOSurfacePixelFormat,
        ]
    };
    let values: [&CFNumber; 4] = [&width_value, &height_value, &bytes, &format];
    let properties = CFDictionary::from_slices(&keys, &values);
    // SAFETY: The dictionary holds the documented property types.
    let surface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }.ok_or_else(|| {
        SoftBufferError::PlatformError(Some("could not create an IOSurface".into()), None)
    })?;
    // SAFETY: The color space's name is a CFString, as `kIOSurfaceColorSpace` takes.
    unsafe {
        let srgb: &CFType = kCGColorSpaceSRGB.as_ref();
        surface.set_value(kIOSurfaceColorSpace, srgb);
    }
    Ok(surface)
}

/// Copies `pixels` into `surface`: only the `stale` rectangles, or all of it when `None`.
/// Pixels get an opaque alpha, as the surface's format has one.
fn fill(
    surface: &IOSurfaceRef,
    pixels: &[u32],
    width: usize,
    height: usize,
    stale: Option<&[Rect]>,
) -> Result<(), SoftBufferError> {
    // SAFETY: No seed is asked for.
    let locked = unsafe { surface.lock(IOSurfaceLockOptions::empty(), ptr::null_mut()) };
    if locked != 0 {
        return Err(SoftBufferError::PlatformError(
            Some(format!("could not lock an IOSurface: {locked}")),
            None,
        ));
    }
    let base = surface.base_address().as_ptr().cast::<u8>();
    let stride = surface.bytes_per_row();
    let rows = surface.height().min(height);
    let columns = (stride / 4).min(width);
    let copy = |x: usize, y: usize, w: usize, h: usize| {
        let w = w.min(columns.saturating_sub(x));
        for row in y..(y + h).min(rows) {
            let source = &pixels[row * width + x..row * width + x + w];
            // SAFETY: The surface is locked, and the row lies within it: `row < rows` and
            // `x + w <= stride / 4`. Its memory is page-aligned, so aligned for `u32`.
            let target = unsafe {
                std::slice::from_raw_parts_mut(base.add(row * stride).cast::<u32>().add(x), w)
            };
            for (t, s) in target.iter_mut().zip(source) {
                *t = s | 0xff00_0000;
            }
        }
    };
    match stale {
        None => copy(0, 0, width, height),
        Some(rects) => {
            for r in rects {
                copy(
                    r.x as usize,
                    r.y as usize,
                    r.width.get() as usize,
                    r.height.get() as usize,
                );
            }
        }
    }
    // SAFETY: Locked above with the same options; no seed is asked for.
    unsafe { surface.unlock(IOSurfaceLockOptions::empty(), ptr::null_mut()) };
    Ok(())
}

/// An IOSurface, which may be used from any thread.
#[derive(Debug)]
struct SendSurface(CFRetained<IOSurfaceRef>);

// SAFETY: IOSurfaces are made to be shared, even between processes; their memory is only
// written while locked.
unsafe impl Send for SendSurface {}
// SAFETY: Same as above.
unsafe impl Sync for SendSurface {}

impl Deref for SendSurface {
    type Target = IOSurfaceRef;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug)]
struct SendCALayer(Retained<CALayer>);

// SAFETY: CALayer is dubiously thread safe, like most things in Core Animation.
// But since we make sure to do our changes within a CATransaction, it is
// _probably_ fine for us to use CALayer from different threads.
//
// See also:
// https://developer.apple.com/documentation/quartzcore/catransaction/1448267-lock?language=objc
// https://stackoverflow.com/questions/76250226/how-to-render-content-of-calayer-on-a-background-thread
unsafe impl Send for SendCALayer {}
// SAFETY: Same as above.
unsafe impl Sync for SendCALayer {}

impl Deref for SendCALayer {
    type Target = CALayer;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
