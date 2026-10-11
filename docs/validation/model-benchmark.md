# Paired model benchmark command (XB09)

`bench` (crate `tools/bench`) runs a paired comparison of exactly two provider profiles over the committed **synthetic** intent corpus. It is a developer tool: it is not linked into the app, reads no production data, and never applies a result to any store. Its output is diagnostic only.

A run in `fake` mode, and a run against a local fixture server, validate the pipeline: argument handling, planning, budgets, journaling, resume, cancellation and redaction. **They say nothing about the quality of any real provider.** Only a `live` run, with explicit grants, sends fixture text to a provider, and even that is a small synthetic sample and not a quality verdict.

## Minimal usage

```sh
cargo run -p bench -- help
cargo run -p bench -- plan --corpus fixtures/intent/contrastive-fixtures.json \
    --profile-a anthropic:<model-a> --profile-b openai:<model-b>
cargo run -p bench -- run  --corpus fixtures/intent/contrastive-fixtures.json \
    --profile-a anthropic:<model-a> --profile-b openai:<model-b> --journal /path/to/run.ndjson
```

- A profile is `<protocol>:<model>[:<credential-ref>]` with protocol `anthropic` or `openai`. Exactly two, different profiles are required. Self-hosted profiles are not supported by this command.
- `--corpus` names the one file that is read. The corpus loader rejects any fixture whose provenance is not synthetic. Nothing is discovered, and no capture, store or database is opened.
- `help` and `plan` need no credentials and open no socket. `help` writes nothing, and `plan` writes nothing but a private scratch copy of the journal in the system temporary directory, deleted before it exits (see below). `plan` lists the corpus, instruction and profile versions and, per case, the arm order and whether each side would be sent; with `--journal` it reads (never writes) an existing journal to show which sides a run would keep, and refuses, exactly as `run` would, a journal that a run would refuse.
- `run` appends every result to `--journal`. `--max-cases`, `--case <id>`, `--attempts-per-side` (1-5), `--request-cap`, `--timeout-seconds` and `--build-revision` narrow or label the run.

## Modes

| Mode | Network | Meaning |
| --- | --- | --- |
| `fake` (default) | none | Scripted synthetic replies. A pipeline check; every report says so. |
| `live` | yes | Real providers. Needs `--execute-network`, an explicit `--request-cap`, and the grants below. |
| `recorded` | none | Reserved label. Refused: no recorded provider responses are wired to this command. |

The mode, corpus version, instruction version, both profile versions and the build revision are pinned in the journal's experiment record and printed in every plan and report. When no `--build-revision` is given the label is the crate version and the source revision is journaled as unknown rather than guessed.

## Live runs and secret references

Choosing a profile grants nothing. A live run is refused, with exit status 2 and before the journal is created or written or any request is reserved, unless all of these are given:

- `--execute-network`, the single switch that lets requests leave the machine;
- `--request-cap <n>`, a hard cap on requests reserved by the run;
- `--allow-destination <protocol>` for each protocol a selected profile uses (and for no other);
- `--credential-env <ref>=<VAR>` for each credential reference a selected profile uses.

A credential reference is an opaque name (default `bench-<protocol>`), never the secret. `--credential-env` maps it to the *name* of an environment variable; the secret is read from that variable only when a request is dispatched, and only for the granted destination. Secrets are never accepted on the command line, printed, or journaled, and error messages do not name the variable or the reference. A reference that looks like a secret is rejected at parse time.

```sh
export BENCH_ANTHROPIC_KEY=...   # set in your shell, never on the command line
export BENCH_OPENAI_KEY=...
cargo run -p bench -- run --corpus fixtures/intent/contrastive-fixtures.json \
    --profile-a anthropic:<model-a>:anthropic-key --profile-b openai:<model-b>:openai-key \
    --mode live --execute-network --request-cap 20 --max-cases 5 \
    --allow-destination anthropic --allow-destination openai \
    --credential-env anthropic-key=BENCH_ANTHROPIC_KEY --credential-env openai-key=BENCH_OPENAI_KEY \
    --journal /path/to/run.ndjson
```

Destinations are the providers' hosted origins; the command has no flag for another endpoint. Selection also never grants processing permission: authorization is evaluated by the core routing rules over a private in-memory store built only from the grants above.

## Resume, rerun, cancellation

- Re-running the same command against the same journal keeps every side that already has a dispatched attempt and sends only the rest. A side whose outcome is unknown is never sent again unless named with `--rerun <case-id>:<a|b>`, which spends new allowance from `--request-cap` and records what it follows. `bench plan` checks the same rerun targets and refuses one that `run` would refuse, so the dry run never shows a plan that execution rejects.
- Changing a profile, the corpus, the instructions or the build revision against an existing journal is refused with the journal untouched; use a new journal. The check reads the journal without opening it for writing, so even a journal with a torn last line stays byte-identical when refused; a run whose configuration matches resumes it and repairs the torn tail. These are refused the same way, each with its own message: a journal that is not exactly one experiment; a case journaled twice or under another experiment; an attempt for a case the journal does not record, for an unknown arm, or written by another executor version; and a journaled case text that differs from the corpus.
- As a backstop for anything else the executor would refuse (for example a derived request that no longer matches the one journaled), `plan` and `run` first rehearse the executor on a private scratch copy of the journal (mode 0700 directory in the system temporary directory) with an already cancelled run and synthetic adapters. Cancellation is checked before any request is reserved, so the rehearsal sends nothing; the copy is deleted when it ends, and the real journal is opened only if it succeeds.
- Ctrl-C ends the in-flight request promptly. The journal records what was and was not sent, and the next run resumes from it.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Every side completed (or `help` / `plan` succeeded). |
| 1 | The run finished or stopped (budget, cancel, provider failure) with sides not completed. |
| 2 | Usage error or refusal. Nothing was sent. |

## Verification

`tools/bench/src/cli/tests.rs` drives the whole command in-process: argument failures, the dry run (no files, no environment reads), fake runs with resume, rerun, cap and configuration-change refusal, every inconsistent-journal refusal with a torn journal left byte-identical by both `plan` and `run`, every live refusal (no switch, no cap, missing grant, unmapped or empty credential) with zero connections, a live run against a local TLS fixture server that checks the credential reaches only the granted origin and appears nowhere in output or journal, and cancellation before a run and while a provider hangs.
