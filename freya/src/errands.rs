//! Errands to the server whose answers the window does not wait for, like a play noted or a
//! scrobble: run one after another, in the order given, on a thread of their own.

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel, sync_channel};
use std::time::Duration;

type Errand = Box<dyn FnOnce() + Send>;

/// How long quitting waits for the errands given before.
const FINISHING: Duration = Duration::from_secs(5);

fn errands() -> Option<&'static Sender<Errand>> {
    static ERRANDS: OnceLock<Option<Sender<Errand>>> = OnceLock::new();
    ERRANDS
        .get_or_init(|| {
            let (send, errands) = channel::<Errand>();
            std::thread::Builder::new()
                .name("freya-errands".into())
                .spawn(move || {
                    for errand in errands {
                        errand();
                    }
                })
                .ok()?;
            Some(send)
        })
        .as_ref()
}

/// Runs `errand` once those given before it ran; here and now when there is no thread for
/// them.
pub fn run(errand: impl FnOnce() + Send + 'static) {
    match errands() {
        Some(errands) => {
            if let Err(unsent) = errands.send(Box::new(errand)) {
                (unsent.0)();
            }
        }
        None => errand(),
    }
}

/// Waits a while for the errands given so far: before quitting, so a play is noted.
pub fn finish() {
    let (done, finished) = sync_channel(1);
    run(move || {
        let _ = done.send(());
    });
    if finished.recv_timeout(FINISHING).is_err() {
        log::warn!("quit before the server was told of everything");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn errands_run_in_order() {
        let done = Arc::new(Mutex::new(Vec::new()));
        for i in 0..100 {
            let done = Arc::clone(&done);
            run(move || done.lock().unwrap().push(i));
        }
        finish();
        assert_eq!(*done.lock().unwrap(), (0..100).collect::<Vec<_>>());
    }
}
