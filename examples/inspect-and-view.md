# Inspect And View

Render a saved trajectory in the terminal:

```bash
daddy traj runs/chat.json
```

The secondary binary exposes the same inspection flow:

```bash
daddy-traj runs/chat.json
```

Launch the local trajectory viewer:

```bash
daddy viewer --host 127.0.0.1 --port 7878
```

Then open the viewer in a browser and load the saved `trajectory.json` file.
