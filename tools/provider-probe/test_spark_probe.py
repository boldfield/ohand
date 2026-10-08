import contextlib
import io
import json
import os
import re
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

import spark_probe

PROBE_SCRIPT = Path(spark_probe.__file__).resolve()
TEST_CREDENTIAL = "sk-synthetic-credential-7f3a91"
PROVIDER_KEY = "stubprovider"
VALID_STRUCTURED_CONTENT = '{"status": "ok", "count": 3}'
EVIDENCE_ID_PATTERN = re.compile(r"^spark-probe-\d{8}T\d{6}Z-([0-9a-f]{8}|unknown)(-dirty)?$")


def chat_body(content, role="assistant"):
    return {"choices": [{"index": 0, "message": {"role": role, "content": content}}]}


class StubBehavior:
    def __init__(self):
        self.models_payload = {"object": "list", "data": [{"id": "listed-a", "object": "model", "owned_by": "stub"},
                                                          {"id": "listed-b", "object": "model"}]}
        self.models_raw_body = None
        self.chat_payload = chat_body(VALID_STRUCTURED_CONTENT)
        self.chat_raw_body = None
        self.require_authentication_for_models = True
        self.require_authentication_for_chat = True
        self.unauthenticated_models_status = 401
        self.authenticated_models_status = 200
        self.chat_status = 200
        self.server_header = None
        self.response_delay_seconds = 0
        self.received = []


def make_handler(behavior):
    class StubHandler(BaseHTTPRequestHandler):
        def log_message(self, *arguments):
            pass

        def send_json(self, status, payload=None, raw=None):
            body = raw if raw is not None else json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def version_string(self):
            return behavior.server_header or "synthetic-stub/1.0"

        def record(self, body=None):
            behavior.received.append({
                "method": self.command,
                "path": self.path,
                "authorization": self.headers.get("Authorization"),
                "body": body,
            })

        def is_authenticated(self):
            return self.headers.get("Authorization") == f"Bearer {TEST_CREDENTIAL}"

        def do_GET(self):
            self.record()
            if behavior.response_delay_seconds:
                threading.Event().wait(behavior.response_delay_seconds)
            if self.path != "/v1/models":
                self.send_json(404, {"error": "not found"})
            elif behavior.require_authentication_for_models and not self.is_authenticated():
                self.send_json(behavior.unauthenticated_models_status, {"error": "unauthorized"})
            elif not behavior.require_authentication_for_models or self.is_authenticated():
                if behavior.models_raw_body is not None:
                    self.send_json(behavior.authenticated_models_status, raw=behavior.models_raw_body)
                else:
                    self.send_json(behavior.authenticated_models_status, behavior.models_payload)

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(length) or b"null")
            self.record(body)
            if self.path != "/v1/chat/completions":
                self.send_json(404, {"error": "not found"})
            elif behavior.require_authentication_for_chat and not self.is_authenticated():
                self.send_json(401, {"error": "unauthorized"})
            elif behavior.chat_raw_body is not None:
                self.send_json(behavior.chat_status, raw=behavior.chat_raw_body)
            else:
                self.send_json(behavior.chat_status, behavior.chat_payload)

    return StubHandler


class StubServer:
    def __init__(self, behavior, tls_certificate=None):
        self.behavior = behavior
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(behavior))
        if tls_certificate is not None:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(*tls_certificate)
            self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.scheme = "https" if tls_certificate else "http"
        self.address = f"127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exception_info):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    @property
    def base_url(self):
        return f"{self.scheme}://{self.address}/v1"


def closed_local_port():
    with socket.socket() as probe_socket:
        probe_socket.bind(("127.0.0.1", 0))
        return probe_socket.getsockname()[1]


def generate_self_signed_certificate(directory):
    certificate_path = Path(directory) / "stub-cert.pem"
    key_path = Path(directory) / "stub-key.pem"
    subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2",
         "-keyout", str(key_path), "-out", str(certificate_path), "-subj", "/CN=localhost",
         "-addext", "subjectAltName=IP:127.0.0.1,DNS:localhost"],
        check=True, capture_output=True,
    )
    return str(certificate_path), str(key_path)


class ProbeTestCase(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary_directory.cleanup)
        self.workspace = Path(self.temporary_directory.name)
        self.output_directory = self.workspace / "evidence"
        self.config_path = self.workspace / "models.json"
        self.behavior = StubBehavior()

    def write_config(self, base_url, credential=TEST_CREDENTIAL, provider_key=PROVIDER_KEY, extra=None):
        provider = {"baseUrl": base_url, "apiKey": credential, "models": [{"id": "config-only-model"}]}
        if extra:
            provider.update(extra)
        provider = {key: value for key, value in provider.items() if value is not None}
        self.config_path.write_text(json.dumps({"providers": {provider_key: provider}}), encoding="utf-8")

    def run_probe(self, *extra_arguments, provider=PROVIDER_KEY):
        arguments = ["--provider", provider, "--config", str(self.config_path),
                     "--output-dir", str(self.output_directory), *extra_arguments]
        standard_output, standard_error = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(standard_output), contextlib.redirect_stderr(standard_error):
            exit_code = spark_probe.main(arguments)
        return exit_code, standard_output.getvalue(), standard_error.getvalue()

    def read_artifact(self):
        artifacts = sorted(self.output_directory.glob("*.json"))
        self.assertEqual(len(artifacts), 1, f"expected exactly one artifact, found {len(artifacts)}")
        text = artifacts[0].read_text(encoding="utf-8")
        self.assertTrue(text.endswith("\n"))
        return json.loads(text), text

    def run_against_stub(self, *extra_arguments):
        with StubServer(self.behavior) as stub:
            self.write_config(stub.base_url)
            self.stub = stub
            result = self.run_probe(*extra_arguments)
        return result

    def assert_sanitized(self, *texts, stub=None):
        forbidden = [TEST_CREDENTIAL]
        if stub is not None:
            forbidden += [stub.address, stub.base_url, "127.0.0.1"]
        for text in texts:
            for value in forbidden:
                self.assertNotIn(value, text)


class SuccessfulRunTests(ProbeTestCase):
    def test_full_pass_writes_complete_sanitized_artifact(self):
        exit_code, standard_output, standard_error = self.run_against_stub()
        self.assertEqual(exit_code, 0, standard_error)
        artifact, artifact_text = self.read_artifact()
        self.assert_sanitized(standard_output, standard_error, artifact_text, stub=self.stub)

        self.assertEqual(artifact["result"], "pass")
        self.assertEqual(artifact["failures"], [])
        self.assertRegex(artifact["evidence_id"], EVIDENCE_ID_PATTERN)
        self.assertNotIn(":", artifact["evidence_id"])
        self.assertTrue(re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", artifact["collected_at"]))
        self.assertIsInstance(artifact["probe_revision"]["dirty"], bool)
        self.assertTrue(re.fullmatch(r"[0-9a-f]{40}|unknown", artifact["probe_revision"]["commit"]))
        self.assertTrue((self.output_directory / f"{artifact['evidence_id']}.json").is_file())
        self.assertIn("phone-context reachability (owned by V08b)", artifact["unverified"])

        by_name = {request["name"]: request for request in artifact["requests"]}
        self.assertEqual(list(by_name), ["models-unauthenticated", "models-authenticated", "chat-structured", "chat-unauthenticated"])
        self.assertEqual(by_name["models-unauthenticated"]["status"], 401)
        self.assertFalse(by_name["models-unauthenticated"]["credential_sent"])
        self.assertEqual(by_name["models-authenticated"]["status"], 200)
        self.assertTrue(by_name["models-authenticated"]["credential_sent"])
        self.assertEqual((by_name["chat-structured"]["method"], by_name["chat-structured"]["path"]), ("POST", "/chat/completions"))
        self.assertEqual(by_name["chat-unauthenticated"]["status"], 401)
        for request in artifact["requests"]:
            self.assertIn("server", request["response_headers"])
            self.assertIn("content-type", request["response_headers"])
            self.assertIn("date", request["response_headers"])
            self.assertLessEqual(set(request["response_headers"]), {"server", "content-type", "date"})
            self.assertIsNone(request["error"])

        self.assertEqual([model["id"] for model in artifact["models"]], ["listed-a", "listed-b"])
        self.assertEqual(artifact["classifications"], {
            "authentication_models": "authentication-required",
            "authentication_chat": "authentication-required",
            "models": "openai-compatible",
            "chat": "openai-compatible",
            "transport": "cleartext-http",
        })
        self.assertEqual(artifact["configured_scheme"], "http")
        self.assertTrue(all(request["tls_verified"] is False for request in artifact["requests"]))
        self.assertEqual(artifact["structured_response"]["passed"], True)
        self.assertEqual(artifact["structured_response"]["model"], "listed-a")
        self.assertEqual(artifact["structured_response"]["observed_keys"], ["count", "status"])

    def test_chat_request_asks_for_json_shape_and_uses_a_listed_model(self):
        exit_code, _, _ = self.run_against_stub()
        self.assertEqual(exit_code, 0)
        chat_requests = [r for r in self.behavior.received if r["method"] == "POST" and r["authorization"]]
        self.assertEqual(len(chat_requests), 1)
        body = chat_requests[0]["body"]
        self.assertEqual(body["model"], "listed-a")
        self.assertNotEqual(body["model"], "config-only-model")
        self.assertEqual(body["response_format"]["type"], "json_schema")
        schema = body["response_format"]["json_schema"]["schema"]
        self.assertEqual(sorted(schema["required"]), ["count", "status"])

    def test_model_flag_selects_another_listed_model(self):
        exit_code, _, _ = self.run_against_stub("--model", "listed-b")
        self.assertEqual(exit_code, 0)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["structured_response"]["model"], "listed-b")

    def test_unlisted_model_flag_fails_without_chat_request(self):
        exit_code, _, standard_error = self.run_against_stub("--model", "config-only-model")
        self.assertEqual(exit_code, 1)
        self.assertIn("requested-model-not-listed-by-endpoint", standard_error)
        self.assertEqual([r for r in self.behavior.received if r["method"] == "POST"], [])

    def test_config_path_falls_back_to_environment_variable(self):
        with StubServer(self.behavior) as stub:
            self.write_config(stub.base_url)
            with mock.patch.dict(os.environ, {spark_probe.CONFIG_PATH_ENV_VAR: str(self.config_path)}):
                standard_output, standard_error = io.StringIO(), io.StringIO()
                with contextlib.redirect_stdout(standard_output), contextlib.redirect_stderr(standard_error):
                    exit_code = spark_probe.main(["--provider", PROVIDER_KEY, "--output-dir", str(self.output_directory)])
        self.assertEqual(exit_code, 0, standard_error.getvalue())

    def test_process_exit_codes_and_no_repository_writes(self):
        repository_evidence = spark_probe.DEFAULT_OUTPUT_DIRECTORY
        before = sorted(repository_evidence.glob("*")) if repository_evidence.exists() else []
        with StubServer(self.behavior) as stub:
            self.write_config(stub.base_url)
            environment = {**os.environ, spark_probe.CONFIG_PATH_ENV_VAR: str(self.config_path)}
            passed = subprocess.run([sys.executable, str(PROBE_SCRIPT), "--provider", PROVIDER_KEY,
                                     "--output-dir", str(self.output_directory)],
                                    capture_output=True, text=True, env=environment, cwd=self.workspace)
            self.behavior.require_authentication_for_models = False
            failed = subprocess.run([sys.executable, str(PROBE_SCRIPT), "--provider", PROVIDER_KEY,
                                     "--output-dir", str(self.workspace / "second-evidence")],
                                    capture_output=True, text=True, env=environment, cwd=self.workspace)
        missing = subprocess.run([sys.executable, str(PROBE_SCRIPT), "--provider", PROVIDER_KEY,
                                  "--config", str(self.workspace / "absent.json")],
                                 capture_output=True, text=True, cwd=self.workspace)
        self.assertEqual(passed.returncode, 0, passed.stderr)
        self.assertEqual(failed.returncode, 1)
        self.assertEqual(missing.returncode, 2)
        for completed in (passed, failed, missing):
            self.assert_sanitized(completed.stdout, completed.stderr, stub=stub)
        after = sorted(repository_evidence.glob("*")) if repository_evidence.exists() else []
        self.assertEqual(before, after)

    def test_missing_provider_argument_is_a_usage_error(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            spark_probe.main([])
        self.assertEqual(raised.exception.code, 2)


class AuthenticationTests(ProbeTestCase):
    def test_unauthenticated_models_success_is_authentication_not_required(self):
        self.behavior.require_authentication_for_models = False
        exit_code, standard_output, standard_error = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, artifact_text = self.read_artifact()
        self.assertEqual(artifact["classifications"]["authentication_models"], "authentication-not-required")
        self.assertIn("authentication-not-required-models", artifact["failures"])
        self.assertEqual(artifact["result"], "fail")
        self.assertIn("authentication-not-required-models", standard_error)
        self.assert_sanitized(standard_output, standard_error, artifact_text, stub=self.stub)

    def test_unauthenticated_chat_success_is_authentication_not_required(self):
        self.behavior.require_authentication_for_chat = False
        exit_code, _, _ = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["classifications"]["authentication_chat"], "authentication-not-required")
        self.assertEqual(artifact["classifications"]["authentication_models"], "authentication-required")
        self.assertIn("authentication-not-required-chat", artifact["failures"])

    def test_unauthenticated_unexpected_status_fails(self):
        self.behavior.unauthenticated_models_status = 500
        exit_code, _, _ = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["classifications"]["authentication_models"], "unexpected-status")
        self.assertEqual(artifact["requests"][0]["status"], 500)

    def test_rejected_credential_fails_and_skips_chat(self):
        self.behavior.authenticated_models_status = 401
        exit_code, _, _ = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["classifications"]["models"], "credential-rejected")
        self.assertEqual(artifact["classifications"]["chat"], "not-attempted")
        self.assertEqual([r for r in self.behavior.received if r["method"] == "POST"], [])

    def test_redirects_are_not_followed(self):
        self.behavior.unauthenticated_models_status = 302
        exit_code, _, _ = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["requests"][0]["status"], 302)
        self.assertEqual(artifact["classifications"]["authentication_models"], "unexpected-status")


class ModelsClassificationTests(ProbeTestCase):
    def assert_models_rejected(self, expected_failure_fragment):
        exit_code, _, standard_error = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["classifications"]["models"], "not-openai-compatible")
        self.assertEqual(artifact["models"], [])
        self.assertIn(expected_failure_fragment, standard_error)
        self.assertIn("no-endpoint-listed-model-for-chat", artifact["failures"])
        self.assertEqual([r for r in self.behavior.received if r["method"] == "POST"], [])

    def test_data_that_is_not_a_list(self):
        self.behavior.models_payload = {"data": "not-a-model-array"}
        self.assert_models_rejected("data-missing-or-empty")

    def test_empty_model_list(self):
        self.behavior.models_payload = {"object": "list", "data": []}
        self.assert_models_rejected("data-missing-or-empty")

    def test_entry_without_id(self):
        self.behavior.models_payload = {"data": [{"object": "model"}]}
        self.assert_models_rejected("model-id-missing-or-not-a-nonempty-string")

    def test_numeric_id(self):
        self.behavior.models_payload = {"data": [{"id": 7}]}
        self.assert_models_rejected("model-id-missing-or-not-a-nonempty-string")

    def test_blank_id_in_second_entry(self):
        self.behavior.models_payload = {"data": [{"id": "listed-a"}, {"id": "  "}]}
        self.assert_models_rejected("model-id-missing-or-not-a-nonempty-string")

    def test_entry_that_is_not_an_object(self):
        self.behavior.models_payload = {"data": ["listed-a"]}
        self.assert_models_rejected("model-entry-not-an-object")

    def test_body_that_is_not_json(self):
        self.behavior.models_raw_body = b"<html>not json</html>"
        self.assert_models_rejected("body-not-a-json-object")

    def test_body_that_is_a_json_array(self):
        self.behavior.models_raw_body = b"[]"
        self.assert_models_rejected("body-not-a-json-object")


class ChatClassificationTests(ProbeTestCase):
    def assert_chat_rejected(self, expected_reason):
        exit_code, _, standard_error = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["classifications"]["chat"], "not-openai-compatible")
        self.assertEqual(artifact["structured_response"]["passed"], False)
        self.assertEqual(artifact["structured_response"]["reason"], expected_reason)
        self.assertIn(expected_reason, standard_error)

    def test_non_json_body(self):
        self.behavior.chat_raw_body = b"plain text"
        self.assert_chat_rejected("body-not-a-json-object")

    def test_missing_choices(self):
        self.behavior.chat_payload = {"result": "ok"}
        self.assert_chat_rejected("choices-missing-or-empty")

    def test_empty_choices(self):
        self.behavior.chat_payload = {"choices": []}
        self.assert_chat_rejected("choices-missing-or-empty")

    def test_missing_message(self):
        self.behavior.chat_payload = {"choices": [{"text": "x"}]}
        self.assert_chat_rejected("message-missing")

    def test_wrong_role(self):
        self.behavior.chat_payload = chat_body(VALID_STRUCTURED_CONTENT, role="user")
        self.assert_chat_rejected("message-role-not-assistant")

    def test_content_not_a_string(self):
        self.behavior.chat_payload = chat_body({"status": "ok", "count": 3})
        self.assert_chat_rejected("message-content-not-a-string")

    def test_chat_error_status(self):
        self.behavior.chat_status = 400
        self.behavior.chat_payload = {"error": "response_format unsupported"}
        exit_code, _, _ = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["classifications"]["chat"], "unexpected-status")
        self.assertEqual(artifact["structured_response"]["reason"], "status-400")


class StructuredResponseTests(ProbeTestCase):
    def assert_structured_failure(self, content, expected_reason):
        self.behavior.chat_payload = chat_body(content)
        exit_code, _, standard_error = self.run_against_stub()
        self.assertEqual(exit_code, 1, content)
        artifact, artifact_text = self.read_artifact()
        self.assertEqual(artifact["classifications"]["chat"], "openai-compatible")
        self.assertEqual(artifact["structured_response"]["passed"], False)
        self.assertEqual(artifact["structured_response"]["reason"], expected_reason)
        self.assertIn(f"structured-response-{expected_reason}", artifact["failures"])

    def test_plain_prose(self):
        self.assert_structured_failure("Hello! How can I help you today?", "content-not-json")

    def test_empty_content(self):
        self.assert_structured_failure("", "content-not-json")

    def test_json_number(self):
        self.assert_structured_failure("42", "content-not-a-json-object")

    def test_json_array(self):
        self.assert_structured_failure("[]", "content-not-a-json-object")

    def test_wrong_keys(self):
        self.assert_structured_failure('{"foo": 1}', "content-keys-do-not-match-requested-shape")

    def test_missing_key(self):
        self.assert_structured_failure('{"status": "ok"}', "content-keys-do-not-match-requested-shape")

    def test_extra_key(self):
        self.assert_structured_failure('{"status": "ok", "count": 3, "extra": 1}', "content-keys-do-not-match-requested-shape")

    def test_wrong_status_value(self):
        self.assert_structured_failure('{"status": "bad", "count": 3}', "status-value-does-not-match-requested-shape")

    def test_status_wrong_type(self):
        self.assert_structured_failure('{"status": 1, "count": 3}', "status-value-does-not-match-requested-shape")

    def test_count_as_string(self):
        self.assert_structured_failure('{"status": "ok", "count": "3"}', "count-value-does-not-match-requested-shape")

    def test_count_as_boolean(self):
        self.assert_structured_failure('{"status": "ok", "count": true}', "count-value-does-not-match-requested-shape")

    def test_wrong_count_value(self):
        self.assert_structured_failure('{"status": "ok", "count": 4}', "count-value-does-not-match-requested-shape")

    def test_model_output_is_not_stored_in_artifact(self):
        self.behavior.chat_payload = chat_body("SYNTHETIC-FREE-TEXT-MARKER that is not json")
        self.run_against_stub()
        _, artifact_text = self.read_artifact()
        self.assertNotIn("SYNTHETIC-FREE-TEXT-MARKER", artifact_text)

    def test_valid_content_with_surrounding_whitespace_passes(self):
        self.behavior.chat_payload = chat_body('\n  {"count": 3, "status": "ok"}  \n')
        exit_code, _, _ = self.run_against_stub()
        self.assertEqual(exit_code, 0)


class TransportTests(ProbeTestCase):
    def test_unknown_scheme_fails_without_requests(self):
        for base_url in ("ftp://example.invalid/v1", "127.0.0.1:9/v1", "ws://example.invalid/v1"):
            with self.subTest(base_url=base_url):
                shutil.rmtree(self.output_directory, ignore_errors=True)
                self.write_config(base_url)
                exit_code, standard_output, standard_error = self.run_probe()
                self.assertEqual(exit_code, 1)
                artifact, artifact_text = self.read_artifact()
                self.assertEqual(artifact["configured_scheme"], "unknown")
                self.assertEqual(artifact["classifications"], {"transport": "unknown-scheme"})
                self.assertEqual(artifact["requests"], [])
                self.assertIn("unknown-scheme", standard_error)
                for text in (standard_output, standard_error, artifact_text):
                    self.assertNotIn(base_url, text)
                    self.assertNotIn("example.invalid", text)
                    self.assertNotIn("127.0.0.1", text)

    def test_connection_refused_fails_with_classification(self):
        self.write_config(f"http://127.0.0.1:{closed_local_port()}/v1")
        exit_code, standard_output, standard_error = self.run_probe()
        self.assertEqual(exit_code, 1)
        artifact, artifact_text = self.read_artifact()
        self.assertEqual(artifact["requests"][0]["error"], "connection-failed")
        self.assertIsNone(artifact["requests"][0]["status"])
        self.assertEqual(artifact["classifications"]["models"], "request-failed")
        self.assertIn("unauthenticated-models-connection-failed", standard_error)
        for text in (standard_output, standard_error, artifact_text):
            self.assertNotIn("127.0.0.1", text)
            self.assertNotIn(TEST_CREDENTIAL, text)

    def test_timeout_is_classified(self):
        self.behavior.response_delay_seconds = 1.0
        with mock.patch.object(spark_probe, "MODELS_TIMEOUT_SECONDS", 0.2):
            exit_code, _, _ = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["requests"][0]["error"], "timeout")

    def test_oversized_response_is_rejected(self):
        with mock.patch.object(spark_probe, "MAX_RESPONSE_BYTES", 10):
            exit_code, _, _ = self.run_against_stub()
        self.assertEqual(exit_code, 1)
        artifact, _ = self.read_artifact()
        self.assertEqual(artifact["requests"][1]["error"], "response-too-large")
        self.assertEqual(artifact["classifications"]["models"], "request-failed")


@unittest.skipUnless(shutil.which("openssl"), "openssl is required to create a synthetic certificate")
class HttpsTests(ProbeTestCase):
    def test_certificate_trusted_by_default_store_is_reported_verified(self):
        certificate = generate_self_signed_certificate(self.workspace)
        with StubServer(self.behavior, tls_certificate=certificate) as stub:
            self.write_config(stub.base_url)
            with mock.patch.dict(os.environ, {"SSL_CERT_FILE": certificate[0]}):
                exit_code, standard_output, standard_error = self.run_probe()
        self.assertEqual(exit_code, 0, standard_error)
        artifact, artifact_text = self.read_artifact()
        self.assertEqual(artifact["configured_scheme"], "https")
        self.assertEqual(artifact["classifications"]["transport"], "https-verified")
        self.assertTrue(all(request["tls_verified"] for request in artifact["requests"]))
        self.assert_sanitized(standard_output, standard_error, artifact_text, stub=stub)

    def test_untrusted_certificate_is_not_verified_and_fails(self):
        certificate = generate_self_signed_certificate(self.workspace)
        environment = {key: value for key, value in os.environ.items() if key not in ("SSL_CERT_FILE", "SSL_CERT_DIR")}
        with StubServer(self.behavior, tls_certificate=certificate) as stub:
            self.write_config(stub.base_url)
            with mock.patch.dict(os.environ, environment, clear=True):
                exit_code, standard_output, standard_error = self.run_probe()
        self.assertEqual(exit_code, 1)
        artifact, artifact_text = self.read_artifact()
        self.assertEqual(artifact["classifications"]["transport"], "https-verification-failed")
        self.assertEqual(artifact["requests"][0]["error"], "tls-verification-failed")
        self.assertFalse(any(request["tls_verified"] for request in artifact["requests"]))
        self.assertIsNone(artifact["requests"][0]["status"])
        self.assertIn("tls-verification-failed", standard_error)
        self.assert_sanitized(standard_output, standard_error, artifact_text, stub=stub)
        self.assertEqual(self.behavior.received, [])


class ConfigurationErrorTests(ProbeTestCase):
    def assert_configuration_error(self, expected_message_fragment, *extra_arguments, provider=PROVIDER_KEY):
        exit_code, standard_output, standard_error = self.run_probe(*extra_arguments, provider=provider)
        self.assertEqual(exit_code, 2)
        self.assertIn(expected_message_fragment, standard_error)
        self.assertEqual(standard_output, "")
        self.assertFalse(self.output_directory.exists())
        self.assertNotIn(TEST_CREDENTIAL, standard_error)
        self.assertEqual(self.behavior.received, [])

    def test_missing_config_file(self):
        self.assert_configuration_error("provider configuration file not found")

    def test_invalid_json(self):
        self.config_path.write_text("{not json", encoding="utf-8")
        self.assert_configuration_error("not valid JSON")

    def test_no_providers_object(self):
        self.config_path.write_text(json.dumps({"providers": []}), encoding="utf-8")
        self.assert_configuration_error("no 'providers' object")

    def test_unknown_provider_key(self):
        self.write_config("http://127.0.0.1:1/v1")
        self.assert_configuration_error(f"provider 'other' is not defined", provider="other")

    def test_empty_provider_key(self):
        self.write_config("http://127.0.0.1:1/v1")
        self.assert_configuration_error("provider key must not be empty", provider=" ")

    def test_missing_base_url(self):
        self.write_config(None)
        self.assert_configuration_error("missing or empty baseUrl")

    def test_empty_base_url(self):
        self.write_config("   ")
        self.assert_configuration_error("missing or empty baseUrl")

    def test_missing_credential(self):
        self.write_config("http://127.0.0.1:1/v1", credential=None)
        self.assert_configuration_error("missing or empty apiKey")

    def test_empty_credential(self):
        self.write_config("http://127.0.0.1:1/v1", credential="")
        self.assert_configuration_error("missing or empty apiKey")

    def test_non_string_credential(self):
        self.write_config("http://127.0.0.1:1/v1", credential=12345)
        self.assert_configuration_error("missing or empty apiKey")

    def test_http_url_without_host(self):
        self.write_config("http:///v1")
        self.assert_configuration_error("has no host")

    def test_unwritable_output_directory_is_an_explicit_error(self):
        blocked_path = self.workspace / "occupied"
        blocked_path.write_text("a file, not a directory", encoding="utf-8")
        self.output_directory = blocked_path / "evidence"
        with StubServer(self.behavior) as stub:
            self.write_config(stub.base_url)
            exit_code, standard_output, standard_error = self.run_probe()
        self.assertEqual(exit_code, 2)
        self.assertIn("could not write the evidence artifact", standard_error)
        self.assert_sanitized(standard_output, standard_error, stub=stub)

    def test_artifact_containing_the_credential_is_refused(self):
        self.behavior.server_header = f"proxy-{TEST_CREDENTIAL}"
        exit_code, standard_output, standard_error = self.run_against_stub()
        self.assertEqual(exit_code, 2)
        self.assertIn("refusing to write evidence", standard_error)
        self.assertFalse(self.output_directory.exists())
        self.assert_sanitized(standard_output, standard_error, stub=self.stub)

    def test_artifact_containing_the_endpoint_address_is_refused(self):
        with StubServer(self.behavior) as stub:
            self.behavior.server_header = f"proxy-{stub.address}"
            self.write_config(stub.base_url)
            exit_code, standard_output, standard_error = self.run_probe()
        self.assertEqual(exit_code, 2)
        self.assertFalse(self.output_directory.exists())
        self.assert_sanitized(standard_output, standard_error, stub=stub)


class ProbeRevisionTests(unittest.TestCase):
    def run_git(self, repository, *arguments):
        subprocess.run(["git", "-c", "user.name=probe-test", "-c", "user.email=probe-test@example.invalid", *arguments],
                       cwd=repository, check=True, capture_output=True)

    def test_dirty_flag_ignores_evidence_directory_only(self):
        if not shutil.which("git"):
            self.skipTest("git is required")
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory)
            self.run_git(repository, "init", "-q")
            (repository / "tracked.txt").write_text("content", encoding="utf-8")
            self.run_git(repository, "add", "tracked.txt")
            self.run_git(repository, "commit", "-q", "-m", "initial")
            with mock.patch.object(spark_probe, "REPO_ROOT", repository):
                commit, dirty = spark_probe.read_probe_revision()
                self.assertRegex(commit, r"^[0-9a-f]{40}$")
                self.assertFalse(dirty)

                evidence = repository / spark_probe.EVIDENCE_DIRECTORY_RELATIVE
                evidence.mkdir(parents=True)
                (evidence / "artifact.json").write_text("{}", encoding="utf-8")
                self.assertFalse(spark_probe.read_probe_revision()[1])

                (repository / "tracked.txt").write_text("changed", encoding="utf-8")
                self.assertTrue(spark_probe.read_probe_revision()[1])

    def test_unreadable_repository_is_reported_unknown_and_dirty(self):
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(spark_probe, "REPO_ROOT", Path(directory)):
                self.assertEqual(spark_probe.read_probe_revision(), ("unknown", True))


if __name__ == "__main__":
    unittest.main()
