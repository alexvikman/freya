//! The server's icon in the notification area on Windows, with its menu on a click. Its window
//! is never shown, but it is what Windows closes the server through: at logout, and when an
//! installer needs its files, as it does any program with a window.

#![allow(
    unsafe_code,
    reason = "the notification area, menus and a window's messages are the system's own calls, \
              made on the main thread for the window this owns"
)]

use std::cell::RefCell;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DispatchMessageW, GetCursorPos, GetMessageW, HICON, IDI_APPLICATION, LoadIconW, MF_CHECKED,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, RegisterClassW, RegisterWindowMessageW,
    SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP,
    WM_CLOSE, WM_ENDSESSION, WM_LBUTTONUP, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP, WNDCLASSW,
    WS_OVERLAPPEDWINDOW,
};

use super::Actions;

/// What the icon sends its window.
const ICON_MESSAGE: u32 = WM_APP + 1;
/// The menu's items.
const OPEN_FREYA: usize = 1;
const SHOW_CODE: usize = 2;
const LOGIN: usize = 3;
const UPDATE: usize = 4;
const SHOW_LOG: usize = 5;
const STOP: usize = 6;

struct Icon {
    actions: Actions,
    window: HWND,
    /// The connection code shown last, for the menu to show until it runs out.
    code: Option<String>,
    /// What Explorer sends when it starts again, and the icon is to be added again.
    taskbar_created: u32,
}

thread_local! {
    static ICON: RefCell<Option<Icon>> = const { RefCell::new(None) };
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `text` into a fixed field of the icon's data, cut to fit with its NUL.
fn fill(field: &mut [u16], text: &str) {
    let text: Vec<u16> = text.encode_utf16().take(field.len() - 1).collect();
    field[..text.len()].copy_from_slice(&text);
    field[text.len()] = 0;
}

/// Shows the icon and runs the window's messages on this thread for as long as the server
/// runs.
pub fn run(actions: Actions) -> Option<()> {
    let class = wide("FreyaServer");
    // SAFETY: the running program's module, a class with a procedure that lives as long as it,
    // and a window of it never shown.
    let window = unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let registered = RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        });
        if registered == 0 {
            return None;
        }
        let title = wide("Freya Server");
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated name.
    let taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    ICON.with_borrow_mut(|icon| {
        *icon = Some(Icon {
            actions,
            window,
            code: None,
            taskbar_created,
        });
    });
    add_icon(window);
    // SAFETY: the thread's own message loop, until the process ends.
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Some(())
}

/// The icon's data, for `window`.
fn icon_data(window: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        ..Default::default()
    };
    fill(&mut data.szTip, "Freya Server");
    data
}

fn add_icon(window: HWND) {
    let mut data = icon_data(window);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = ICON_MESSAGE;
    data.hIcon = app_icon();
    // SAFETY: the icon's data, filled in, for the window this owns.
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
}

/// Freya's icon, built into the program (see build.rs), or Windows' own for a program.
fn app_icon() -> HICON {
    // SAFETY: the icon resource 1 of the running program, or a stock icon.
    unsafe {
        let icon = LoadIconW(GetModuleHandleW(std::ptr::null()), 1 as _);
        if icon.is_null() {
            LoadIconW(std::ptr::null_mut(), IDI_APPLICATION)
        } else {
            icon
        }
    }
}

/// Shows `text` beside the icon for a while, as Windows shows notifications.
fn notify(window: HWND, title: &str, text: &str) {
    let mut data = icon_data(window);
    data.uFlags = NIF_INFO;
    data.dwInfoFlags = NIIF_INFO;
    fill(&mut data.szInfoTitle, title);
    fill(&mut data.szInfo, text);
    // SAFETY: the icon's data, filled in, for the window this owns.
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

/// Freya, where its installer puts it, for "Open Freya".
fn freya() -> Option<PathBuf> {
    [
        ("ProgramFiles", r"Freya\freya.exe"),
        ("LOCALAPPDATA", r"Programs\Freya\freya.exe"),
    ]
    .into_iter()
    .filter_map(|(folder, program)| Some(PathBuf::from(std::env::var_os(folder)?).join(program)))
    .find(|program| program.is_file())
}

/// Shows the menu at the pointer, and does what is chosen.
fn show_menu(window: HWND) {
    // What the menu shows, read before it opens: the menu runs the window's messages itself.
    let Some((starts, offered, code)) = ICON.with_borrow(|icon| {
        icon.as_ref().map(|icon| {
            (
                icon.actions.starts_at_login(),
                icon.actions.updates.offered(),
                icon.code.clone(),
            )
        })
    }) else {
        return;
    };
    let freya = freya();
    // SAFETY: a menu this makes, shown for the window this owns, and destroyed after.
    let chosen = unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let add = |flags: u32, id: usize, text: &str| {
            AppendMenuW(menu, flags, id, wide(text).as_ptr());
        };
        add(MF_STRING | MF_GRAYED, 0, "Freya Server is running");
        add(
            MF_STRING | MF_GRAYED,
            0,
            concat!("Version ", env!("CARGO_PKG_VERSION")),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        if freya.is_some() {
            add(MF_STRING, OPEN_FREYA, "Open Freya");
        }
        add(MF_STRING, SHOW_CODE, "Show Connection Code");
        if let Some(code) = &code {
            add(MF_STRING | MF_GRAYED, 0, code);
        }
        add(
            MF_STRING | if starts { MF_CHECKED } else { 0 },
            LOGIN,
            "Start at Login",
        );
        if let Some(offered) = &offered {
            add(MF_STRING, UPDATE, offered);
        }
        add(MF_STRING, SHOW_LOG, "Show Log");
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        add(MF_STRING, STOP, "Stop Server");
        let mut at = POINT { x: 0, y: 0 };
        GetCursorPos(&mut at);
        // So the menu closes when the user clicks elsewhere.
        SetForegroundWindow(window);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            0,
            window,
            std::ptr::null(),
        );
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
        chosen as usize
    };
    if chosen == OPEN_FREYA {
        if let Some(freya) = freya
            && let Err(e) = std::process::Command::new(freya).spawn()
        {
            log::warn!("could not open Freya: {e}");
        }
        return;
    }
    ICON.with_borrow_mut(|icon| {
        let Some(icon) = icon else {
            return;
        };
        match chosen {
            SHOW_CODE => {
                let code = icon.actions.connection_code();
                notify(icon.window, "Connection code", &code);
                icon.code = Some(code);
            }
            LOGIN => icon.actions.toggle_start_at_login(),
            UPDATE => icon.actions.update(),
            SHOW_LOG => icon.actions.show_log(),
            STOP => icon.actions.stop(),
            _ => {}
        }
    });
}

/// Stops the server, as Windows asks a program with a window to end: the icon goes too.
fn end() {
    ICON.with_borrow(|icon| {
        if let Some(icon) = icon {
            let data = icon_data(icon.window);
            // SAFETY: the icon's data, for the window this owns.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
            icon.actions.stop();
        }
    });
}

/// The window's procedure: the icon's clicks, Explorer starting again, and Windows asking it
/// to end.
unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        ICON_MESSAGE => {
            if matches!(lparam as u32, WM_LBUTTONUP | WM_RBUTTONUP) {
                show_menu(window);
            }
            0
        }
        // Ending is fine at any time: what the server keeps is written as it changes.
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                end();
            }
            0
        }
        WM_CLOSE => {
            end();
            0
        }
        _ => {
            let restarted = ICON
                .try_with(|icon| {
                    icon.try_borrow().is_ok_and(|icon| {
                        icon.as_ref().is_some_and(|i| i.taskbar_created == message)
                    })
                })
                .unwrap_or(false);
            if restarted && message != 0 {
                add_icon(window);
                return 0;
            }
            // SAFETY: passed on as the system gave it.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}
