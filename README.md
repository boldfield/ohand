# Oh And

Catch a thought. Get back to your day. Find it when it matters.

Oh And is an open-source personal capture, memory, and action assistant being designed for iPhone and Mac. Speak or type a thought with minimal friction, keep the original, and retrieve it conversationally or have it resurface when useful.

**Status: design and project bootstrap. There is no installable app yet.**

The planned experience has three entry points:

- **Catch this:** save a voice or text capture immediately, including offline.
- **Help me now:** find a remembered fragment or get help choosing a manageable next step.
- **Bring this back:** explicit reminders and a small, optional amount of proactive resurfacing.

Normal use should require no tagging, inbox clearing, daily planning ritual, or catch-up after time away. Tasks, ideas, general notes, and private session notes share a capture experience without turning every thought into an obligation.

## Project direction

- Durable local capture; a Mac does not have to stay online for the phone to work.
- Configurable AI providers and accounts, targeting Anthropic, OpenAI, and supported self-hosted endpoints.
- Source records and user corrections remain authoritative; model output is replaceable.
- Processing destinations are explicitly configured before upload, including for transcription and review.
- Tauri 2 is a candidate, subject to real-device capture and system-integration tests. The phone experience determines the framework choice.
- Hosted service support is a possible later milestone, not a prerequisite for the personal app.

See [DESIGN.md](DESIGN.md) for the behavior, constraints, and three milestones. The intended primary domain is `ohand.app`; this repository does not imply that a website has been deployed.

## Development

Implementation runs through [Odonian](https://github.com/boldfield/odonian): small model-pinned tasks, independent adversarial review, automated checks, and a separate merge agent. This project has no routine human approval or merge gate. Real device testing and account-holder actions remain genuine inputs.

See [AGENTS.md](AGENTS.md) for the project-specific execution policy. The repository currently contains documentation only; build and CI commands will be established by implementation tasks.

Do not submit credentials, personal captures, private conversation transcripts, or recordings in issues, fixtures, or pull requests. Use synthetic examples.

## License

Apache License 2.0. See [LICENSE](LICENSE).
