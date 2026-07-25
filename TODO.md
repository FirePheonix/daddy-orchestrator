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
- [x] Add provider-specific usage-limit detection and `doctor --live` probes.
- [x] Add tool-call and thinking block fidelity that matches the current provider output formats more closely.

## Remaining parity work

- [x] Add broader cross-platform integration tests with mocked CLIs and fixture outputs.
- [x] Add workflow examples for common one-shot, chat, resume, and viewer flows.

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
- [x] Unit tests for provider-specific usage-limit detection.
- [x] Fixture tests for Claude JSON output parsing.
- [x] Fixture tests for opencode JSON event parsing.
- [x] Integration tests for CLI commands with mocked provider binaries.
- [x] Windows and Linux CI coverage.

## Next orchestration track

- [x] Add `docs/ORCHESTRATOR_ARCHITECTURE.md` that defines the worker runtime, orchestrator core, and intelligence layer.
- [x] Add a new `daddy-orchestrator` crate for job orchestration above provider execution.
- [x] Define shared orchestrator types for `Job`, `Task`, `TaskGraph`, `WorkerAssignment`, and `HandoffArtifact`.
- [ ] Define orchestrator traits for `Planner`, `Scheduler`, `ContextRouter`, `WorkspaceManager`, `MergeEngine`, and `MemoryStore`.
- [x] Implement a `CavemanPlanner` that decomposes requests with deterministic heuristics before model-backed planning exists.
- [ ] Implement a `GitWorktreeManager` that gives every worker an isolated worktree and branch.
- [x] Implement a `BasicScheduler` that can assign tasks to providers and cap parallelism.
- [x] Add a `daddy run "<goal>"` command that executes an orchestrated job instead of a single worker session.
- [ ] Add task-scoped context selection so workers receive only relevant files, snippets, diffs, and acceptance criteria.
- [ ] Add session eviction rules that stop saturated workers, persist handoff artifacts, and respawn fresh workers instead of compacting long chats.
- [ ] Add a merge and review pipeline for combining worker outputs and escalating conflicts to a reviewer worker.
- [ ] Add structured telemetry for task cost, latency, restarts, merge outcomes, and worker selection decisions.
- [ ] Add SQLite-backed memory and benchmark storage for completed jobs and worker outcomes.
- [ ] Add a configurable daddy planner backend that can use local inference, OpenAI-compatible endpoints, or `vllm`.
- [ ] Add adaptive routing that learns which worker/model combinations perform best for each task category.

## Next testing matrix

- [x] Unit tests for heuristic task decomposition and task graph validation.
- [x] Unit tests for scheduler assignment, retries, and parallelism limits.
- [ ] Unit tests for context selection and handoff artifact generation.
- [ ] Unit tests for session eviction and worker respawn thresholds.
- [ ] Integration tests for git worktree isolation and merge behavior.
- [ ] Integration tests for orchestrated `daddy run` flows with mocked provider workers.
- [ ] Benchmark fixtures that compare single-worker runs against orchestrated parallel runs.
