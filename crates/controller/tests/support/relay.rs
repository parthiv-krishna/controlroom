//! A TCP relay in front of a test host's SSH port. Taking it down drops every connection and
//! refuses new ones (they are accepted and closed at once), so the machine becomes unreachable
//! until it is brought up again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// A relay to `127.0.0.1:<target>`.
pub struct Relay {
    /// The port to connect to instead of the target.
    pub port: u16,
    up: Arc<AtomicBool>,
    links: Arc<Mutex<Vec<JoinHandle<()>>>>,
    accept: JoinHandle<()>,
}

impl Relay {
    /// Starts a relay that is up.
    pub async fn start(target: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let up = Arc::new(AtomicBool::new(true));
        let links: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::default();
        let accept = tokio::spawn({
            let (up, links) = (up.clone(), links.clone());
            async move {
                while let Ok((mut inbound, _)) = listener.accept().await {
                    if !up.load(Ordering::SeqCst) {
                        continue; // dropped: closed at once
                    }
                    let link = tokio::spawn(async move {
                        if let Ok(mut outbound) = TcpStream::connect(("127.0.0.1", target)).await {
                            let _ =
                                tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                        }
                    });
                    links.lock().unwrap().push(link);
                }
            }
        });
        Self {
            port,
            up,
            links,
            accept,
        }
    }

    /// Drops every connection and refuses new ones.
    pub fn down(&self) {
        self.up.store(false, Ordering::SeqCst);
        for link in self.links.lock().unwrap().drain(..) {
            link.abort();
        }
    }

    /// Accepts connections again.
    pub fn up(&self) {
        self.up.store(true, Ordering::SeqCst);
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.accept.abort();
        self.down();
    }
}
