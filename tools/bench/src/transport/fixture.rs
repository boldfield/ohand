//! Local TLS fixture servers for transport tests. Loopback only; certificates are generated per
//! test run and nothing here is a real endpoint or credential.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

pub struct TestAuthority {
    certificate: rcgen::Certificate,
    key: KeyPair,
}

pub struct ServerIdentity {
    chain: Vec<CertificateDer<'static>>,
    key_der: Vec<u8>,
}

impl TestAuthority {
    pub fn new() -> TestAuthority {
        let mut params = CertificateParams::new(Vec::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name = DistinguishedName::new();
        let key = KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap();
        TestAuthority { certificate, key }
    }

    pub fn root_der(&self) -> Vec<u8> {
        self.certificate.der().to_vec()
    }

    pub fn issue(&self, dns_name: &str) -> ServerIdentity {
        let params = CertificateParams::new(vec![dns_name.to_string()]).unwrap();
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = params
            .signed_by(&leaf_key, &self.certificate, &self.key)
            .unwrap();
        ServerIdentity {
            chain: vec![leaf.der().clone()],
            key_der: leaf_key.serialize_der(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub request_line: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub enum Reply {
    Full {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// Accept the request and never answer.
    Hang,
    /// Announce a large body, then stream bytes until the client goes away.
    Endless,
}

impl Reply {
    pub fn ok(body: &[u8]) -> Reply {
        Reply::status(200, body)
    }

    pub fn status(status: u16, body: &[u8]) -> Reply {
        Reply::Full {
            status,
            headers: Vec::new(),
            body: body.to_vec(),
        }
    }
}

type Handler = dyn Fn(&RecordedRequest) -> Reply + Send + Sync;

pub struct Fixture {
    port: u16,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    connections: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl Fixture {
    pub fn start(
        identity: ServerIdentity,
        handler: impl Fn(&RecordedRequest) -> Reply + Send + Sync + 'static,
    ) -> Fixture {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = Arc::new(
            ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    identity.chain,
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der)),
                )
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);

        let accept_thread = {
            let requests = Arc::clone(&requests);
            let connections = Arc::clone(&connections);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            connections.fetch_add(1, Ordering::SeqCst);
                            let config = Arc::clone(&config);
                            let handler = Arc::clone(&handler);
                            let requests = Arc::clone(&requests);
                            let stop = Arc::clone(&stop);
                            thread::spawn(move || {
                                serve(stream, config, &*handler, &requests, &stop)
                            });
                        }
                        Err(_) => thread::sleep(Duration::from_millis(2)),
                    }
                }
            })
        };
        Fixture {
            port,
            requests,
            connections,
            stop,
            accept_thread: Some(accept_thread),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("https://localhost:{}{path}", self.port)
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.accept_thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    stream: TcpStream,
    config: Arc<ServerConfig>,
    handler: &Handler,
    requests: &Mutex<Vec<RecordedRequest>>,
    stop: &AtomicBool,
) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let connection = ServerConnection::new(config).unwrap();
    let mut tls = StreamOwned::new(connection, stream);
    let Some(request) = read_request(&mut tls) else {
        return;
    };
    requests.lock().unwrap().push(request.clone());
    match handler(&request) {
        Reply::Full {
            status,
            headers,
            body,
        } => {
            let mut head = format!(
                "HTTP/1.1 {status} Fixture\r\ncontent-length: {}\r\n",
                body.len()
            );
            for (name, value) in headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("connection: close\r\n\r\n");
            let _ = tls.write_all(head.as_bytes());
            let _ = tls.write_all(&body);
            let _ = tls.flush();
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
        Reply::Hang => {
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(10));
            }
        }
        Reply::Endless => {
            let head =
                "HTTP/1.1 200 Fixture\r\ncontent-length: 1000000000\r\nconnection: close\r\n\r\n";
            if tls.write_all(head.as_bytes()).is_err() {
                return;
            }
            let chunk = [b'x'; 4096];
            while !stop.load(Ordering::SeqCst) {
                if tls.write_all(&chunk).is_err() {
                    return;
                }
            }
        }
    }
}

fn read_request(tls: &mut StreamOwned<ServerConnection, TcpStream>) -> Option<RecordedRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        match tls.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect();
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        match tls.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(count) => body.extend_from_slice(&chunk[..count]),
        }
    }
    Some(RecordedRequest {
        request_line,
        headers,
        body,
    })
}
