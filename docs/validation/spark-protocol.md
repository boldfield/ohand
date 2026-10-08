# Spark serving protocol: worker-context observations (V08a)

Every value below was observed by `tools/provider-probe/spark_probe.py` and is read from one committed, sanitized artifact. Nothing here is taken from configuration, documentation or assumption.

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

## Phone-context reachability recorded by V08b

These worker-context observations come from the Odonian worker network only. Phone-context evidence (maintainer's iPhone over Tailscale, Wi-Fi and cellular) is recorded in the V08b section below.

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

## What this does not establish

- Behavior of the five models that were not sent a chat request.
- Whether the credential is validated: the endpoint accepted requests with and without it.
- Any property not listed in the artifact, such as latency, rate limits or streaming.

---

# Phone-context reachability: maintainer-collected observations (V08b)

Every value below was observed by the maintainer on the configured iPhone and is read from one committed, sanitized artifact. Nothing here is taken from configuration, documentation or assumption.

| Field | Value |
| --- | --- |
| Evidence identifier | `spark-phone-2026-10-08` |
| Artifact | [`docs/validation/evidence/spark-phone/2026-10-08-phone-reachability.json`](evidence/spark-phone/2026-10-08-phone-reachability.json) |
| Device | iPhone 16 Pro |
| iOS version | 26.6.2 |
| Client | Safari, plain navigation to the models path of the configured base URL |
| Path to endpoint | Tailscale; endpoint is private-network only (not publicly resolvable) |
| Credential sent | No (unauthenticated requests only; a browser cannot attach a bearer header) |
| Collection clock accuracy | Coordinator session clock when results were reported; accurate to about one minute |

## Phone-context: Wi-Fi (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, Wi-Fi attempt, reported 2026-10-08T06:42:00Z (±~1 min).

| Field | Value |
| --- | --- |
| Network | Wi-Fi |
| Tailscale connected | Yes |
| Request type | Unauthenticated GET `/models` |
| HTTP status | 200 (inferred from rendered JSON response; Safari does not display numeric status) |
| Scheme | https |
| Certificate | Validated, no Safari warning |
| Models list returned | 6 |

## Phone-context: cellular (Tailscale connected)

Source: evidence `spark-phone-2026-10-08`, cellular attempt, reported 2026-10-08T06:44:30Z (±~1 min).

| Field | Value |
| --- | --- |
| Network | cellular |
| Tailscale connected | Yes |
| Request type | Unauthenticated GET `/models` |
| HTTP status | 200 (inferred from rendered JSON response; Safari does not display numeric status) |
| Scheme | https |
| Certificate | Validated, no Safari warning |
| Models list returned | 6 |

## Compatibility: V08a (worker) and V08b (phone) observations

### Recorded observations

**Transport**: Both worker-context (V08a) and phone-context (V08b) confirm HTTPS with certificate validation.
- Worker: Probe classification `https-verified` (default trust store, verified certificates on all four requests).
- Phone: Certificate validated on both Wi-Fi and cellular; no Safari warning.

**Unauthenticated reachability**: Phone-context attempted unauthenticated `GET /models` on both Wi-Fi and cellular over Tailscale.
- Result: Both attempts succeeded with rendered response containing six model identifiers.
- Worker-context: Unauthenticated `GET /models` received HTTP 200 with six models listed in response body.
- Phone-context: HTTP status inferred from rendered JSON (Safari does not expose numeric status); certificate chain validated.

**Model list**: Both worker-context and phone-context artifacts list the same six model identifiers in the same order.
- Worker-context: Listed by `models-authenticated` response in the artifact.
- Phone-context: Listed by rendered JSON body on both Wi-Fi and cellular; no models were probed for chat completion on the phone.

### Verification gaps for V09

These facts do not cross evidence sources or satisfy authentication-behavior agreement, and are recorded as the exact verified compatibility boundary:

1. **Authenticated phone-context missing**: V08b collected only unauthenticated `GET /models` on the phone. No authenticated request (either `GET /models` with credential or `POST /chat/completions` with credential) was attempted on the phone. This is a browser limitation, not endpoint behavior. An authenticated phone-context request remains unobserved.

2. **HTTP status method differs**: Worker-context V08a (probe) recorded actual HTTP response status codes. Phone-context V08b inferred status from rendered JSON body, which does not establish numeric status 200 with certainty. A response body does not prove the response status code.

3. **Collection clock accuracy**: Phone-context times (2026-10-08T06:42:00Z and 2026-10-08T06:44:30Z) come from the coordinator's session clock when results were reported; accuracy is approximately one minute, not subsecond.

4. **Configuration/endpoint model mismatch**: The maintainer's provider configuration lists seven models; the endpoint serves six. The missing model is the IQ3_XXS GLM variant. Whether this gap is a deployment state, ephemeral, or intended is unknown and matters to endpoint-readiness assessment in V09.

5. **V08a probe result is fail**: Worker-context V08a's own reported result is `fail` (exit status 1), because the endpoint does not enforce authentication on any tested path. Whether the endpoint validates the credential when one is provided remains unknown. The absence of a credential validation failure does not establish credential acceptance.

6. **Authenticated V08a paths not probed on phone**: The worker-context V08a probed `POST /chat/completions` with and without credentials and received 200 to both. The phone-context V08b did not attempt `POST /chat/completions` in any form (authenticated or unauthenticated).

**Status**: V08a and V08b artifacts exist. Phone-context reachability (unauthenticated only, on Wi-Fi and cellular via Tailscale) is now recorded. Transport (HTTPS + certificate validation) is confirmed by both sources. Model list agreement is confirmed. Gaps in authentication behavior observation and HTTP status method are recorded above for V09 to evaluate.
