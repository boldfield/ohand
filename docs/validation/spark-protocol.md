# Spark serving protocol: worker and phone-context evidence (V08a and V08b)

Worker-context values below were observed by `tools/provider-probe/spark_probe.py`. Phone-context values were observed by the maintainer on iPhone with a-Shell curl (which exposes HTTP response status codes). Both are read from committed, sanitized artifacts. Nothing here is taken from configuration, documentation or assumption.

| Field | Value |
| --- | --- |
| Evidence identifier | `spark-probe-20261008T093909Z-22c47772` |
| Artifact | [`docs/validation/evidence/spark-probe/spark-probe-20261008T093909Z-22c47772.json`](evidence/spark-probe/spark-probe-20261008T093909Z-22c47772.json) |
| Probe revision | commit `22c47772849c77b3d3a5e2b03be87098690e738e`, dirty-tree flag `false` |
| Collection time | 2026-10-08T09:39:09Z |
| Collection context | Odonian worker container, using the single provider entry named on the command line from the mounted provider configuration |
| Command | `python3 tools/provider-probe/spark_probe.py --provider <provider-key>` (the key is the explicit argument; the provider configuration file and key value are private) |
| Probe exit status | 1 (probe result `fail`: the endpoint does not enforce authentication, see below) |

The dirty-tree flag ignores `docs/validation/evidence/spark-probe/`, which is the probe's own output. The commits after `22c47772` only add the artifact and this document. The endpoint address and credential are in no artifact; the Kubernetes Secret `ohand-spark-provider` and its mount are the private reference.


## Transport

Source: evidence `spark-probe-20261008T093909Z-22c47772`, probe revision `22c47772` (clean), collected 2026-10-08T09:39:09Z.

- Configured scheme: `https`.
- All four requests completed over HTTPS with `tls_verified: true`. The probe uses the default trust store (`ssl.create_default_context()`, certificate and hostname verification on); classification `https-verified`.
- Cleartext HTTP was not observed.

## Authentication

Source: same evidence, revision and collection time.

| Request | Path | Credential sent | HTTP status | Classification |
| --- | --- | --- | --- | --- |
| `models-unauthenticated` | `GET /models` | no | 200 | `authentication-not-required` |
| `models-authenticated` | `GET /models` | yes | 200 | `openai-compatible` |
| `chat-structured` | `POST /chat/completions` | yes | 200 | `openai-compatible` |
| `chat-unauthenticated` | `POST /chat/completions` | no | 200 | `authentication-not-required` |

The endpoint returned 200 to `/models` and to a one-token chat completion without any credential. The unauthenticated successes are reported as "authentication not required", not as reachability success, and make the probe fail. The credential-bearing requests were also accepted, so these observations do not show whether the credential is checked at all.

Paths are relative to the configured base URL, which is not recorded.

## Response headers

Source: same evidence, revision and collection time. Allowlisted headers only (`server`, `content-type`, `date`).

- `GET /models`: `content-type: application/json` and `date`; no `server` header was returned.
- `POST /chat/completions`: `content-type: application/json`, `date`, and `server: nginx/1.27.5`.

## Compatibility matrix

Source: the `models-authenticated` response in the same evidence, revision and collection time. The response had the expected OpenAI-compatible structure: a JSON object with a non-empty `data` list whose entries each have a non-empty string `id`. Rows are exactly the identifiers the endpoint listed, in the order returned. The probe sends a chat request for only one model (the first listed), so the other rows are not probed.

| Model identifier (from `/models`) | `object` reported | Chat completion probed | Structured JSON response probed |
| --- | --- | --- | --- |
| `deepseek-flash-iq3:latest` | `model` | yes: OpenAI-compatible chat response | yes: passed |
| `qwen3.8:27b` | `model` | not probed | not probed |
| `qwen3.5:122b` | `model` | not probed | not probed |
| `gpt-oss:120b` | `model` | not probed | not probed |
| `gpt-oss:20b` | `model` | not probed | not probed |
| `glm-5.3-flash` | `model` | not probed | not probed |

## Structured response

Source: same evidence, revision and collection time.

The probe requested a JSON object through `response_format` of type `json_schema` (strict, no additional properties) with exactly two keys, `status` equal to `"ok"` and `count` equal to the integer 3. For `deepseek-flash-iq3:latest` the returned `message.content` parsed as a JSON object with exactly the keys `count` and `status`, and both values matched, so the structured-response probe passed. The probe records the verdict and the observed keys, not the model output.

## What this does not establish (worker-context V08a)

- Behavior of the five models that were not sent a chat request.
- Whether the credential is validated: the endpoint accepted requests with and without it.
- Any property not listed in the artifact, such as latency, rate limits or streaming.

---

# Phone-context reachability: maintainer-collected observations (V08b)

Every value below was observed by the maintainer on the configured iPhone and is read from one committed, sanitized artifact. Nothing here is taken from configuration, documentation or assumption.

| Field | Value |
| --- | --- |
| Evidence identifier | `spark-phone-2026-10-08`, revision 2 |
| Artifact | [`docs/validation/evidence/spark-phone/2026-10-08-phone-reachability.json`](evidence/spark-phone/2026-10-08-phone-reachability.json) |
| Device | iPhone 16 Pro |
| iOS version | 26.6.2 |
| Client | a-Shell curl (observes and records HTTP status code via `curl`'s `http_code` variable) |
| Path to endpoint | Tailscale and split-horizon DNS; the configured base URL hostname resolves to the private reverse proxy on the home LAN and to a public web forwarder elsewhere |
| Collection clock accuracy | Phone's local clock in the screenshots, converted from UTC-7 timezone; accurate to about one minute |

## Phone-context: Wi-Fi with configured hostname (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, revision 2, Wi-Fi attempts to configured hostname, collected 2026-10-08T16:34:00Z.

| Request | Credential sent | HTTP status (curl observed) | Scheme | Certificate |
| --- | --- | --- | --- | --- |
| `GET /v1/models` | No | 200 | https | Verified |
| `GET /v1/models` | Yes | 200 | https | Verified |

## Phone-context: cellular with configured hostname (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, revision 2, cellular attempts to configured hostname, collected 2026-10-08T16:35:00Z.

| Request | Credential sent | HTTP status (curl observed) | Scheme | Certificate | Note |
| --- | --- | --- | --- | --- | --- |
| `GET /v1/models` | No | 302 | https | Verified | Redirect to tailnet hostname |
| `GET /v1/models` | Yes | 302 | https | Verified | Redirect to tailnet hostname; credential does not change response |

## Phone-context: cellular with tailnet hostname (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, revision 2, cellular attempts to tailnet hostname, collected 2026-10-08T16:39:00Z.

| Request | Credential sent | HTTP status (curl observed) | Scheme | Certificate |
| --- | --- | --- | --- | --- |
| `GET /v1/models` | No | 200 | https | Verified |
| `GET /v1/models` | Yes | 200 | https | Verified |

## Phone-context: cellular with configured hostname followed by redirect (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, revision 2, cellular attempt with `curl -L`, collected 2026-10-08T16:39:00Z.

| Request | Credential sent | HTTP status (curl observed) | Scheme | Certificate | Note |
| --- | --- | --- | --- | --- | --- |
| `GET /v1/models` (with `-L` redirect) | No | 200 | https | Verified on both hops | Public forwarder redirected to tailnet hostname; curl followed the redirect |

## Phone-context: Wi-Fi with tailnet hostname (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, revision 2, Wi-Fi attempt to tailnet hostname, collected 2026-10-08T16:39:00Z.

| Request | Credential sent | HTTP status (curl observed) | Scheme | Certificate |
| --- | --- | --- | --- | --- |
| `GET /v1/models` | No | 200 | https | Verified |

## Model list observed on phone

Source: evidence `spark-phone-2026-10-08`, revision 1, Safari rendering of `GET /v1/models` on Wi-Fi and cellular at 2026-10-08T06:42:00Z; identical in both contexts. This is from the first collection attempt and predates the curl observations.

| Model ID |
| --- |
| `deepseek-flash-iq3:latest` |
| `qwen3.8:27b` |
| `qwen3.5:122b` |
| `gpt-oss:120b` |
| `gpt-oss:20b` |
| `glm-5.3-flash` |

## Compatibility: V08a (worker) and V08b (phone) observations

### Recorded observations

**Transport**: Both worker-context (V08a) and phone-context (V08b) confirm HTTPS with certificate validation on all observed requests.
- Worker-context: Probe classification `https-verified` (default trust store, verified certificates on all four requests).
- Phone-context: the artifact records 8 attempts, each over https with a verified certificate: Wi-Fi with configured hostname (both credentials), Wi-Fi with tailnet hostname (unauthenticated only), cellular with configured hostname (both credentials, ending in 302), cellular with tailnet hostname (both credentials), and cellular with configured hostname using `curl -L` (unauthenticated; verified on both hops).

**Unauthenticated reachability**: Phone-context attempted unauthenticated `GET /v1/models` on Wi-Fi and cellular over Tailscale.
- Wi-Fi with configured hostname: HTTP 200 (direct).
- Cellular with configured hostname (direct curl): HTTP 302 (redirect to tailnet hostname).
- Cellular with configured hostname (curl -L): HTTP 200 (after following redirect to tailnet).
- Cellular with tailnet hostname: HTTP 200 (direct).
- Wi-Fi with tailnet hostname: HTTP 200 (direct).
- Worker-context: Unauthenticated `GET /models` received HTTP 200.

**Authenticated reachability**: Phone-context also attempted authenticated `GET /v1/models` (with bearer credential) on Wi-Fi and cellular.
- Wi-Fi with configured hostname: HTTP 200 (direct).
- Cellular with configured hostname: HTTP 302 (same redirect as unauthenticated; credential does not change response).
- Cellular with tailnet hostname: HTTP 200 (direct).
- Worker-context: Authenticated `GET /models` received HTTP 200.

**Authentication behavior agreement**: Both sources observe that the endpoint returns identical HTTP status to credentialed and non-credentialed requests.
- Worker-context: Both `models-unauthenticated` and `models-authenticated` returned 200; endpoint does not enforce authentication.
- Phone-context: Unauthenticated and credentialed `GET /v1/models` returned identical statuses in every paired attempt (200 via the configured hostname on Wi-Fi and via the tailnet hostname on cellular, 302 via the configured hostname on cellular); the Wi-Fi tailnet attempt had no credentialed pair; endpoint does not require credential.

**Model list agreement**: Both worker-context and phone-context artifacts list six identical model identifiers.
- Worker-context: `models-authenticated` response body.
- Phone-context: Rendered response body from Safari on Wi-Fi and cellular (identical list).

### Verification gaps for V09

These facts are recorded as the exact verified compatibility boundary:

1. **Collection method and paths differ**: Worker-context V08a used `python3 tools/provider-probe/spark_probe.py` on an Odonian worker. Phone-context V08b used a-Shell curl on iPhone with Tailscale. Different tools, different network paths:
   - Worker-context: the V08a artifact records `collection_context: odonian-worker` and the configured scheme; the worker network path is not recorded (it does not say whether the worker reached the endpoint directly, through the reverse proxy or through the forwarder).
   - Phone-context on-LAN (Wi-Fi): either direct via configured hostname or via Tailscale MagicDNS (tailnet hostname); both reach the endpoint directly.
   - Phone-context off-LAN (cellular): via Tailscale, using either the configured hostname (which resolves to a public web forwarder) or the tailnet hostname (which reaches the endpoint directly).

2. **Chat request not probed on phone**: Worker-context V08a probed `POST /chat/completions` with and without credentials; both returned 200. Phone-context V08b did not attempt `POST /chat/completions`. Authentication behavior observed on `/models` does not necessarily apply to `/chat/completions`.

3. **Configuration/endpoint model mismatch**: The maintainer's provider configuration lists seven models; the endpoint serves six. The missing model is the IQ3_XXS GLM variant. Whether this gap is a deployment state, ephemeral, or intended is unknown and matters to endpoint-readiness assessment in V09.

4. **Critical POST redirect incompatibility on configured hostname off-LAN**: The artifact records that off the LAN, the configured hostname resolves to a public web forwarder that answers 302 to both `GET /v1/models` requests and to `POST` requests (coordinator observation from the public address on 2026-10-08). The artifact names Foundation URLSession as an example of a client that converts a redirected POST to GET on a 302. Therefore, a client of that kind sending `POST /chat/completions` to the configured hostname off-LAN will receive a 302 and convert it to GET, failing to complete the chat request. V09 must either use the tailnet hostname as the phone-context base URL for off-LAN communication, or require that the forwarder emit HTTP 307 or 308 (which preserve the request method), which it does not do today.

5. **V08a probe result is fail**: Worker-context V08a reported exit status 1 (`fail`) because the endpoint does not enforce authentication. This is a protocol violation from the probe's perspective. Whether authentication enforcement is required for V09 readiness is a V09 decision.

6. **Off-LAN communication requires Tailscale**: Phone-context observations show that off the home LAN (cellular network), only the Tailscale-connected tailnet hostname reaches the endpoint directly. The configured hostname goes to a public forwarder with the POST redirect incompatibility in gap 4. Nothing here tests the behavior of off-LAN requests with Tailscale disconnected.

7. **Collection clock accuracy**: Phone-context collection times come from the phone's local clock in the screenshots, converted from UTC-7, accurate to approximately one minute.

**Status**: PARTIALLY VERIFIED. V08a and V08b artifacts exist and agree on:
- **Transport**: HTTPS with certificate validation on all observed requests.
- **Model list identity**: Both sources observe the same six models.
- **Authentication behavior on `/models`**: Neither source observed authentication enforcement.
- **On-LAN reachability**: Both unauthenticated and authenticated requests succeed via the configured hostname on Wi-Fi. The tailnet hostname on Wi-Fi was observed unauthenticated only (one attempt, HTTP 200); no authenticated tailnet request on Wi-Fi was recorded.

Phone-context reachability off-LAN (cellular) succeeds only via the tailnet hostname or via the configured hostname with `curl -L` (which follows the redirect to tailnet). Direct requests to the configured hostname off-LAN receive HTTP 302, not direct endpoint access. The exact compatibility boundary is recorded in gap 4: a client that converts 302 POST to GET (like Foundation URLSession) cannot complete chat completions through the configured hostname off-LAN.

Unobserved: chat-path behavior on phone, whether credential is validated by the endpoint, endpoint behavior with Tailscale disconnected, and implications of the model configuration mismatch. V09 must evaluate whether these gaps are acceptable for the endpoint's intended use and decide the phone-context base URL (tailnet hostname vs. forwarder configuration).

## V09 adapter smoke run

The self-hosted adapter (`core/src/providers/self_hosted/`) was run against the configured endpoint from the Odonian worker on 2026-10-09 with its own request: the interpretation prompt and the full `output_schema()` as a strict `json_schema`, three synthetic captures. The transport was a `curl` subprocess standing in for the native HTTPS transport. Re-run with `OHAND_SMOKE_EVIDENCE_DIR=docs/validation/evidence/spark-adapter-smoke cargo test --test providers_self_hosted live_endpoint_smoke -- --ignored`.

| Artifact (`docs/validation/evidence/spark-adapter-smoke/`) | Adapter revision | `max_tokens` | Result |
| --- | --- | --- | --- |
| `self-hosted-adapter-smoke-20261009T111140Z-7be0c91a.json` | `7be0c91a` | 1024 | pass, 3/3; superseded, its raw exchange was not preserved |
| `self-hosted-adapter-smoke-20261009T145407Z-d1243e1f.json` | `d1243e1f` | 1024 | fail: reminder capture `finish_reason` `length`, empty content (reasoning used the budget) |
| `self-hosted-adapter-smoke-20261009T145540Z-d1243e1f.json` | `d1243e1f` | 1024 | fail: same reminder capture, same outcome |
| `self-hosted-adapter-smoke-20261009T145738Z-e45f02b4.json` | `e45f02b4cdec971bdd0e16cd162af60afe8a4a16` | 4096 | pass, 3/3: 200, assistant, `finish_reason` `stop`, each reply mapped to a validated proposal. The cited evidence for `Supported`. |

The sanitized artifacts carry no endpoint, credential, captured text or model output text. Each also carries `private_raw_evidence`: the run id, file name, size and SHA-256 of the raw exchange (request and response bodies, no endpoint or credential), kept outside Git in the collecting worker's private evidence directory (`OHAND_PRIVATE_EVIDENCE_DIR`, default `~/.ohand-private-evidence`, mode 0600 in a 0700 directory), the same convention as `docs/validation/signing.md`. The harness refuses a private directory inside the repository. The raw file for the cited run was written on the worker that collected it; recompute the SHA-256 of `<file_name>` there to audit it. No raw files exist for the `7be0c91a` run. The leak check on the raw file looks for the endpoint everywhere but for the credential only in request bodies, because the configured credential is a short placeholder word the model's own output can contain.

Observed latency reached about 69 seconds for one call, so a profile for this endpoint needs a timeout above the 30 seconds used in unit tests. No smoke capture produced `reminder_proposal` or `session_topic_proposal`, so those remain unverified.
