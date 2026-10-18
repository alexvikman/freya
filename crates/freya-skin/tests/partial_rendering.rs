//! Slint's software renderer, as Freya patches it (vendor/README.md), redrawing only what
//! changed: an icon under something that is redrawn must not be drawn over what lies on top
//! of it and was not.

use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, PlatformError, Rgb8Pixel};
use slint_interpreter::{Compiler, ComponentHandle};

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(Rc::clone(&self.0) as Rc<dyn WindowAdapter>)
    }
}

/// A red cross, a path, under a blue menu whose rows light up under the pointer.
const SCENE: &str = r#"
component Row inherits Rectangle {
    height: 30px;
    background: touch.has-hover ? #ddd : transparent;
    touch := TouchArea { }
}
export component Scene inherits Window {
    width: 400px;
    height: 400px;
    background: white;
    Path {
        x: 170px; y: 170px; width: 60px; height: 60px;
        viewbox-width: 24; viewbox-height: 24;
        commands: "M 2 2 L 22 22 M 22 2 L 2 22";
        stroke: red; stroke-width: 8px;
    }
    menu := PopupWindow {
        x: 140px; y: 140px; width: 120px;
        Rectangle {
            background: blue;
            VerticalLayout { Row { } Row { } Row { } }
        }
    }
    public function open() { menu.show(); }
}
"#;

#[test]
fn a_path_is_not_drawn_over_a_menu_beyond_what_is_redrawn() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless(Rc::clone(&window)))).unwrap();
    let result =
        spin_on::spin_on(Compiler::default().build_from_source(SCENE.into(), "scene.slint".into()));
    let ui = result.component("Scene").unwrap().create().unwrap();
    ui.show().unwrap();
    window.set_size(PhysicalSize::new(400, 400));
    let mut pixels = vec![Rgb8Pixel::default(); 400 * 400];
    let mut draw = || {
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 400);
        });
        // Red where the menu, 140 to 260 by 140 to 230, should be blue throughout.
        (140..230)
            .flat_map(|y| (140..260).map(move |x| y * 400 + x))
            .filter(|&i| pixels[i].r > 200 && pixels[i].b < 100)
            .count()
    };
    draw();
    ui.invoke("open", &[]).unwrap();
    assert_eq!(draw(), 0, "the menu covers the cross");
    // Lighting up a row redraws it and the cross under it, which was drawn whole, over the
    // rows that were not redrawn.
    for y in [150.0, 175.0, 205.0, 300.0] {
        window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(200.0, y),
        });
        assert_eq!(draw(), 0, "pointer at y {y}");
    }
}
