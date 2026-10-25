//! Switching skins: the skin picker window, installing packages and replacing the window.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use freya_skin::{PACKAGE_EXTENSION, SkinInfo, SkinSource};
use slint::{ModelRc, VecModel};
use slint_interpreter::{ComponentHandle, Struct, Value};

use super::app::App;
use super::bindings::{self, connect};
use super::skin_state::string;
use super::skin_window::SkinWindow;
use crate::settings;

/// The system asking a skin window to close is closing it by the system (see
/// `super::app::CloseBy`), which the app carries out; without the app, Freya quits, even if
/// the About or skin picker window is open.
pub fn attach_window(ui: &SkinWindow, app: Weak<RefCell<App>>) {
    ui.window().on_close_requested(move || {
        match app.upgrade().as_ref().map(|app| app.try_borrow_mut()) {
            Some(Ok(mut app)) => app.close_window(super::app::CloseBy::System),
            _ => {
                let _ = slint::quit_event_loop();
            }
        }
        // Whatever the window is to do, closing it did it.
        slint::CloseRequestResponse::KeepWindowShown
    });
}

/// Loads `source` and puts it in place of the current skin. On failure the current skin
/// stays, and the error is shown in the skin picker.
pub fn switch_skin(app: &Rc<RefCell<App>>, source: SkinSource) {
    let ui = match SkinWindow::load(&source) {
        Ok(ui) => ui,
        Err(e) => {
            log::warn!("could not load skin: {e}");
            app.borrow_mut().skin_error = format!("Could not use this skin: {e}");
            refresh_picker(app);
            return;
        }
    };
    let (size, position, same_skin) = {
        let app = app.borrow();
        let window = app.view.ui.window();
        (window.size(), window.position(), app.skin == source)
    };
    connect(&ui, app);
    attach_window(&ui, Rc::downgrade(app));
    ui.window().set_position(position);
    // A reloaded skin keeps its size; another skin starts at its own preferred size.
    if same_skin {
        ui.window().set_size(size);
    }
    let old = app.borrow_mut().replace_view(ui, source);
    let _ = app.borrow().view.ui.show();
    let _ = old.ui.hide();
    // Windows ties the media controls to the window, so they move to the new one with the
    // next tick. The old ones close first: on Linux both would want the same name on the bus.
    if app.borrow_mut().system_media.take().is_some() {
        app.borrow_mut().media_tries = super::MEDIA_TRIES;
    }
    refresh_picker(app);
}

/// Opens Freya's skin picker.
pub fn show_picker(app: &Rc<RefCell<App>>) {
    if app.borrow().picker.is_none() {
        let picker = match freya_skin::skin_picker_window() {
            Ok(picker) => picker,
            Err(e) => {
                log::warn!("could not open the skin picker: {e}");
                return;
            }
        };
        let weak = Rc::downgrade(app);
        let on = |name: &str, action: fn(&Rc<RefCell<App>>, &[Value])| {
            let weak: Weak<RefCell<App>> = weak.clone();
            let _ = picker.set_callback(name, move |args| {
                if let Some(app) = weak.upgrade() {
                    action(&app, args);
                }
                Value::Void
            });
        };
        on("select", |app, args| {
            let chosen = app
                .borrow()
                .picker_skins
                .get(bindings::index(args, 0))
                .map(|s| s.source.clone());
            if let Some(source) = chosen {
                switch_skin(app, source);
            }
        });
        on("install", |app, _| install(app));
        on("open-folder", |_, _| {
            if let Some(dir) = settings::skins_dir() {
                open_folder(&dir);
            }
        });
        app.borrow_mut().picker = Some(picker);
    }
    refresh_picker(app);
    if let Some(picker) = &app.borrow().picker {
        let _ = picker.show();
    }
}

/// Fills the picker with the installed skins and the current error.
fn refresh_picker(app: &Rc<RefCell<App>>) {
    let mut app = app.borrow_mut();
    if app.picker.is_none() {
        return;
    }
    let dir = settings::skins_dir();
    let mut skins = dir
        .as_deref()
        .map(freya_skin::discover)
        .unwrap_or_else(|| vec![SkinInfo::builtin()]);
    // A skin used from elsewhere (e.g. with --skin) is listed too.
    if !skins.iter().any(|s| s.source == app.skin)
        && let Ok(info) = SkinInfo::read(&app.skin)
    {
        skins.push(info);
    }
    let entries: Vec<Value> = skins
        .iter()
        .map(|skin| {
            Value::Struct(Struct::from_iter([
                ("name".to_owned(), string(&skin.name)),
                ("details".to_owned(), string(details(skin))),
                ("current".to_owned(), Value::Bool(skin.source == app.skin)),
            ]))
        })
        .collect();
    let folder = dir.map(|d| d.display().to_string()).unwrap_or_default();
    let error = app.skin_error.clone();
    if let Some(picker) = &app.picker {
        let _ = picker.set_property("skins", Value::Model(ModelRc::new(VecModel::from(entries))));
        let _ = picker.set_property("folder", string(folder));
        let _ = picker.set_property("error", string(error));
    }
    app.picker_skins = skins;
}

/// "by Someone · 1.2 · A dark skin", leaving out what the skin does not say.
fn details(skin: &SkinInfo) -> String {
    let mut parts = Vec::new();
    if !skin.author.is_empty() {
        parts.push(format!("by {}", skin.author));
    }
    if !skin.version.is_empty() {
        parts.push(skin.version.clone());
    }
    if !skin.description.is_empty() {
        parts.push(skin.description.clone());
    }
    parts.join(" · ")
}

/// Copies a skin package into the skins folder and switches to it.
fn install(app: &Rc<RefCell<App>>) {
    let Some(picked) = rfd::FileDialog::new()
        .set_title("Install skin")
        .add_filter("Freya skins", &[PACKAGE_EXTENSION])
        .pick_file()
    else {
        return;
    };
    let Some(dir) = settings::skins_dir() else {
        return;
    };
    match copy_into(&picked, &dir) {
        Ok(installed) => switch_skin(app, SkinSource::from_path(&installed)),
        Err(e) => {
            app.borrow_mut().skin_error = format!("Could not install the skin: {e}");
            refresh_picker(app);
        }
    }
}

fn copy_into(file: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = file
        .file_name()
        .ok_or_else(|| std::io::Error::other("not a file"))?;
    let target = dir.join(name);
    // Picked from the skins folder itself: installed already.
    if let (Ok(a), Ok(b)) = (file.canonicalize(), target.canonicalize())
        && a == b
    {
        return Ok(target);
    }
    // Through a new file renamed over the old: copying straight onto a file that is the one
    // copied (a link to it, say) empties it, and a failed copy would leave half a package.
    let partial = target.with_extension("partial");
    std::fs::copy(file, &partial)?;
    std::fs::rename(&partial, &target)?;
    Ok(target)
}

fn open_folder(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!("could not create {}: {e}", dir.display());
        return;
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(opener).arg(dir).spawn() {
        log::warn!("could not open {}: {e}", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::copy_into;

    #[test]
    fn installing_a_package_from_the_skins_folder_keeps_it() {
        // It was copied onto itself, which left it empty.
        let dir = std::env::temp_dir().join(format!("freya-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let package = dir.join("Night.freyaskin");
        std::fs::write(&package, b"package bytes").unwrap();
        let installed = copy_into(&package, &dir).unwrap();
        assert_eq!(std::fs::read(&installed).unwrap(), b"package bytes");

        // From elsewhere, it replaces the installed one.
        let elsewhere = dir.join("download");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("Night.freyaskin"), b"newer").unwrap();
        copy_into(&elsewhere.join("Night.freyaskin"), &dir).unwrap();
        assert_eq!(std::fs::read(&package).unwrap(), b"newer");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
