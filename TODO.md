# Roadmap

This file tracks the implementation phases for the Rust port of `caw`.

## Completed

- [x] Create the Rust workspace and split the project into focused crates.
- [x] Implement the core `Agent` and `Session` API with provider abstraction.
- [x] Implement trajectory persistence with `trajectory.json`, `traj.jsonl`, and per-turn files.
- [x] Add a CLI surface with one-shot execution, chat, trajectory inspection, resume, and viewer commands.
- [x] Add a local trajectory viewer.
- [x] Add GitHub Actions for `fmt`, `clippy`, and `test`.
- [x] Add provider-native resume metadata flow for `codex`, `claude`, and `opencode`, with replay fallback.
- [x] Add self-contained JSON resume handles in addition to path-based resume.
- [x] Add CLI-level MCP server loading from JSON config files.

## Remaining parity work

- [ ] Replace one-shot provider parsing with richer event-to-trajectory block parsing for all providers.
- [ ] Add tool-call and thinking block fidelity that matches the Python wrapper more closely.
- [ ] Add persisted agent config files for MCP attachment and reusable project defaults.
- [ ] Add portable pricing tables and computed cost fallback when providers omit cost fields.
- [ ] Add provider-specific usage-limit detection and `doctor --live` probes.
- [ ] Add broader cross-platform integration tests with mocked CLIs and fixture outputs.
- [ ] Add examples that mirror the Python reference project.

## Testing matrix

- [x] Unit tests for provider alias resolution.
- [x] Unit tests for codex usage parsing.
- [x] Unit tests for trajectory inspection rendering.
- [x] Unit tests for on-disk session persistence.
- [x] Unit tests for env-driven provider order.
- [x] Unit tests for native resume metadata flow.
- [ ] Fixture tests for Claude JSON output parsing.
- [ ] Fixture tests for opencode JSON event parsing.
- [ ] Integration tests for CLI commands with mocked provider binaries.
- [ ] Windows and Linux CI coverage.
