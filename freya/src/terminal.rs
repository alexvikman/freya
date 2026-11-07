//! The terminal, for the command-line player (see cli_player): keys read as they are pressed,
//! without echo, and the line the status is drawn on cleared before each drawing. The
//! terminal's own settings are put back when the player ends, on an error too.
//!
//! On Unix this is the terminal's raw mode and its escape sequences; on Windows the console's
//! input records, with its virtual terminal processing turned on for the clearing sequence.

use std::io;
use std::time::Duration;

/// A key pressed in the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Escape,
    /// Ctrl+C, which the terminal hands over as a key rather than ending the program.
    CtrlC,
    /// A key the player has no use for: a function key, a modifier on its own.
    Other,
}

/// Clears the line the cursor is on and puts the cursor at its start: written before a line.
pub const CLEAR_LINE: &str = "\r\x1b[2K";

/// The terminal as the player wants it: keys arrive as they are pressed, nothing is echoed,
/// and Ctrl+C is a key. Its settings before are put back when this is dropped.
pub struct RawMode(platform::Saved);

impl RawMode {
    pub fn enable() -> io::Result<Self> {
        platform::enable().map(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        platform::restore(&self.0);
    }
}

/// Waits up to `timeout` for a key; None when none was pressed in that time.
pub fn read_key(timeout: Duration) -> io::Result<Option<Key>> {
    platform::read_key(timeout)
}

/// The wait in milliseconds the system takes, `timeout` rounded up and capped.
fn millis(timeout: Duration) -> u32 {
    u32::try_from(timeout.as_millis())
        .unwrap_or(u32::MAX)
        .max(1)
}

#[cfg(unix)]
mod platform {
    use std::io;
    use std::time::Duration;

    use super::Key;

    const STDIN: libc::c_int = libc::STDIN_FILENO;

    /// The terminal's settings before raw mode.
    pub struct Saved(libc::termios);

    #[allow(unsafe_code, reason = "reads and sets the terminal's settings")]
    pub fn enable() -> io::Result<Saved> {
        // SAFETY: a zeroed termios is a valid value for the system to fill in.
        let mut before: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(STDIN, &mut before) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = before;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(STDIN, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Saved(before))
    }

    #[allow(unsafe_code, reason = "puts the terminal's settings back")]
    pub fn restore(saved: &Saved) {
        unsafe { libc::tcsetattr(STDIN, libc::TCSANOW, &saved.0) };
    }

    pub fn read_key(timeout: Duration) -> io::Result<Option<Key>> {
        if !wait(super::millis(timeout))? {
            return Ok(None);
        }
        let Some(first) = read_byte()? else {
            return Ok(None);
        };
        let key = match first {
            0x03 => Key::CtrlC,
            // An escape, alone or opening a sequence; the sequence's bytes follow at once.
            0x1b => match wait(1)?.then(read_byte).transpose()?.flatten() {
                None => Key::Escape,
                Some(b'[' | b'O') => match read_byte()? {
                    Some(b'A') => Key::Up,
                    Some(b'B') => Key::Down,
                    Some(b'C') => Key::Right,
                    Some(b'D') => Key::Left,
                    _ => Key::Other,
                },
                Some(_) => Key::Other,
            },
            byte if byte.is_ascii() => Key::Char(char::from(byte)),
            // The first byte of a character in UTF-8 says how many follow it.
            byte => {
                let mut bytes = vec![byte];
                for _ in 1..byte.leading_ones() {
                    match read_byte()? {
                        Some(next) => bytes.push(next),
                        None => break,
                    }
                }
                str::from_utf8(&bytes)
                    .ok()
                    .and_then(|s| s.chars().next())
                    .map_or(Key::Other, Key::Char)
            }
        };
        Ok(Some(key))
    }

    /// Whether a byte can be read from the terminal within `millis`.
    #[allow(unsafe_code, reason = "waits for the terminal to have input")]
    fn wait(millis: u32) -> io::Result<bool> {
        let mut fds = libc::pollfd {
            fd: STDIN,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `fds` is one pollfd, as the count says.
        let ready = unsafe { libc::poll(&mut fds, 1, i32::try_from(millis).unwrap_or(i32::MAX)) };
        match ready {
            0 => Ok(false),
            n if n > 0 => Ok(true),
            _ => {
                let error = io::Error::last_os_error();
                // A signal, such as the window changing size: nothing was pressed.
                if error.kind() == io::ErrorKind::Interrupted {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
        }
    }

    /// One byte from the terminal; None at its end.
    #[allow(unsafe_code, reason = "reads a byte from the terminal")]
    fn read_byte() -> io::Result<Option<u8>> {
        let mut byte = 0u8;
        // SAFETY: `byte` is one byte, as the count says.
        let read = unsafe { libc::read(STDIN, (&raw mut byte).cast(), 1) };
        match read {
            1 => Ok(Some(byte)),
            0 => Ok(None),
            _ => {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Console::{
        CONSOLE_MODE, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, INPUT_RECORD, KEY_EVENT,
        ReadConsoleInputW, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RIGHT, VK_UP,
    };

    use super::Key;

    /// The console's input and output modes before.
    pub struct Saved {
        input: CONSOLE_MODE,
        output: CONSOLE_MODE,
    }

    #[allow(unsafe_code, reason = "reads and sets the console's modes")]
    pub fn enable() -> io::Result<Saved> {
        let input = mode(handle(STD_INPUT_HANDLE)?)?;
        let output = mode(handle(STD_OUTPUT_HANDLE)?)?;
        // Keys as they are pressed, not echoed, and Ctrl+C as a key.
        let raw_input = input & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
        set_mode(handle(STD_INPUT_HANDLE)?, raw_input)?;
        // The escape sequence that clears a line.
        if let Err(error) = set_mode(
            handle(STD_OUTPUT_HANDLE)?,
            output | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        ) {
            restore(&Saved { input, output });
            return Err(error);
        }
        Ok(Saved { input, output })
    }

    pub fn restore(saved: &Saved) {
        if let Ok(handle) = handle(STD_INPUT_HANDLE) {
            let _ = set_mode(handle, saved.input);
        }
        if let Ok(handle) = handle(STD_OUTPUT_HANDLE) {
            let _ = set_mode(handle, saved.output);
        }
    }

    #[allow(unsafe_code, reason = "reads a key from the console")]
    pub fn read_key(timeout: Duration) -> io::Result<Option<Key>> {
        let input = handle(STD_INPUT_HANDLE)?;
        // SAFETY: `input` is the console's input handle.
        match unsafe { WaitForSingleObject(input, super::millis(timeout)) } {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => return Ok(None),
            _ => return Err(io::Error::last_os_error()),
        }
        let mut record = INPUT_RECORD::default();
        let mut read = 0;
        // SAFETY: `record` is one record, as the count says.
        if unsafe { ReadConsoleInputW(input, &mut record, 1, &mut read) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if read == 0 || u32::from(record.EventType) != KEY_EVENT {
            return Ok(Some(Key::Other));
        }
        // SAFETY: the record is a key event, as its type says.
        let key = unsafe { record.Event.KeyEvent };
        if key.bKeyDown == 0 {
            return Ok(None);
        }
        // SAFETY: the character is the union's only member the player reads.
        let character = unsafe { key.uChar.UnicodeChar };
        let key = match key.wVirtualKeyCode {
            VK_UP => Key::Up,
            VK_DOWN => Key::Down,
            VK_LEFT => Key::Left,
            VK_RIGHT => Key::Right,
            VK_ESCAPE => Key::Escape,
            _ => match character {
                0x03 => Key::CtrlC,
                0 => Key::Other,
                unit => char::from_u32(u32::from(unit)).map_or(Key::Other, Key::Char),
            },
        };
        Ok(Some(key))
    }

    #[allow(unsafe_code, reason = "asks for one of the console's handles")]
    fn handle(which: u32) -> io::Result<HANDLE> {
        let handle = unsafe { GetStdHandle(which) };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(handle)
    }

    #[allow(unsafe_code, reason = "reads a console mode")]
    fn mode(handle: HANDLE) -> io::Result<CONSOLE_MODE> {
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(mode)
    }

    #[allow(unsafe_code, reason = "sets a console mode")]
    fn set_mode(handle: HANDLE, mode: CONSOLE_MODE) -> io::Result<()> {
        if unsafe { SetConsoleMode(handle, mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
