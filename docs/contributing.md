# Contributing

This repository is public. Commit only synthetic fixtures; keep credentials, personal captures, recordings, private conversations, signing material and internal endpoints out of it. See [AGENTS.md](../AGENTS.md) for the build and review gates.

## Public fixture and secret hygiene

`make hygiene-check` (run by `.github/workflows/hygiene.yml` on every pull request) does two things. A filename scan cannot prove privacy, so the policy layer narrows what may be committed and the scanner looks inside content and history.

### Maintained secret scanner

[gitleaks](https://github.com/gitleaks/gitleaks) **8.21.2** scans every commit reachable from `HEAD`, so a secret that was added and later deleted still fails. It runs with `--log-opts="-m HEAD"`, so a merge commit's changes are diffed against each parent and content added while resolving a merge is scanned too. It uses the default ruleset plus Anthropic and OpenAI key rules from `tools/hygiene/gitleaks.toml`. Findings are redacted: output names only the file, line, rule and commit, never the matched value.

- The check fails closed. A missing binary, a scanner error, a timeout or a malformed report exits with status 2 and never counts as clean.
- Inline `gitleaks:allow` comments are disabled, and a committed `.gitleaksignore` is itself a policy failure. To tune a rule, change `tools/hygiene/gitleaks.toml` and justify it in review.
- Locally, install the pinned release from the gitleaks GitHub releases page and put `gitleaks` on `PATH`. CI verifies the archive against a pinned SHA-256.

### Policy checks on tracked files

These rules are evaluated against the git index and the recorded tree of each commit, not the working tree. Every index entry is checked whatever its type, so replacing a prohibited file with a symlink (dangling or not) or deleting it only from the working tree does not hide it. Audio/video fixtures must be regular files; a symlinked fixture fails even if its sidecar digest matches the link.

- **Signing material and secrets files** are never allowed: `.p12`, `.pfx`, `.p8`, `.mobileprovision`, `.provisionprofile`, `.cer`, `.crt`, `.der`, `.pem`, `.key`, `.jks`, `.keystore`, `.gpg`, `.asc`, `.certSigningRequest`, keychains, SSH private key names such as `id_ed25519`, and `.env` / `.env.*` files (use `.env.example`, `.env.sample` or `.env.template`).
- **Audio and video** (`.m4a`, `.wav`, `.mp3`, `.aac`, `.flac`, `.ogg`, `.opus`, `.caf`, `.aiff`, `.mp4`, `.mov` and similar) is allowed only under these documented synthetic fixture roots. Tracked files with other names are also sniffed for common container signatures (WAV/AVI, MP4/MOV/M4A `ftyp` other than HEIC/AVIF images, ID3, Ogg, FLAC, CAF, AIFF, Matroska/WebM, ASF, AMR), so `data/voice.bin` or `notes.m4a.txt` holding a WAV fails the same way:
  - `fixtures/`
  - `core/tests/fixtures/`
  - `ios/Tests/Fixtures/`
- Each such file needs a tracked sidecar named `<file>.provenance.json`:

  ```json
  {
    "synthetic": true,
    "contains_personal_data": false,
    "generator": "script or tool that produced the audio",
    "description": "what the fixture contains",
    "sha256": "hex SHA-256 of the media file"
  }
  ```

  The check fails if the sidecar is missing, untracked, malformed, says anything other than the values above, or its `sha256` no longer matches the media file. The sidecar is an attestation by the author, enforced for presence and integrity; reviewers still judge whether the content really is synthetic.

- **Private capture directories**: no file of any type may live under `captures/`, `recordings/`, `private/`, `personal/`, `transcripts/`, `conversations/`, `voice-memos/`, `voicememos/` or `voice_memos/` at the repository root, or directly beneath a fixture root (for example `fixtures/recordings/`). Source modules deeper in the tree, such as `core/src/store/captures/`, are ordinary code and are not affected.
- **Reachable history**: the same signing-material, environment-file, suppression-file, private-directory and audio/video rules also apply to paths that were deleted but are still reachable from `HEAD`, because a deleted private artifact stays public in history. Merge commits are walked against each parent (`git log -m`), so a path added and removed only while resolving merges is checked too. Every version of an audio/video file under a fixture root that is reachable from `HEAD` (overwritten or deleted versions included) must have had a valid sidecar, with the same fields and a matching digest, in the same commit's tree; a private recording that was later replaced or removed still fails. Commits that change only the sidecar are checked as well, so deleting or corrupting a record before deleting its media fails. CI checks out full history (`fetch-depth: 0`); a shallow clone fails closed (exit status 2) because truncated history cannot be certified. Run `git fetch --unshallow` locally if needed.

These rules cannot prove privacy. The content sniff applies only to files tracked at `HEAD` and recognises only the listed container headers; deleted history is judged by path and extension. A recording that was renamed and later removed, or stored raw, compressed, encoded or in an unlisted format, is not detected by the policy layer. Other files, including documents under `docs/validation/`, are covered by the secret scanner only, and reviewers remain responsible for what a fixture actually contains.

### Tests

`make hygiene-test` (also part of `make test`) builds temporary git repositories and runs the real `check_hygiene.py` entry point against committed seeded secrets, private-path audio, signing material and approved fixtures, asserting exit status and that seeded values never appear in output. Seeded secrets are assembled at runtime from fragments, so this repository never contains one. The scanner-backed tests skip locally when `gitleaks` is not installed; the hygiene workflow sets `HYGIENE_REQUIRE_GITLEAKS=1` so they cannot skip in CI.
