//! One HTTPS transport for all three adapters: a `curl` subprocess, as in `live_smoke.rs`
//! (https only, certificate validation on, redirects not followed, credential on stdin).
//!
//! This is an adapted copy of the `live_smoke.rs` transport, not a reuse of it: that one is private
//! to another test target, and the bench transport lives in a different crate.
//!
//! Credentials are bound to the origin they were registered for and are refused for any other,
//! the same rule as the host transport in `tools/bench/src/transport/`. The CI proof adds two
//! test-only knobs, both off on the live path: a trusted CA file and host remapping
//! (`--connect-to`), so production endpoints resolve to a local TLS fixture server.

use ohand_core::providers::anthropic::{AnthropicTransport, HttpRequest, HttpResponse};
use ohand_core::providers::contracts::{CancelToken, Clock, TransportError};
use ohand_core::providers::openai::HttpTransport;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// What the transport saw of one exchange. Raw bodies go only to the private evidence file.
#[derive(Debug, Clone, Default)]
pub struct Exchange {
    pub http_status: Option<u16>,
    pub transport_failure: Option<String>,
    pub latency_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub raw_request: Vec<u8>,
    pub raw_response: Vec<u8>,
}

#[derive(Default)]
pub struct TestRouting {
    pub ca_file: Option<String>,
    /// `(host, port)` to the local `(127.0.0.1, port)` that serves it.
    pub connect_to: Vec<(String, u16, u16)>,
}

pub struct CurlTransport {
    /// credential reference to (secret, origin the secret may be sent to).
    credentials: Mutex<HashMap<String, (String, String)>>,
    observed: Mutex<Vec<Exchange>>,
    routing: TestRouting,
    max_seconds: u32,
}

/// Origin (`https://host[:port]`) of an https URL, lowercased.
pub fn origin_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    let authority = authority.strip_suffix(":443").unwrap_or(&authority);
    Some(format!("https://{authority}"))
}

fn curl_quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn host_and_port(url: &str) -> Option<(String, u16)> {
    let authority = url
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_ascii_lowercase(), port.parse().ok()?)),
        None => Some((authority.to_ascii_lowercase(), 443)),
    }
}

impl CurlTransport {
    pub fn new(max_seconds: u32, routing: TestRouting) -> Arc<CurlTransport> {
        Arc::new(CurlTransport {
            credentials: Mutex::new(HashMap::new()),
            observed: Mutex::new(Vec::new()),
            routing,
            max_seconds,
        })
    }

    /// Bind `secret` to `credential_ref`, usable only toward `origin`.
    pub fn bind_credential(&self, credential_ref: &str, secret: &str, origin: &str) {
        self.credentials.lock().unwrap().insert(
            credential_ref.to_string(),
            (secret.to_string(), origin.to_string()),
        );
    }

    /// Exchanges since the last call, oldest first.
    pub fn drain_exchanges(&self) -> Vec<Exchange> {
        std::mem::take(&mut *self.observed.lock().unwrap())
    }

    #[allow(clippy::too_many_arguments)]
    fn send_one(
        &self,
        url: &str,
        credential_ref: &str,
        credential_header: Option<&str>,
        headers: &[(String, String)],
        body: Vec<u8>,
        remaining_ms: u64,
        max_response_bytes: usize,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        let mut exchange = Exchange {
            raw_request: body.clone(),
            ..Exchange::default()
        };
        let secret = {
            let credentials = self.credentials.lock().unwrap();
            match credentials.get(credential_ref) {
                Some((secret, origin)) if origin_of(url).as_deref() == Some(origin.as_str()) => {
                    secret.clone()
                }
                _ => {
                    exchange.transport_failure = Some("credential-not-bound-to-origin".into());
                    self.observed.lock().unwrap().push(exchange);
                    return Err(TransportError::Rejected);
                }
            }
        };

        let body_file = tempfile::NamedTempFile::new().expect("temporary request body");
        std::fs::write(body_file.path(), &body).expect("write request body");
        let mut config = format!("url = {}\n", curl_quoted(url));
        let credential_line = match credential_header {
            Some(name) => format!("{name}: {secret}"),
            None => format!("Authorization: Bearer {secret}"),
        };
        config.push_str(&format!("header = {}\n", curl_quoted(&credential_line)));
        for (name, value) in headers {
            config.push_str(&format!(
                "header = {}\n",
                curl_quoted(&format!("{name}: {value}"))
            ));
        }
        config.push_str(&format!(
            "data-binary = {}\n",
            curl_quoted(&format!("@{}", body_file.path().display()))
        ));

        let max_time = (remaining_ms.div_ceil(1000)).clamp(1, u64::from(self.max_seconds));
        let mut command = Command::new("curl");
        command.args([
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--max-time",
            &max_time.to_string(),
            "--write-out",
            "\n%{http_code}",
            "--config",
            "-",
        ]);
        if let Some(ca_file) = &self.routing.ca_file {
            command.args(["--cacert", ca_file]);
        }
        if let Some((host, port)) = host_and_port(url) {
            for (mapped_host, mapped_port, local_port) in &self.routing.connect_to {
                if *mapped_host == host && *mapped_port == port {
                    command.args([
                        "--connect-to",
                        &format!("{host}:{port}:127.0.0.1:{local_port}"),
                    ]);
                }
            }
        }
        let started = Instant::now();
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("curl is installed");
        child
            .stdin
            .take()
            .expect("curl stdin")
            .write_all(config.as_bytes())
            .expect("pass configuration to curl on stdin");
        let output = child.wait_with_output().expect("curl finishes");
        exchange.latency_ms = started.elapsed().as_millis() as u64;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            exchange.transport_failure = Some(format!("curl-exit-{code}"));
            self.observed.lock().unwrap().push(exchange);
            return Err(if code == 28 {
                TransportError::Timeout
            } else {
                TransportError::Unavailable
            });
        }
        let stdout = output.stdout;
        let split = stdout.iter().rposition(|byte| *byte == b'\n');
        let status = split
            .and_then(|at| std::str::from_utf8(&stdout[at + 1..]).ok())
            .and_then(|text| text.trim().parse::<u16>().ok());
        let (Some(split), Some(status)) = (split, status) else {
            exchange.transport_failure = Some("unreadable-status".into());
            self.observed.lock().unwrap().push(exchange);
            return Err(TransportError::Unavailable);
        };
        let mut response = stdout[..split].to_vec();
        response.truncate(max_response_bytes + 1);

        exchange.http_status = Some(status);
        exchange.raw_response = response.clone();
        if let Ok(envelope) = serde_json::from_slice::<Value>(&response) {
            let usage = &envelope["usage"];
            exchange.input_tokens = usage["prompt_tokens"]
                .as_u64()
                .or_else(|| usage["input_tokens"].as_u64());
            exchange.output_tokens = usage["completion_tokens"]
                .as_u64()
                .or_else(|| usage["output_tokens"].as_u64());
        }
        self.observed.lock().unwrap().push(exchange);
        Ok((status, response))
    }
}

/// `HttpTransport` handle (self-hosted and OpenAI adapters).
#[derive(Clone)]
pub struct SharedTransport(pub Arc<CurlTransport>);

impl HttpTransport for SharedTransport {
    fn post(
        &self,
        endpoint: &str,
        credential_ref: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        deadline_ms: u64,
        _cancel: &CancelToken,
        clock: &dyn Clock,
        max_response_bytes: u64,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        self.0.send_one(
            endpoint,
            credential_ref,
            None,
            &headers,
            body,
            deadline_ms.saturating_sub(clock.now_ms()),
            max_response_bytes as usize,
        )
    }
}

impl AnthropicTransport for CurlTransport {
    fn send(
        &self,
        request: &HttpRequest,
        _cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        let attachment = request
            .credential
            .as_ref()
            .ok_or(TransportError::Rejected)?;
        let (status, body) = self.send_one(
            &request.url,
            &attachment.reference,
            Some(&attachment.header),
            &request.headers,
            request.body.clone(),
            request.timeout_ms,
            request.max_response_bytes,
        )?;
        Ok(HttpResponse {
            status,
            headers: Vec::new(),
            body,
        })
    }
}
