# Native-to-Tauri handoff and management accessibility (P07)

Probe for task P07. It checks that the native capture entry (P02, `CaptureProbe`) can hand a saved entry to the Tauri
management shell (P06, `probes/tauri`) without the shell being needed to save, and that the management surface stays
usable with a keyboard, VoiceOver and large text. P10 records the shell decision; this document only records evidence.

## What the handoff carries

Only a capture identifier. The native entry has already durably saved the capture (P02 `records/<id>.json`) before the
"Review in management app" button is enabled, so the save never depends on the shell or its webview. The shell stores the
identifier and delivery facts, nothing else. Looking up the capture content by identifier belongs to the future core; this
probe deliberately does not move capture text, transcripts or credentials across the boundary.

## URL grammar (identical in Rust and Swift)

```
ohand-tauri://capture?captureId=<ID>
```

`<ID>` is an upper-case, hyphenated 8-4-4-4-12 hex identifier, the form the native entry mints. The grammar is a literal
string match with no URL library, so the Rust and Swift parsers cannot disagree about normalisation. Anything else is
rejected with a stable code:

| Code | Meaning |
| --- | --- |
| `bad_scheme` | not `ohand-tauri://` |
| `unknown_route` | anything between `ohand-tauri://` and `?` other than exactly `capture`: other route, extra path, trailing slash, user info, port, a fragment before the query, other casing |
| `missing_capture_id` | no `captureId=` query |
| `unexpected_component` | a query that does not start with `captureId=` (another parameter first) |
| `invalid_capture_id` | the value after `captureId=` is not exactly a canonical identifier: lower-case, wrong length, non-hex, encoded, or followed by anything (second or repeated parameter, fragment, whitespace, newline) |

Both implementations are tested against the same vectors, `probes/tauri-handoff/fixtures/handoff-urls.json`:

- Rust: `probes/tauri-handoff/src/url_grammar.rs`, `tests/handoff.rs` (run by `make test` as part of the workspace). The test
  `every_shared_vector_gets_the_same_verdict_through_the_receiver` runs each vector through `receive_urls`, the function both
  iOS scene hooks call, including the upper-case-scheme and trailing-newline vectors a normalising parser would accept.
  The iOS hook wiring itself (that the raw string reaches `receive_urls`) is only proven by the simulator `rejected` phase.
- Swift: `ios/CaptureProbe/Shared/ManagementHandoff.swift`, `ios/CaptureProbe/Tests/ManagementHandoffTests.swift`.

## Receiving: native, not webview

- `probes/tauri/src-tauri/Info.ios.plist` registers the `ohand-tauri` scheme (merged into the app plist by tauri-cli).
- `probes/tauri/src-tauri/src/lib.rs` hands every delivered URL to `receive_urls`. Each URL is parsed with the
  grammar and, if valid, written atomically to `<app data>/handoffs/<ID>.json` (`ohand-tauri-handoff::HandoffInbox`).
  First write wins, so a replayed URL does not rewrite the record. Rejected URLs only increment a counter in
  `handoffs/rejections.json` (count, last reason code, time); the hostile URL text is never stored or shown.
- On iOS both delivery paths come from `scene_urls.rs`, which wraps tao's scene delegate and passes the receiver the
  unmodified `absoluteString`: `scene:willConnectToSession:options:` for the URL that launched the app (cold) and
  `scene:openURLContexts:` for a URL opened while it runs (warm). The shell deliberately does not record from
  `RunEvent::Opened` on iOS, because tao has already parsed that URL with the `url` crate, which lower-cases the scheme
  and strips tab and newline, so `OHAND-TAURI://capture?captureId=<id>` would arrive looking canonical. Cold and warm
  therefore apply the same literal grammar to the same bytes (see known risks 1 and 2). On macOS and Android
  `RunEvent::Opened` is still the receiver's input; those targets are not exercised by this probe.
- A record holds `captureId`, `receivedAtUnixMs` and `webviewReady`. `webviewReady` is false when the URL arrived before
  the web UI reported it had loaded, which is what a cold launch looks like. No webview is needed to record it.
- The web UI lists identifiers with `textContent` only and polls the `list_handoffs` command, so a handoff recorded
  before the webview existed shows up as soon as it loads.

## Automated evidence (simulator)

CI job `tauri-probe` in `.github/workflows/ios.yml` builds and installs the shell, then runs the
`ManagementHandoffUITests` scheme (`ios/CaptureProbe/UITests/ManagementHandoffUITests.swift`) against real installs of
both apps on one simulator. The test taps the real "Review in management app" button.

| Phase | What the test does | What is asserted |
| --- | --- | --- |
| cold | Native entry launched once (the runner installs it on first launch), then both terminated; the entry saves via its URL, button tapped. | Shell state was `notRunning` when the entry saved; shell comes to the foreground and lists the same identifier. |
| warm | Shell left in the background; a second entry is saved and handed off. | Shell was backgrounded; lists both identifiers. |
| rejected | Three hostile `ohand-tauri://` URLs are opened directly while the shell is running: a bad identifier, path traversal, and an upper-case scheme around a valid, already-stored identifier (the URL the `url` crate would normalise into an accepted one). | The shell shows "Handoffs rejected: 1.", "2.", then "3."; the list still has exactly two identifiers. |
| large-text | Both apps launched at `AccessibilityXXXL`. | The handoff button exists, is enabled and becomes hittable by scrolling; tapping it opens the shell (also at largest text), which lists that entry's identifier. |

Afterwards `ios/scripts/verify_handoff_evidence.py` (unit-tested in `ios/scripts/test_verify_handoff_evidence.py`,
run by `make check`) reads the real files from both simulator data containers and requires that:

- the shell inbox holds exactly the cold, warm and large-text identifiers plus `rejections.json`, and nothing else;
- each shell record has only `captureId`, `receivedAtUnixMs`, `webviewReady`, and the same identifier exists as a saved
  CaptureProbe record, so identity is preserved end to end;
- the cold record has `webviewReady == false` and the warm record `true`;
- the rejection summary counts exactly three and uses a known reason code;
- the shell's `run-events.log` shows the scene hook installed, a `scene-connect urls=1` delivery (the cold URL) and a
  `scene-open urls=` delivery (warm URLs through the raw hook).

Evidence uploaded as `tauri-probe-evidence` (7 days): `handoff-uitests.log`, `handoff-phases.txt`,
`handoff-verification.txt`, `shell-handoffs/`, `capture-root/`, the xcresult bundle with screenshots.

## What the simulator does not prove

The simulator cannot show real keyboard, VoiceOver speech or hardware behaviour, and the large-text phase checks
reachability, not readability. Those are recorded by the device procedure below. Nothing here claims they were run.

## Device procedure (repeatable, real UI)

Needs a physical iPhone with both apps installed (`CaptureProbe` and the Tauri probe), VoiceOver available, and a Bluetooth
or Smart Keyboard for step 4. Record the device model, iOS version and build commit, then the result of each step as
pass/fail with a screenshot or screen recording. Use synthetic text only.

1. Cold handoff. Force-quit the Tauri probe. In CaptureProbe save a synthetic entry, tap "Review in management app".
   Expect the Tauri probe to open and list the same identifier shown in CaptureProbe's detail text.
2. Warm handoff. Leave the Tauri probe in the background, save a second entry, tap the button. Expect both identifiers.
3. Hostile route. From Safari or Notes open `ohand-tauri://capture?captureId=not-a-uuid`. Expect "Handoffs rejected"
   to increase and no new list entry. Stay with well-formed URLs (see known risk 1).
4. Keyboard. In the Tauri probe, with a hardware keyboard, Tab to reach the text field, Echo, Refresh list. Expect a
   visible focus outline on each and activation with Space/Return.
5. VoiceOver. In CaptureProbe, swipe to "Review in management app": it is announced as a button with the hint "Opens the
   management app. Only this entry's identifier is sent." In the Tauri probe, the "Received captures" heading is
   navigable by the rotor (Headings), the summary is a live status, and each identifier is read in full.
6. Large text. Settings > Accessibility > Display & Text Size > Larger Text, set to the maximum with Larger Accessibility
   Sizes on. In both apps nothing is clipped, no control disappears, and the CaptureProbe button is reachable by
   scrolling.
7. Secret isolation. CaptureProbe saves the fixed synthetic text "Synthetic probe capture". After a handoff, confirm the
   Tauri probe never displays that text, and (Xcode > Devices and Simulators > download the app container) that
   `handoffs/<ID>.json` contains only `captureId`, `receivedAtUnixMs` and `webviewReady`. The CI `tauri-probe` boundary
   check separately confirms the webview assets contain no credential words or network calls.

## Known risks

1. tao 0.37.1 (via Tauri 2.12.1) has a path that can crash on a malformed URL: its app-delegate `application:openURL:`
   handler and the universal-link handler call `Url::parse(...).unwrap()`. The scene path this shell receives URLs on
   drops a URL that does not parse (and logs it) without reaching `RunEvent::Opened`; the `scene_urls.rs` hooks see the
   raw string before that, so such a URL is counted as a rejection instead of vanishing. Only the scene path was
   exercised; the unwrap paths were read in source, not tested.
2. tao 0.37.1 mishandles scene URLs twice, and `probes/tauri/src-tauri/src/scene_urls.rs` works around both through the
   Objective-C runtime (tao's method runs unchanged first, then the hook reads the same arguments).
   - Cold: the launching URL is only in `UISceneConnectionOptions.URLContexts`, which tao's `TaoSceneDelegate` ignores;
     CI showed the shell listing nothing after a cold handoff while a warm one worked.
   - Warm: `scene:openURLContexts:` parses the URL into a `url::Url` before `RunEvent::Opened`. The `url` crate
     normalises (lower-case scheme, tab and newline stripped), so validating `Url::as_str()` accepts input the grammar
     must reject. The shell therefore does not record from `RunEvent::Opened` on iOS.
   The hooks depend on tao's private class name (`TaoSceneDelegate`) and selectors, so a tao upgrade can silently disable
   them. `install` changes nothing unless both selectors exist; the shell writes `run-events.log` (event kinds and
   timestamps only, never URLs, capped at 16 KiB) and the CI verifier requires `scene-hook installed=true`, a
   `scene-connect` line and a `scene-open` line, so such a regression fails CI. If the hooks are not installed the iOS
   shell records nothing, which fails closed rather than falling back to normalised URLs. The production shell should
   get this fixed upstream or in its own native scene hook.
3. The cold `webviewReady == false` assertion depends on timing; a slow runner could flip it, which would be a false
   failure, not a false pass.
