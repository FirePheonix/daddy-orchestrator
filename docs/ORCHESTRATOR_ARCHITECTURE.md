# Orchestrator Architecture

`daddy` is a CLI-native orchestration layer for coding-agent workers.

The runtime that launches provider CLIs already exists. The next stage is to build the orchestrator above it instead of extending the provider wrapper indefinitely.

## Product shape

The user should issue one high-level goal:

```text
daddy run "Build OAuth login"
```

The orchestrator should then:

1. decompose the goal into sub-tasks
2. decide which tasks can run in parallel
3. choose worker backends and model tiers
4. select only the context relevant to each task
5. execute workers inside isolated Git workspaces
6. terminate and respawn workers when their task-scoped context becomes inefficient
7. merge the resulting changes
8. store execution telemetry for future routing decisions

## Layer split

### Worker runtime

This layer already exists in the current workspace.

Responsibilities:

- provider abstraction
- worker process execution
- event normalization
- usage and cost tracking
- session persistence
- health checks

Crates already covering this:

- `daddy-core`
- `daddy-providers`
- `daddy-storage`
- `daddy-viewer`
- `daddy-cli`

### Orchestrator core

This is the next build target.

Responsibilities:

- jobs and tasks
- task graph execution
- worker assignment
- parallelism control
- context routing
- isolated workspaces
- merge and review flow
- session eviction and respawn

Planned crate:

- `daddy-orchestrator`

### Intelligence layer

This layer should improve token efficiency and long-run quality.

Responsibilities:

- semantic retrieval
- memory pruning
- handoff artifact generation
- compression when it is cheaper than worker restart
- benchmark history
- adaptive routing

## Design rules

### Workers are disposable

Worker sessions should be short-lived and task-scoped.

`daddy` should prefer:

- stopping a saturated worker
- saving a structured handoff artifact
- respawning a fresh worker with only the next task's context

Instead of:

- carrying huge chat histories
- compacting long worker sessions by default
- forwarding planner chatter into worker prompts

### Planning and coding are different roles

The orchestrator model should not write code directly.

It should only:

- decompose work
- assign workers
- choose context
- escalate review
- decide when to stop or respawn workers

Worker models should only:

- edit files
- run tools
- write tests
- implement task-scoped changes

### Context is task-scoped

Every worker should receive:

- its task
- acceptance criteria
- relevant files and snippets
- relevant prior diffs
- relevant handoff artifacts

Workers should not receive:

- the full repository by default
- unrelated worker transcripts
- large planner histories

## First implementation track

### Track 1: orchestration skeleton

- define orchestrator types
- define planner and scheduler traits
- implement a deterministic caveman planner
- implement a basic scheduler
- expose `daddy run`

### Track 2: workspace isolation

- create one worktree per worker
- capture branch metadata
- store worker diffs
- prepare merge inputs

### Track 3: token efficiency

- add task-scoped context selection
- add session saturation thresholds
- add handoff artifacts
- stop and respawn workers instead of compacting by default

### Track 4: intelligence

- add a configurable planner backend
- add retrieval
- add memory and telemetry
- add adaptive routing

## CLI roadmap

### Near term

```text
daddy run "Build OAuth login"
daddy run "Fix failing tests in auth flow"
```

### Later

```text
daddy run --planner heuristic "Build OAuth login"
daddy run --planner local:qwen3-4b "Build OAuth login"
daddy benchmark
```

## Output contracts

The orchestrator should standardize these objects:

- `Job`
- `Task`
- `TaskGraph`
- `WorkerAssignment`
- `ContextBundle`
- `HandoffArtifact`
- `TaskResult`
- `JobResult`

These contracts need to stay stable enough that the CLI, scheduler, workspace layer, and memory layer can evolve independently.
