# M1 external execution prerequisites

## Verified provider-access update

At 2026-10-08T04:11:22Z, the coordinator verified ArgoCD Synced/Healthy at manifests revision `17104178df5a24fb372747cffd534e943cd8344e`, deployment generation 18 observed, and all four configured workers ready. In every actual execution container, UID 1000 could read and parse `/etc/ohand-provider/models.json`; the mount and kernel filesystem were read-only, with observed root:1000 mode 0440. No credentials/configuration contents were printed and no pods were restarted.

[Immutable sanitized operator evidence](https://github.com/boldfield/manifests/blob/07515bf53f7b2d96bf9b596f0f88de78f99f2fac/docs/validation/ohand-provider-access.md) records the individual container observations and private raw-evidence reference. This clears the missing worker-configuration-access prerequisite. Infrastructure task B still needs its PR cleanup and independent review; V08 can now run its authorized synthetic endpoint probes using that file. Endpoint behavior, successful authentication and phone-context reachability are not established by this access check. Do not ask the execution worker to install kubectl or obtain cluster-admin access to repeat the operator observation.

The original blocker description below is historical and superseded for worker Secret mounting by this verified update. Apple enrollment/signing is unchanged by this observation.

## Current blockers

P08 (`7d4836ee-0236-4f17-9f21-bc3ec1d3d7e0`) requires real Apple signing and device-install evidence. Developer enrollment is still pending, as corrected by the maintainer on 2026-10-07. This Mac currently selects Command Line Tools, has no valid code-signing identity and has no installed provisioning profiles. Do not unblock P08 based on enrollment alone. The existing PR #27 also has unresolved review findings; preserve that work and fix those findings before resubmission. Do not claim a signed installation from simulator results or fixture tests.

V08 (`939fe62e-5074-45e5-ad67-156834129c13`) requires actual provider and phone-context evidence. The maintainer authorized the provider configuration from their Pi models file to be stored in Kubernetes. Secret `ohand-spark-provider` exists in namespace `odonian-fleet`, with key `models.json`. The existing worker deployment has no mount for it. Secret creation is not access verification. No endpoint, credential, model configuration contents or device identifiers belong in this public repository.

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

V08 should read `/etc/ohand-provider/models.json` (or the non-secret `OHAND_PROVIDER_CONFIG_PATH`) inside the worker once task B has verified deployment. Treat its protocol declaration as configuration to test, not proof of compatibility. Use authorized bounded synthetic requests, keep credentials/addresses out of outputs, and distinguish worker/Mac reachability from the intended iPhone network context. Missing phone-context evidence can still block completion after the server-side probe succeeds.
