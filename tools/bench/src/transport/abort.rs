//! Tears down the live connection of one dispatch so cancellation and expiry stop the exchange
//! itself, not only the caller waiting on it.

use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex};

use ureq::{Error, ReadWrite, TlsConnector};

#[derive(Default)]
struct AbortState {
    aborted: bool,
    sockets: Vec<TcpStream>,
}

/// Shared between the waiting caller and the worker's connector for exactly one dispatch.
#[derive(Default)]
pub(crate) struct AbortHandle {
    state: Mutex<AbortState>,
}

impl AbortHandle {
    /// Shuts down every connection the dispatch opened, and any it opens later.
    pub(crate) fn abort(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.aborted = true;
        for socket in state.sockets.drain(..) {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    fn register(&self, socket: &TcpStream) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.aborted {
            let _ = socket.shutdown(Shutdown::Both);
        } else if let Ok(clone) = socket.try_clone() {
            state.sockets.push(clone);
        }
    }
}

/// Registers each TCP connection with the dispatch's [`AbortHandle`] before any TLS bytes are
/// exchanged, then hands the connection to the verifying rustls connector.
pub(crate) struct AbortableTls {
    pub(crate) config: Arc<rustls::ClientConfig>,
    pub(crate) abort: Arc<AbortHandle>,
}

impl TlsConnector for AbortableTls {
    fn connect(&self, dns_name: &str, io: Box<dyn ReadWrite>) -> Result<Box<dyn ReadWrite>, Error> {
        if let Some(socket) = io.socket() {
            self.abort.register(socket);
        }
        TlsConnector::connect(&self.config, dns_name, io)
    }
}
