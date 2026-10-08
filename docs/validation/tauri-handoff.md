# Native-to-Tauri handoff and management accessibility (P07)

P07 probes the handoff from the native capture entry (P02) to the candidate Tauri management shell (P06). It connects using stable identifiers and rejects unknown or malicious routes. This document records what the probe implements and what automated checks assert. Device-specific accessibility evidence is not measured in simulator results.

## What exists

| Path | Role |
| --- | --- |
| `probes/tauri-handoff/src/lib.rs` | `HandoffValidator` validates handoff URLs, rejects unknown routes, and extracts the capture ID. `HandoffRoute` and `HandoffRequest` types define the protocol. |
| `probes/tauri-handoff/Cargo.toml` | Library manifest with minimal dependencies (url, uuid, chrono). |
| `probes/tauri-handoff/Sources/HandoffCore.swift` | Swift equivalent for unit tests and potential native integration. |
| `probes/tauri-handoff/tests/HandoffValidatorTests.swift` | XCTest suite for Swift handoff validation logic. |
| `probes/tauri/src-tauri/tauri.conf.json` | Deep-link plugin configuration registering the `ohand-tauri` scheme. |
| `probes/tauri/src-tauri/src/lib.rs` | Native deep-link receiver and Tauri invokable command for handoff validation. |
| `ios/CaptureProbe/Shared/IngressCore.swift` | `IngressOutcome.buildHandoffURL()` builds the handoff URL from the capture ID. |

## Design

- **Handoff URL scheme: `ohand-tauri://`** Routes are registered paths; the only supported route in P07 is `capture`.
- **Capture ID parameter.** The handoff URL includes `?captureId=<uuid>` extracted from the `IngressRecord` (P02). The capture ID is minted by P02 and is stable across retries and relaunches.
- **Reject unknown routes.** A URL like `ohand-tauri://settings` or `ohand-tauri://../../admin` returns `InvalidRoute` and is not opened.
- **Reject malformed URLs.** Missing `captureId`, empty parameter value, or invalid URL syntax return specific errors.
- **No webview required to save.** The native capture (P02) saves the entry immediately when a handoff is received, before the Tauri app is launched. The handoff to Tauri is a separate, optional action that does not block or affect the save.
- **Timestamp on handoff.** The request carries an ISO8601 timestamp from the moment validation succeeds. This is a baseline for timing and auditing; it is not interpreted as a reminder time.
- **No private data in URL.** The URL carries only the capture ID identifier, not message content or user preferences. The Tauri app loads additional context from the native store (not implemented in this probe).

## What the automated checks assert

Rust tests (via `make test` on the workspace):

- Valid `ohand-tauri://capture?captureId=<uuid>` parses and yields the capture ID and timestamp;
- Non-UUID `captureId` (e.g., `evil`, `<script>`) returns `InvalidCaptureId`;
- Missing, empty or wrong-typed `captureId` parameter returns `MissingCaptureId`;
- Duplicate `captureId` parameters return `DuplicateCaptureId`;
- Invalid route paths (e.g., `invalid`, `../../settings`) return `InvalidRoute`;
- URL with no host component returns `MissingRoute`;
- Wrong scheme (e.g., `ohand://` or `http://`) returns `InvalidUrl`;
- Malicious route injection (`../../admin` parsed as route) is rejected;
- Timestamp is a valid ISO8601 datetime string.

Swift tests (wired into `ios/project.yml` and run as part of `OhAndTests` scheme):

- Valid handoff URL with valid UUID validates to a `HandoffRequest` with the correct capture ID and route;
- Non-UUID `captureId` values return `InvalidCaptureId`;
- Duplicate `captureId` parameters return `DuplicateCaptureId`;
- Invalid capture ID, route, or missing query parameters return appropriate errors;
- Malicious route paths are rejected.

Integration tests (CaptureProbe unit tests, run on Linux via `make test`):

- `IngressOutcome.buildHandoffURL()` builds a valid URL with the `ohand-tauri` scheme and `capture` host;
- The handoff URL contains only the scheme, host, and `captureId` query parameter;
- The handoff URL does not include path, user info, port, or fragment components.

Tauri deep-link handler (native receiver in P06):

- The registered `ohand-tauri` scheme directs the OS to route matching URLs to the app;
- The native receiver validates incoming URLs using `HandoffValidator::validate()`;
- Valid handoff URLs are written to `handoff.json` without requiring the webview to be running (cold launch support).

## Limits of the simulator evidence

- The simulator does not exercise the actual handoff from the system launcher, Control Center, or Shortcuts. Unit tests call the validator directly without invoking the system URL routing.
- Accessibility features (keyboard navigation, VoiceOver, large-text rendering, private-route isolation) are not systematically tested in the simulator; they require a physical device.
- The probe validates only the URL format and capture ID; it does not test the Tauri app's reception of the handoff or the full webview UI rendering.

## Needs a physical device (not measured)

- Whether a user can activate a handoff from the lock screen, before the app is running, and whether the handoff correctly passes the capture ID to the Tauri app (cold launch).
- Whether the Tauri app receives the handoff while foreground or background, and how it manages the capture ID when multiple handoffs arrive concurrently or in quick succession.
- Keyboard navigation through the Tauri UI while displaying the handoff capture ID.
- VoiceOver compatibility: whether the capture ID is read aloud when the app launches, whether the handoff status is announced, and whether focus management is correct.
- Large-text (Dynamic Type) rendering of the capture ID and status messages in the Tauri UI.
- Whether private capture IDs (e.g., from sensitive contexts) are isolated from general-purpose routes. This requires a real device and a defined sensitive context.
- Latency from handoff to the Tauri app appearing on screen, and from capture to handoff completion.

## Device procedure for accessibility testing

This procedure tests the handoff mechanism and accessibility features once the P06 Tauri management app UI is implemented with a capture handoff display. P07 provides only the handoff validators and the native receiver; the display itself is part of the P06 implementation.

On an iPhone with a successfully installed and signed build of both CaptureProbe and ohand-tauri-probe:

1. **Cold launch handoff (app not running):**
   - Close the ohand-tauri-probe app completely.
   - Trigger a capture from CaptureProbe (control or shortcut).
   - Verify that the system offers to open ohand-tauri-probe, or verify that ohand-tauri-probe opens automatically.
   - If the app displays the capture ID or a handoff status, verify it is correct and accessible.

2. **Warm launch handoff (app in background):**
   - Launch ohand-tauri-probe and minimize it (or return to home).
   - Trigger another capture from CaptureProbe.
   - Verify that ohand-tauri-probe comes to the foreground.
   - If the app displays the capture ID or status, verify it is correct and differs from the earlier cold-launch ID.

3. **Keyboard navigation test (if the display shows the capture ID):**
   - Connect a hardware keyboard (Bluetooth keyboard or iPad keyboard case).
   - Trigger a handoff from CaptureProbe to ohand-tauri-probe.
   - Use Tab and Shift-Tab to navigate through the UI elements.
   - Verify that focus indicators are visible and the capture ID is reachable.
   - Confirm that activating focused elements (Enter or Space) works as expected.

4. **VoiceOver test (if the display shows the capture ID):**
   - Enable VoiceOver (Settings > Accessibility > VoiceOver > toggle On).
   - Trigger a handoff from CaptureProbe.
   - Use VoiceOver gestures (two-finger Z to go back, one-finger swipe right to next element).
   - Verify that all elements are announced, including the capture ID.
   - Confirm that the handoff status is announced.

5. **Large-text test (if the display shows the capture ID):**
   - Set Dynamic Type to Extra Large or Accessibility Sizes (Settings > Accessibility > Display & Text Size > Larger Accessibility Sizes).
   - Trigger a handoff from CaptureProbe.
   - Verify that the capture ID and status text are fully visible and legible without truncation or overlap.
   - Confirm that layout adjusts gracefully (no clipping, no horizontal scroll required for reading the ID).

6. **Secret isolation test (if a sensitive-context feature is implemented):**
   - Configure a private context or sensitive-data flag in the CaptureProbe.
   - Trigger a handoff from that context.
   - Verify that the ohand-tauri-probe app displays the capture ID without exposing any content or labels that indicate the context is sensitive.
   - Confirm that the handoff URL does not leak sensitive markers (e.g., `?private=true`).

Document the results, toolchain version, device model and OS build. Link any video or screenshot evidence to the PR. If a feature is not testable (e.g., the display is not yet implemented, or private contexts are not yet implemented), record it as "not tested" rather than "passes by default."

## Reuse notes

- The `HandoffValidator` (Rust) and `HandoffCore` (Swift) require a valid UUID as the capture ID. Any non-UUID string (including empty, arbitrary text, or injection attempts) is rejected. The canonical hyphenated lowercase form (8-4-4-4-12 digits) is required for stable identifiers.
- Duplicate `captureId` query parameters are rejected with `DuplicateCaptureId` to prevent ambiguous identities.
- The P06 Tauri app registers the `ohand-tauri` scheme via `tauri.conf.json` deep-link plugin configuration so the system can route handoff URLs to the app.
- The native receiver in P06 listens for the deep-link event (not a webview command) and validates the URL without requiring the webview to be running. This enables cold-launch handoff handling.
- P02 builds the handoff URL via `IngressOutcome.buildHandoffURL()` and can open it (via `UIApplication.shared.open()`) to initiate the handoff to the Tauri app.
- The webview in P06 should decode the query parameter in JavaScript (`new URL(window.location).searchParams.get('captureId')`) when the app launches via a handoff URL.
- Production code should validate the capture ID against the native store (P02) to confirm it exists before displaying it in the UI.
- If the app is not running, the system queues the handoff and delivers it as a cold launch. If the app is already foreground, the handoff is a warm launch. The native receiver handles both cases identically since both write to `handoff.json`.
