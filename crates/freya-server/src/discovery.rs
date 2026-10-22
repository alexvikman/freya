//! Answering clients looking for servers on the network: each sends
//! [`DISCOVERY_QUESTION`] to every computer on it, and the server answers with its name, where
//! it takes clients and its certificate's fingerprint. Nothing secret: a client still proves
//! the certificate with a connection code and logs in before it is let in.
//!
//! Only questions from this network are answered, from a private, link-local or this computer's
//! own address, and from one address at most twice a second: so the server cannot be used to
//! flood another computer with answers to questions sent in its name.

use std::collections::HashMap;
use std::net::{IpAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often one address is answered at most.
const ANSWER_EVERY: Duration = Duration::from_millis(500);

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
                let mut answered: HashMap<IpAddr, Instant> = HashMap::new();
                while !stopped.load(Ordering::Relaxed) {
                    let Ok((read, from)) = socket.recv_from(&mut question) else {
                        continue;
                    };
                    if &question[..read] != DISCOVERY_QUESTION || !is_near(from.ip()) {
                        continue;
                    }
                    let now = Instant::now();
                    if answered.len() > 1024 {
                        answered.retain(|_, at| now.duration_since(*at) < ANSWER_EVERY);
                    }
                    if answered
                        .get(&from.ip())
                        .is_some_and(|at| now.duration_since(*at) < ANSWER_EVERY)
                    {
                        continue;
                    }
                    answered.insert(from.ip(), now);
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

/// Whether `ip` is on this network: private, link-local, or this computer's own.
fn is_near(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
        IpAddr::V6(ip) => {
            ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_loopback()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|ip| ip.is_private() || ip.is_link_local() || ip.is_loopback())
        }
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

    #[test]
    fn answers_only_this_network() {
        assert!(is_near("192.168.1.20".parse().unwrap()));
        assert!(is_near("10.0.0.5".parse().unwrap()));
        assert!(is_near("172.16.12.169".parse().unwrap()));
        assert!(is_near("169.254.3.4".parse().unwrap()));
        assert!(is_near("127.0.0.1".parse().unwrap()));
        assert!(is_near("fe80::1".parse().unwrap()));
        assert!(is_near("fd12::1".parse().unwrap()));
        assert!(!is_near("8.8.8.8".parse().unwrap()));
        assert!(!is_near("2001:db8::1".parse().unwrap()));
    }
}
