//! A skin's window: the built-in skin compiled into Freya, or any other skin read by Slint's
//! interpreter. The app talks to both the same way, by global and property name.

use std::rc::Rc;

use freya_skin::SkinSource;
use slint_interpreter::{ComponentHandle, ComponentInstance, Value};

use super::builtin_skin::Builtin;

pub enum SkinWindow {
    Builtin(Builtin),
    Loaded(ComponentInstance),
}

/// Another handle to the same window.
impl Clone for SkinWindow {
    fn clone(&self) -> Self {
        match self {
            Self::Builtin(ui) => Self::Builtin(ui.clone()),
            Self::Loaded(ui) => Self::Loaded(ui.clone_strong()),
        }
    }
}

/// A handle to a skin's window that does not keep it open, as for the window's own callbacks.
pub enum WeakSkinWindow {
    Builtin(slint::Weak<super::builtin_skin::FreyaSkin>),
    Loaded(slint::Weak<ComponentInstance>),
}

impl WeakSkinWindow {
    pub fn upgrade(&self) -> Option<SkinWindow> {
        match self {
            Self::Builtin(ui) => Builtin::upgrade(ui).map(SkinWindow::Builtin),
            Self::Loaded(ui) => ui.upgrade().map(SkinWindow::Loaded),
        }
    }
}

impl SkinWindow {
    pub fn downgrade(&self) -> WeakSkinWindow {
        match self {
            Self::Builtin(ui) => WeakSkinWindow::Builtin(ui.as_weak()),
            Self::Loaded(ui) => WeakSkinWindow::Loaded(ui.as_weak()),
        }
    }

    pub fn load(source: &SkinSource) -> Result<Self, freya_skin::Error> {
        match source {
            SkinSource::Builtin => Ok(Self::Builtin(Builtin::new()?)),
            other => Ok(Self::Loaded(freya_skin::load(other)?)),
        }
    }

    pub fn window(&self) -> &slint::Window {
        match self {
            Self::Builtin(ui) => ui.window(),
            Self::Loaded(ui) => ui.window(),
        }
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        match self {
            Self::Builtin(ui) => ui.show(),
            Self::Loaded(ui) => ui.show(),
        }
    }

    pub fn hide(&self) -> Result<(), slint::PlatformError> {
        match self {
            Self::Builtin(ui) => ui.hide(),
            Self::Loaded(ui) => ui.hide(),
        }
    }

    /// Whether the skin exports `global`; skins may leave out all but `Player`.
    pub fn has_global(&self, global: &str) -> bool {
        match self {
            Self::Builtin(_) => Builtin::globals().any(|g| g == global),
            Self::Loaded(ui) => ui.definition().globals().any(|g| g == global),
        }
    }

    pub fn set_global_property(
        &self,
        global: &str,
        name: &str,
        value: Value,
    ) -> Result<(), String> {
        match self {
            Self::Builtin(ui) => ui.set_global_property(global, name, value),
            Self::Loaded(ui) => ui
                .set_global_property(global, name, value)
                .map_err(|e| e.to_string()),
        }
    }

    pub fn get_global_property(&self, global: &str, name: &str) -> Result<Value, String> {
        match self {
            Self::Builtin(ui) => ui.get_global_property(global, name),
            Self::Loaded(ui) => ui
                .get_global_property(global, name)
                .map_err(|e| e.to_string()),
        }
    }

    pub fn set_global_callback(
        &self,
        global: &str,
        name: &str,
        handler: impl Fn(&[Value]) -> Value + 'static,
    ) -> Result<(), String> {
        match self {
            Self::Builtin(ui) => ui.set_global_callback(global, name, Rc::new(handler)),
            Self::Loaded(ui) => ui
                .set_global_callback(global, name, handler)
                .map_err(|e| e.to_string()),
        }
    }

    /// Calls a skin callback, as the skin itself would; for tests.
    #[cfg(test)]
    pub fn invoke_global(&self, global: &str, name: &str, args: &[Value]) -> Result<Value, String> {
        match self {
            Self::Builtin(ui) => ui.invoke_global(global, name, args),
            Self::Loaded(ui) => ui
                .invoke_global(global, name, args)
                .map_err(|e| e.to_string()),
        }
    }
}
