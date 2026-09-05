# fae

Codex-style terminal client for Free Agent Engine. The interface keeps a
scrollable transcript, a multiline composer, live tool and workflow status,
and streamed model output in one stable TUI.

## Single agent

Create `~/.fae/agents/fae_config.json`:

```json
{
  "agent": {
    "name": "fae",
    "user_id": "local",
    "session_id": "default",
    "metadata": {}
  },
  "model": {
    "model": "gpt-xxx",
    "context_size": 32000,
    "history_turns": 20,
    "max_completion_tokens": 4096,
    "temperature": null,
    "max_tool_iterations": 8
  },
  "tools": ["read_file", "execute_command"],
  "skills": [
    {
      "type": "name",
      "value": "fae-agent"
    },
    {
      "type": "name",
      "value": "fae-workflow"
    }
  ],
  "mcp_servers": []
}
```

Put the system prompt in `~/.fae/agents/fae_prompt.txt`, then start an
interactive session:

```bash
cargo run -p fae
```

Select another agent ID or explicit files:

```bash
cargo run -p fae -- agent --agent-id reviewer "review this workspace"
cargo run -p fae -- agent \
  --agent-config ./reviewer.json \
  --agent-prompt ./reviewer.txt \
  "review this workspace"
```

Available session commands are `/help`, `/status`, `/clear`, and `/exit`.

Keyboard controls:

- `Enter`: submit
- `Ctrl+J` or `Shift+Enter`: insert a newline
- `Up` / `Down`: browse input history
- `PageUp` / `PageDown`: scroll the transcript
- `Esc`: interrupt the active run
- `Ctrl+C`: exit while idle, interrupt while running

The TUI uses the alternate screen by default. Use `--no-alt-screen` to retain
the interface in terminal scrollback:

```bash
cargo run -p fae -- --no-alt-screen
```

## Workflow

Workflow metadata is loaded from
`$FAE_HOST/workflows/<workflow-id>.json`, or from
`~/.fae/workflows/<workflow-id>.json` when `FAE_HOST` is unset.

```bash
cargo run -p fae -- workflow release-review \
  --input '{"path":"Cargo.toml"}'
```

Use `@path` to read input JSON from a file:

```bash
cargo run -p fae -- workflow release-review --input @input.json
```

Tool, nested workflow, single-agent, session, and the default
`workflow.python` actions are registered by the application. Custom workflow
actions still require their own runtime.
