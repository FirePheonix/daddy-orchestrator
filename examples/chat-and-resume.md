# Chat And Resume

Start a multi-turn session and persist session artifacts:

```bash
daddy --config examples/daddy.json chat --save runs/chat.json
```

Exit the chat with `/exit` or `/quit`.

Resume from a saved trajectory path:

```bash
daddy resume runs/chat.json "Continue with the next implementation step"
```

Resume from a JSON handle captured from library code:

```bash
daddy resume "{\"version\":1,\"provider\":\"codex\",\"session_id\":\"...\"}" "Continue from the same backend session"
```
