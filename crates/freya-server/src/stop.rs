//! `freya-server --stop`, for Windows' installer: stops this user's running server before its
//! files are replaced or removed. It is asked to end through the window of its icon, as
//! logging out does, and given a while; one that has not ended by then, or has no window yet,
//! is ended at once. What the server keeps is written as it changes, so nothing is lost.

#![allow(
    unsafe_code,
    reason = "finding, waiting for and ending processes are the system's own calls, on handles \
              this closes"
)]

use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};

/// How long a server asked to end is given, before it is ended at once.
const ENDING: Duration = Duration::from_secs(10);

/// The program's name, as the system lists processes.
const PROGRAM: &str = "freya-server.exe";

/// Stops every server of this user: asked first, then ended. Returns how many there were.
pub fn stop() -> usize {
    let mut stopped = 0;
    // The one with the icon's window, asked to end and waited for.
    let class: Vec<u16> = "FreyaServer".encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated class name; the window is only posted to.
    let window = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
    if !window.is_null() {
        let mut id = 0;
        // SAFETY: the window found, and where its process's id goes.
        unsafe { GetWindowThreadProcessId(window, &mut id) };
        if let Some(process) = open(id) {
            // SAFETY: the window found, and the process opened, closed below.
            unsafe {
                PostMessageW(window, WM_CLOSE, 0, 0);
                if WaitForSingleObject(process.0, ENDING.as_millis() as u32) != WAIT_OBJECT_0 {
                    TerminateProcess(process.0, 1);
                }
            }
            stopped += 1;
        }
    }
    // Any other, as one without its window yet: ended at once.
    // SAFETY: this process's own id.
    let this = unsafe { GetCurrentProcessId() };
    for id in servers().into_iter().filter(|&id| id != this) {
        if let Some(process) = open(id) {
            // SAFETY: the process opened, closed when dropped.
            unsafe { TerminateProcess(process.0, 1) };
            stopped += 1;
        }
    }
    stopped
}

/// A process handle, closed when dropped.
struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: a handle this opened.
        unsafe { CloseHandle(self.0) };
    }
}

/// The process `id`, to wait for and end; none when it is not this user's, or gone.
fn open(id: u32) -> Option<Process> {
    // SAFETY: a process id; a null handle when it cannot be opened.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, id) };
    (!handle.is_null()).then_some(Process(handle))
}

/// The ids of the processes running the server's program.
fn servers() -> Vec<u32> {
    let mut found = Vec::new();
    // SAFETY: a snapshot of the processes, read entry by entry and closed when dropped.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return found;
        }
        let snapshot = Process(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot.0, &mut entry) != 0;
        while more {
            let length = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
            if name.eq_ignore_ascii_case(PROGRAM) {
                found.push(entry.th32ProcessID);
            }
            more = Process32NextW(snapshot.0, &mut entry) != 0;
        }
    }
    found
}
