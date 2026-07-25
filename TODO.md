# Roadmap

This file tracks the implementation phases for this project.

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
- [x] Replace one-shot provider parsing with richer event-to-trajectory block parsing for all providers.
- [x] Add persisted agent config files for MCP attachment and reusable project defaults.
- [x] Add portable pricing tables and computed cost fallback when providers omit cost fields.

## Remaining parity work

- [ ] Add tool-call and thinking block fidelity that matches the Python wrapper more closely.
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
- [x] Unit tests for provider block parsing across codex, claude, and opencode.
- [x] Unit tests for project config loading and CLI-over-config precedence.
- [x] Unit tests for pricing table lookup and fallback estimation.
- [ ] Fixture tests for Claude JSON output parsing.
- [ ] Fixture tests for opencode JSON event parsing.
- [ ] Integration tests for CLI commands with mocked provider binaries.
- [ ] Windows and Linux CI coverage.
