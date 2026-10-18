//! `freya-cli`: Freya for a terminal on Windows, where Freya itself is a window app. A
//! terminal does not wait for a window app, so it would take its prompt back at once, read the
//! keys meant for `freya play`, and lose Freya's exit code. This small console program starts
//! `freya.exe` next to it with the same arguments, in the same terminal, waits for it, and
//! exits with its code. Elsewhere `freya` is a terminal program already, and this only passes
//! the arguments on.

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let name = if cfg!(windows) { "freya.exe" } else { "freya" };
    let freya = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name(name),
        Err(e) => {
            eprintln!("freya-cli: cannot find {name}: {e}");
            return ExitCode::FAILURE;
        }
    };
    ignore_ctrl_c();
    match Command::new(&freya)
        .args(std::env::args_os().skip(1))
        .status()
    {
        // Windows' exit codes are 32-bit; one that does not fit is a failure all the same.
        Ok(status) => match status.code() {
            Some(0) => ExitCode::SUCCESS,
            Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
            None => ExitCode::FAILURE,
        },
        Err(e) => {
            eprintln!("freya-cli: cannot start {}: {e}", freya.display());
            ExitCode::FAILURE
        }
    }
}

/// Ctrl+C reaches every program in the terminal. Freya decides what it means; this one waits
/// for Freya to finish rather than leaving it running behind the prompt.
#[cfg(windows)]
#[allow(unsafe_code, reason = "a plain Windows API call without pointers")]
fn ignore_ctrl_c() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: no handler, only the flag that the process ignores Ctrl+C.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
    }
}

#[cfg(not(windows))]
fn ignore_ctrl_c() {}
