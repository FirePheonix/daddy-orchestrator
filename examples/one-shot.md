# One-Shot Prompt

Run a single prompt against a specific provider:

```bash
daddy --provider codex "Summarize the current repository layout"
```

Use provider fallback order instead of a single provider:

```bash
daddy --provider codex,claude,opencode "Explain the failing test"
```

Save the resulting trajectory:

```bash
daddy --provider codex --traj-path runs/one-shot.json "Describe the public CLI commands"
```
