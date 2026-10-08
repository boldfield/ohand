# Native-to-Tauri handoff and management accessibility (P07)

P07 probes the handoff from the native capture entry (P02) to the candidate Tauri management shell (P06). It connects using stable identifiers and rejects unknown or malicious routes. This document records what the probe implements and what automated checks assert. Device-specific accessibility evidence is not measured in simulator results.

## What exists

| Path | Role |
| --- | --- |
| `probes/tauri-handoff/src/lib.rs` | `HandoffValidator` validates handoff URLs, rejects unknown routes, and extracts the capture ID. `HandoffRoute` and `HandoffRequest` types define the protocol. |
| `probes/tauri-handoff/Cargo.toml` | Library manifest with minimal dependencies (url, chrono). |
| `probes/tauri-handoff/Sources/HandoffCore.swift` | Swift equivalent for unit tests and potential native integration. |
| `probes/tauri-handoff/tests/HandoffValidatorTests.swift` | XCTest suite for Swift handoff validation logic. |

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
- Case-insensitive scheme matching (`OHAND-TAURI://...` works);
- Malicious path injection (`../../settings`) is rejected;
- Timestamp is a valid ISO8601 datetime string.

Swift tests (`HandoffValidatorTests` in `ios/project.yml`, run as part of `OhAndTests` scheme):

- Valid handoff URL with valid UUID validates to a `HandoffRequest` with the correct capture ID and route;
- Non-UUID `captureId` values return `InvalidCaptureId`;
- Duplicate `captureId` parameters return `DuplicateCaptureId`;
- Invalid capture ID, route, or missing query parameters return appropriate errors;
- Case-insensitive scheme matching works in Swift;
- Malicious route paths are rejected.

## Limits of the simulator evidence

- The simulator does not exercise the actual handoff from the system launcher, Control Center, or Shortcuts. Tests open URLs through scene delegates (`openURLContexts`), which does not invoke the system UI that users interact with.
- Accessibility features (keyboard navigation, VoiceOver, large-text rendering, private-route isolation) are not systematically tested in the simulator; they require a physical device.
- The handoff does not test the full webview UI, only the URL validation layer.

## Needs a physical device (not measured)

- Whether a user can activate a handoff from the lock screen, before the app is running, and whether the handoff correctly passes the capture ID to the Tauri app (cold launch).
- Whether the Tauri app receives the handoff while foreground or background, and how it manages the capture ID when multiple handoffs arrive concurrently or in quick succession.
- Keyboard navigation through the Tauri UI while displaying the handoff capture ID.
- VoiceOver compatibility: whether the capture ID is read aloud when the app launches, whether the handoff status is announced, and whether focus management is correct.
- Large-text (Dynamic Type) rendering of the capture ID and status messages in the Tauri UI.
- Whether private capture IDs (e.g., from sensitive contexts) are isolated from general-purpose routes. This requires a real device and a defined sensitive context.
- Latency from handoff to the Tauri app appearing on screen, and from capture to handoff completion.

## Device procedure for accessibility testing

When a device is available, perform the following steps on an iPhone running the latest supported OS:

1. **Setup:**
   - Install both the CaptureProbe and Tauri-handoff probe apps.
   - Ensure VoiceOver is available (Settings > Accessibility > VoiceOver).
   - Ensure Dynamic Type is set to a testable size (Settings > Accessibility > Display & Text Size).

2. **Keyboard navigation test:**
   - Launch the Tauri-handoff app.
   - Connect a hardware keyboard (Bluetooth keyboard or iPad keyboard case).
   - Use Tab and Shift-Tab to navigate through the UI elements.
   - Verify that focus indicators are visible and the capture ID is reachable.
   - Confirm that activating focused elements (Enter or Space) works as expected.

3. **VoiceOver test:**
   - Enable VoiceOver (Settings > Accessibility > VoiceOver > toggle On).
   - Launch the CaptureProbe app with a keyboard, shortcut, or control.
   - Trigger a handoff to the Tauri-handoff app.
   - Use VoiceOver gestures (two-finger Z to go back, one-finger swipe right to next element).
   - Verify that all elements are announced, including the capture ID.
   - Confirm that the handoff status (e.g., "Capture received") is announced.

4. **Large-text test:**
   - Set Dynamic Type to Extra Large or Accessibility Sizes (Settings > Accessibility > Display & Text Size > Larger Accessibility Sizes).
   - Launch the Tauri-handoff app.
   - Trigger a handoff from the CaptureProbe.
   - Verify that the capture ID and status text are fully visible and legible without truncation or overlap.
   - Confirm that layout adjusts gracefully (no clipping, no horizontal scroll required for reading the ID).

5. **Secret isolation test (if a sensitive-context feature is implemented):**
   - Configure a private context or sensitive-data flag in the CaptureProbe.
   - Trigger a handoff from that context.
   - Verify that the Tauri-handoff app displays the capture ID without exposing any content or labels that indicate the context is sensitive.
   - Confirm that the handoff URL does not leak sensitive markers (e.g., `?private=true`).

Document the results, toolchain version, device model and OS build. Link any video or screenshot evidence to the PR. If a feature is not testable on the device (e.g., private contexts are not yet implemented), record it as "not tested" rather than "passes by default."

## Reuse notes

- The `HandoffValidator` (Rust) and `HandoffValidator` (Swift) require a valid UUID as the capture ID. Any non-UUID string (including empty, arbitrary text, or injection attempts) is rejected.
- Duplicate `captureId` query parameters are rejected with `DuplicateCaptureId` to prevent ambiguous identities.
- The `ohand-tauri` scheme must be registered in the app's `Info.plist` so the system can route handoff URLs to the Tauri app.
- The Tauri app should decode the query parameter in JavaScript (`new URL(window.location).searchParams.get('captureId')`) or via a Tauri command that the webview invokes.
- Production code should validate the capture ID against the native store (P02) to confirm it exists before displaying it in the UI.
- If the app is not running, the system queues the handoff and delivers it as a cold launch. If the app is already foreground, the handoff is a warm launch. The Tauri app should distinguish these for logging and testing.
