//! Answering clients looking for servers on the network: each sends
//! [`DISCOVERY_QUESTION`] to every computer on it, and the server answers with its name, where
//! it takes clients and its certificate's fingerprint. Nothing secret: a client still pins the
//! certificate and logs in before it is let in.

use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use freya_protocol::{DISCOVERY_PORT, DISCOVERY_QUESTION, ServerFound};

/// Answers until dropped.
pub struct Discovery {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Discovery {
    /// Answers on [`DISCOVERY_PORT`] with what `found` says now.
    pub fn start(found: impl Fn() -> ServerFound + Send + 'static) -> std::io::Result<Self> {
        Self::on(DISCOVERY_PORT, found)
    }

    fn on(port: u16, found: impl Fn() -> ServerFound + Send + 'static) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", port))?;
        // So it can look at `stop` now and then.
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("freya-discovery".into())
            .spawn(move || {
                let mut question = [0; 64];
                while !stopped.load(Ordering::Relaxed) {
                    let Ok((read, from)) = socket.recv_from(&mut question) else {
                        continue;
                    };
                    if &question[..read] != DISCOVERY_QUESTION {
                        continue;
                    }
                    if let Ok(answer) = serde_json::to_vec(&found()) {
                        let _ = socket.send_to(&answer, from);
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_one_looking_for_servers() {
        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let _discovery = Discovery::on(port, || ServerFound {
            name: "Mini".into(),
            port: 47475,
            fingerprint: "AB".into(),
            version: "1".into(),
        })
        .unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .send_to(b"something else", ("127.0.0.1", port))
            .unwrap();
        client
            .send_to(DISCOVERY_QUESTION, ("127.0.0.1", port))
            .unwrap();
        let mut answer = [0; 1024];
        let read = client.recv(&mut answer).unwrap();
        let found: ServerFound = serde_json::from_slice(&answer[..read]).unwrap();
        assert_eq!(found.name, "Mini");
    }
}
