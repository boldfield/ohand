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

## Phone-context reachability is unverified

These observations come from the Odonian worker network only. Whether the maintainer's iPhone can reach the endpoint over Tailscale (Wi-Fi or cellular) is **unverified and owned by V08b**. Worker reachability must not be used as phone reachability, and V08 is not complete until V08b records the phone-context evidence.

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

- Phone-context reachability (V08b), as stated above.
- Behavior of the five models that were not sent a chat request.
- Whether the credential is validated: the endpoint accepted requests with and without it.
- Any property not listed in the artifact, such as latency, rate limits or streaming.
