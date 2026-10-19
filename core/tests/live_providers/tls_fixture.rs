//! A local TLS fixture server for the no-network proof (see `fixture_server.py`).
//!
//! A throwaway self-signed certificate (made with the `openssl` command) names the production
//! hosts the proof reaches, so the real adapters keep their production endpoints and the
//! transport remaps them to the local port with `--connect-to` and trusts only that certificate.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use tempfile::TempDir;

pub const SELF_HOSTED_HOST: &str = "self-hosted.fixture.test";
pub const SELF_HOSTED_PORT: u16 = 8443;
pub const SELF_HOSTED_BASE_URL: &str = "https://self-hosted.fixture.test:8443/v1";
pub const FIXTURE_CREDENTIAL: &str = "fixture-credential-7f3a9c1e5b2d4468";
pub const MODEL_OUTPUT_MARKER: &str = "fixture-model-output-marker";

pub struct TlsFixture {
    directory: TempDir,
    child: Child,
    pub port: u16,
}

impl TlsFixture {
    pub fn start() -> TlsFixture {
        let directory = tempfile::tempdir().expect("fixture directory");
        let state = directory.path();
        let status = Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            ])
            .args(["-subj", "/CN=live-harness-fixture"])
            .args([
                "-addext",
                &format!(
                    "subjectAltName=DNS:{SELF_HOSTED_HOST},DNS:api.anthropic.com,DNS:api.openai.com"
                ),
            ])
            .arg("-keyout")
            .arg(state.join("key.pem"))
            .arg("-out")
            .arg(state.join("cert.pem"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("the openssl command is installed");
        assert!(status.success(), "openssl made the fixture certificate");

        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/live_providers/fixture_server.py");
        let mut child = Command::new("python3")
            .arg("-I")
            .arg(script)
            .arg(state)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 is installed");
        let mut first_line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut first_line)
            .expect("fixture server reports its port");
        let port = first_line.trim().parse().expect("numeric port");
        let fixture = TlsFixture {
            directory,
            child,
            port,
        };
        fixture.set_credential(FIXTURE_CREDENTIAL);
        fixture.set_mode("ok");
        fixture
    }

    fn file(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    pub fn ca_file(&self) -> String {
        self.file("cert.pem").to_string_lossy().into_owned()
    }

    pub fn set_credential(&self, secret: &str) {
        std::fs::write(self.file("credential.txt"), secret).unwrap();
    }

    pub fn set_mode(&self, mode: &str) {
        std::fs::write(self.file("mode.txt"), mode).unwrap();
    }

    /// The proposal object the server returns in `ok` mode.
    pub fn set_reply(&self, proposal: &serde_json::Value) {
        std::fs::write(self.file("reply.json"), proposal.to_string()).unwrap();
    }

    /// `(path, status)` for every request served so far.
    pub fn requests(&self) -> Vec<(String, u16)> {
        std::fs::read_to_string(self.file("requests.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (path, status) = line.split_once(' ')?;
                Some((path.to_string(), status.parse().ok()?))
            })
            .collect()
    }
}

impl Drop for TlsFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
