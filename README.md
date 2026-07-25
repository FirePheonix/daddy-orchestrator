# daddy-orchestrator

`daddy-orchestrator` is a Rust wrapper around coding-agent CLIs such as `codex`, `claude`, and `opencode`.

It mirrors the core purpose of `caw`: one API, one CLI surface, multiple agent backends. The Rust version focuses on:

- a unified `Agent` / `Session` API
- provider selection and health checks
- replayable multi-turn sessions
- native resume handles for supported providers
- saved trajectories with JSON and JSONL artifacts
- a local trajectory viewer

## Current architecture

The workspace is split into focused crates:

- `daddy-core`: shared models, provider traits, `Agent`, `Session`
- `daddy-providers`: adapters for `codex`, `claude`, and `opencode`
- `daddy-storage`: trajectory loading and terminal inspection helpers
- `daddy-viewer`: local web viewer for saved trajectories
- `daddy-cli`: the `daddy` and `daddy-traj` binaries

## Important design choice

This implementation manages multi-turn sessions by replaying prior turns into each provider request. That keeps the wrapper provider-agnostic and resumable from disk even when a backend CLI does not expose a stable native resume API.

The tradeoff is straightforward: this is simpler and more portable than native provider-session resumption, but it can be less token-efficient on long conversations.

## Install

```bash
cargo build --workspace
```

## CLI

One-shot prompt:

```bash
daddy --provider codex "Explain this repository"
```

Interactive chat:

```bash
daddy chat --provider claude --data-dir daddy_data
```

Resume from a saved trajectory path or a JSON resume handle:

```bash
daddy resume path/to/trajectory.json "Continue the task"
daddy resume "{\"version\":1,...}" "Continue the task"
```

Inspect a saved trajectory:

```bash
daddy traj path/to/trajectory.json
daddy-traj path/to/trajectory.json
```

Launch the web viewer:

```bash
daddy viewer --host 127.0.0.1 --port 7878
```

## Environment variables

- `DADDY_PROVIDER`: comma-separated provider order, for example `claude,codex,opencode`
- `DADDY_MODEL`: explicit model string
- `DADDY_MODEL_TIER`: `strongest` or `fast`
- `DADDY_REASONING`: provider-specific reasoning hint
- `DADDY_SYSTEM_PROMPT`: default system prompt
- `DADDY_DATA_DIR`: default session data directory
- `DADDY_CWD`: override working directory for provider execution

## Provider notes

- `codex` uses `codex exec` with JSON output and reads the final message from a temporary file.
- `claude` uses `claude -p --output-format json`.
- `opencode` uses `opencode run --format json`.

These adapters are best-effort wrappers around the current CLIs and should be validated against your local provider versions.
