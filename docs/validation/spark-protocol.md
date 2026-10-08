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
| Path to endpoint | Tailscale; endpoint is private-network only (not publicly resolvable) |
| Collection clock accuracy | Coordinator session clock converted from UTC-7 timezone; accurate to about one minute |

## Phone-context: Wi-Fi (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, Wi-Fi attempts, collected 2026-10-08T16:34:00Z.

| Request | Credential sent | HTTP status (curl observed) | Scheme | Certificate |
| --- | --- | --- | --- | --- |
| `GET /v1/models` | No | 200 | https | Verified |
| `GET /v1/models` | Yes | 200 | https | Verified |

## Phone-context: cellular with configured hostname (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, cellular attempts to configured hostname, collected 2026-10-08T16:35:00Z.

| Request | Credential sent | HTTP status (curl observed) | Scheme | Certificate | Note |
| --- | --- | --- | --- | --- | --- |
| `GET /v1/models` | No | 302 | https | Verified | Redirect to tailnet hostname |
| `GET /v1/models` | Yes | 302 | https | Verified | Redirect to tailnet hostname; credential does not change response |

## Phone-context: cellular with tailnet hostname (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, cellular attempts to tailnet hostname, collected 2026-10-08T16:39:00Z.

| Request | Credential sent | HTTP status (curl observed) | Scheme | Certificate |
| --- | --- | --- | --- | --- |
| `GET /v1/models` | No | 200 | https | Verified |
| `GET /v1/models` | Yes | 200 | https | Verified |

## Model list observed on phone

Source: evidence `spark-phone-2026-10-08`, Safari rendering of `GET /v1/models` on Wi-Fi and cellular; identical in both contexts.

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
- Phone-context: Certificate verified on Wi-Fi (both credentials), cellular configured hostname (both credentials), and cellular tailnet hostname (both credentials); all 8 HTTP exchanges show verified TLS.

**Unauthenticated reachability**: Phone-context attempted unauthenticated `GET /v1/models` on Wi-Fi and cellular over Tailscale.
- Wi-Fi with configured hostname: HTTP 200.
- Cellular with configured hostname: HTTP 302 (redirect to tailnet).
- Cellular with tailnet hostname: HTTP 200.
- Worker-context: Unauthenticated `GET /models` received HTTP 200.

**Authenticated reachability**: Phone-context also attempted authenticated `GET /v1/models` (with bearer credential) on Wi-Fi and cellular.
- Wi-Fi with configured hostname: HTTP 200.
- Cellular with configured hostname: HTTP 302 (same redirect, credential does not change response).
- Cellular with tailnet hostname: HTTP 200.
- Worker-context: Authenticated `GET /models` received HTTP 200.

**Authentication behavior agreement**: Both sources observe that the endpoint returns identical HTTP status to credentialed and non-credentialed requests.
- Worker-context: Both `models-unauthenticated` and `models-authenticated` returned 200; endpoint does not enforce authentication.
- Phone-context: Unauthenticated and credentialed `GET /v1/models` returned identical statuses (200 on home LAN and tailnet, 302 on public forwarder); endpoint does not require credential.

**Model list agreement**: Both worker-context and phone-context artifacts list six identical model identifiers.
- Worker-context: `models-authenticated` response body.
- Phone-context: Rendered response body from Safari on Wi-Fi and cellular (identical list).

### Verification gaps for V09

These facts are recorded as the exact verified compatibility boundary:

1. **Collection method differs**: Worker-context V08a used `python3 tools/provider-probe/spark_probe.py` on an Odonian worker. Phone-context V08b used a-Shell curl on iPhone with Tailscale. Different tools, different network paths, and different endpoints (worker-context is the configured base URL from Odonian Secrets; phone-context is same base URL via Tailscale from a home LAN).

2. **Chat request not probed on phone**: Worker-context V08a probed `POST /chat/completions` with and without credentials; both returned 200. Phone-context V08b did not attempt `POST /chat/completions`. Authentication behavior observed on `/models` does not necessarily apply to `/chat/completions`.

3. **Configuration/endpoint model mismatch**: The maintainer's provider configuration lists seven models; the endpoint serves six. The missing model is the IQ3_XXS GLM variant. Whether this gap is a deployment state, ephemeral, or intended is unknown and matters to endpoint-readiness assessment in V09.

4. **Public forwarder behavior on cellular**: On cellular, the configured hostname resolves to a public web forwarder (not in the artifact); the forwarder answers 302 to both unauthenticated and credentialed `GET /v1/models`. The forwarder also redirects `POST` requests (coordinator observation). Whether the public forwarder endpoint is considered part of the Spark protocol contract or is out of scope is a V09 decision.

5. **V08a probe result is fail**: Worker-context V08a reported exit status 1 (`fail`) because the endpoint does not enforce authentication. This is a protocol violation from the probe's perspective. Whether authentication enforcement is required for V09 readiness is a V09 decision.

6. **Collection clock accuracy**: Phone-context collection times are accurate to approximately one minute (coordinator session clock converted from UTC-7), not subsecond.

**Status**: PARTIALLY VERIFIED. V08a and V08b artifacts exist and agree on transport (HTTPS with certificate validation), unauthenticated reachability (both succeed), authenticated reachability (both succeed), and model list identity. Both sources confirm the endpoint does not enforce authentication. Unobserved: chat-path behavior on phone, whether credential is validated by the endpoint, and implications of the model configuration mismatch. V09 must evaluate whether these gaps are acceptable for the endpoint's intended use.
