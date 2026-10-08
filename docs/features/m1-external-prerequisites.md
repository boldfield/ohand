# M1 external execution prerequisites

## Verified provider-access update

At 2026-10-08T04:11:22Z, the coordinator verified ArgoCD Synced/Healthy at manifests revision `17104178df5a24fb372747cffd534e943cd8344e`, deployment generation 18 observed, and all four configured workers ready. In every actual execution container, UID 1000 could read and parse `/etc/ohand-provider/models.json`; the mount and kernel filesystem were read-only, with observed root:1000 mode 0440. No credentials/configuration contents were printed and no pods were restarted.

[Immutable sanitized operator evidence](https://github.com/boldfield/manifests/blob/07515bf53f7b2d96bf9b596f0f88de78f99f2fac/docs/validation/ohand-provider-access.md) records the individual container observations and private raw-evidence reference. This clears the missing worker-configuration-access prerequisite. Infrastructure task B still needs its PR cleanup and independent review; V08 can now run its authorized synthetic endpoint probes using that file. Endpoint behavior, successful authentication and phone-context reachability are not established by this access check. Do not ask the execution worker to install kubectl or obtain cluster-admin access to repeat the operator observation.

The original blocker description below is historical and superseded for worker Secret mounting by this verified update. Apple enrollment/signing is unchanged by this observation.

## Current blockers

P08 was split on 2026-10-08 after two rejected review rounds on PR #27; see the replacement map in [the task refinement overlay](m1-task-refinement.md). The original task `7d4836ee-0236-4f17-9f21-bc3ec1d3d7e0` is retired. P08a (signing tooling with stubbed Apple tools) is executable on the Linux fleet now. P08b (signed build and device install) starts blocked on the maintainer evidence described in "Signed-build evidence for P08b" below. Apple Developer Program enrollment completed on 2026-10-08. Later that day the maintainer's Mac had Xcode 26.6 selected, one valid Apple Development identity, one automatically managed development profile covering the trial iPhone (observed expiry 2027-10-08), and the signed BridgeProbe installed on the device through Xcode. Steps 1 to 3 below are therefore done; steps 4 to 6 wait on the P08a tooling. Do not claim a signed installation from simulator results or fixture tests.

V08 was split on 2026-10-08 after two rejected review rounds on PR #60; see the replacement map in [the task refinement overlay](m1-task-refinement.md). The original task `939fe62e-5074-45e5-ad67-156834129c13` is retired. V08a (server-side probe from the worker) is executable now that worker configuration access is verified. V08b (phone-context reachability) starts blocked on the maintainer evidence described in "Phone-context evidence for V08b" below. The maintainer authorized the provider configuration from their Pi models file to be stored in Kubernetes. Secret `ohand-spark-provider` exists in namespace `odonian-fleet`, with key `models.json`, and is mounted read-only in the workers. No endpoint, credential, model configuration contents or device identifiers belong in this public repository.

## Infrastructure task A: expose the existing Secret to execution workers

Implementation belongs to boldfield/manifests, whose ArgoCD configuration owns the fleet. At the inspected baseline, relevant locations are `cp/odonian-fleet/worker-deployment.yaml:71` (mounts), `:82` (volumes), and `cp/odonian-fleet/README.md:7` (out-of-band secrets and rollout constraints).

Add a read-only Secret volume for the existing `ohand-spark-provider`, selecting only `models.json`, available at `/etc/ohand-provider/models.json`. A non-secret `OHAND_PROVIDER_CONFIG_PATH` value may identify that path. It must be readable by the existing non-root worker identity, not world-readable. Keep Secret values outside Git, environment variables, CLI arguments and logs. Do not copy this into agent authentication files. Do not grant Kubernetes Secret-list permissions or change unrelated images, service accounts, replicas, selectors or reviewer/merger access.

Acceptance: rendered manifests reference the correct existing Secret/key, mount read-only with compatible restrictive permissions, and preserve existing fleet settings. Document the out-of-band prerequisite and stable file reference. Run repository checks and inspect the rendered resource. Missing optional integration configuration must not silently change provider destinations; if the Secret reference is optional to protect unrelated fleet operation, V08 must explicitly detect an absent file and remain blocked.

Deployment changes replace ephemeral worker pods. The merge/deployment operator must first arrange an idle/drained fleet using supported controls and preserve active work; do not restart busy workers, scale them down or delete pods to force this change through. Preserve this repository's independent review and human merge gates. A merged configuration is not proof of a completed rollout.

## Infrastructure task B: verify the deployed worker access

This follows task A after its reviewed change is merged and safely deployed. This is an operational evidence task, not an excuse to expand the credential mount or rebuild the fleet. Own `docs/validation/ohand-provider-access.md` in boldfield/manifests for sanitized evidence only.

Verify ArgoCD sync/health and expected worker readiness without exposing Secrets. From the execution container under its actual UID, confirm the expected file is readable, parses as provider configuration and is on a read-only mount. Emit only pass/fail, resource revision, timestamp and counts where needed; do not print credential values, endpoint addresses, configuration contents or hashes of secret material. Report only evidence actually observed. Preserve private raw evidence outside Git with a sanitized reference.

Acceptance: record exact deployed manifest revision and collection time, read-only/readability/parse checks, and healthy expected replicas. If safe rollout or Kubernetes access is unavailable, block with that precise limitation rather than claiming success. This task does not certify endpoint behavior or phone reachability; those remain V08 requirements. After successful deployment verification, the coordinator can resume V08 with this file reference, retaining all original provider and phone evidence criteria.

## Consumer instructions

V08a reads `/etc/ohand-provider/models.json` (or the non-secret `OHAND_PROVIDER_CONFIG_PATH`) inside the worker. Treat its protocol declaration as configuration to test, not proof of compatibility. Use authorized bounded synthetic requests and keep credentials and addresses out of every output, including the committed sanitized evidence artifact. The endpoint may be served over cleartext HTTP on the private network; record the configured scheme as observed and never report TLS that was not negotiated and verified. Worker, Mac or simulator reachability is not phone-context evidence.

## Signed-build evidence for P08b

Only the maintainer can produce this, on a Mac with the pinned Xcode and the iPhone attached. Steps, in order:

1. Install the Xcode version pinned in `ios/project.yml` (26.6 since F08 landed; 16.4 cannot install on an iOS 26 device), select it with `xcode-select`, and accept the license. Command Line Tools alone cannot sign or talk to a device.
2. In Xcode, add the Apple ID that holds the enrolled team under Settings, Accounts. Let Xcode create the Apple Development certificate. Confirm with `security find-identity -v -p codesigning` that one valid identity exists.
3. On the iPhone, enable Developer Mode, connect it by cable, trust the Mac, and let Xcode register the device with the team. Automatic signing on the probe target then produces a development profile that covers the device. Export that profile, or note its name and UUID, for the P08a tooling.
4. Run the P08a tooling with the identity, profile and device supplied through its documented environment inputs, never on the command line, and let it build the signed probe, install it, and write its evidence record.
5. Commit the sanitized record under `docs/validation/evidence/apple-signing/` named with the collection date. It carries the build revision, collection time, the content-free build and device identifiers the tooling emits, and the profile's observed expiry date. It must not carry the team identifier, certificate serial, raw device UDID, profile contents, or any hash of them. Keep the tooling's raw private evidence outside Git with a reference in the record.
6. Unblock P08b with a transition note naming that file.

The paid-account development profile is expected to be valid for about a year, which covers the two-week trial, but P08b records the observed expiry rather than this expectation.

## Phone-context evidence for V08b

The intended phone context is the maintainer's iPhone reaching the Spark endpoint over Tailscale. The endpoint is not publicly exposed, and the worker network is not the phone network, so this evidence can only be collected by the maintainer. It is an external input in the same sense as Apple enrollment: a task that lacks it blocks with that exact reason and does not substitute another network context.

Collection procedure, performed by the maintainer on the iPhone with Tailscale connected, once on Wi-Fi and once on cellular:

1. Send an unauthenticated GET to the models path of the configured base URL and record the HTTP status.
2. Send the same request with the bearer credential and record the HTTP status and the number of model identifiers returned.
3. Record the URL scheme used and, for HTTPS, whether the certificate validated without a warning or override.
4. Record the ISO 8601 collection time, the network type (Wi-Fi or cellular), that Tailscale was connected, the iOS version and the client used (for example an iOS Shortcut or a terminal app). Do not record the SSID, carrier, Tailscale node names, the endpoint address, the credential, or any hash of them.

If the V08a server-side artifact shows that the endpoint enforces no authentication, the phone record covers the unauthenticated request only and says so; a browser cannot attach a bearer header, and there is nothing for it to prove. Commit the sanitized record as a JSON or Markdown file under `docs/validation/evidence/spark-phone/` on `main`, named with the collection date, then unblock V08b with a transition note naming that file. If a request fails, record the failure as observed; a failed phone-context probe is valid evidence and defines the compatibility boundary that V09 must respect.

Observed on 2026-10-08 (phone evidence revision 2): the configured hostname is split-horizon. On the home LAN it reaches the endpoint directly; elsewhere it resolves to a public web forwarder that answers 302 to the node's Tailscale MagicDNS name, for POST as well as GET. A client that follows a 302 by switching to GET cannot complete chat completions through the configured hostname off-LAN. V09 must either use the tailnet hostname as the phone-context base URL, or the forwarder must emit 307/308. The tailnet name is configuration and stays out of this repository.

Open design question raised by this path: the V05a transport contract forbids cleartext and certificate bypass. If the Spark endpoint is served over plain HTTP inside the Tailscale network, V09 cannot use it through V05a without an explicit, reviewed allowance for private-interface cleartext, or the endpoint must gain TLS (for example through Tailscale's certificate serving). V08a and V08b must record the scheme so this decision is made on evidence.
