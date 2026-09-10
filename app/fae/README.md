# fae

Codex-style terminal client for Free Agent Engine. The interface keeps a
scrollable transcript, a multiline composer, live tool and workflow status,
and streamed model output in one stable TUI.

## Single agent

Initialize the default `fae` agent:

```bash
fae init
```

This creates `~/.fae/agents/fae_config.json`, enabling every built-in tool and
every skill currently installed under `~/.fae/skills`. Existing configs are
preserved; use `fae init --force` to replace one. Set `FAE_DEFAULT_MODEL` or
pass `--model` to choose another model:

```bash
fae init --model gpt-5
```

The generated configuration has this shape:

```json
{
  "agent": {
    "name": "fae",
    "user_id": "local",
    "session_id": "default",
    "metadata": {}
  },
  "model": {
    "model": "gpt-4o-mini",
    "trigger_compression_size": 32000,
    "history_turns": 20,
    "max_completion_tokens": 65536,
    "temperature": null,
    "max_tool_iterations": 8
  },
  "tools": [
    "execute_command",
    "read_file",
    "write_file",
    "list_directory",
    "apply_patch",
    "send_http_request",
    "execute_python"
  ],
  "skills": [
    {
      "type": "name",
      "value": "fae-agent"
    },
    {
      "type": "name",
      "value": "fae-workflow"
    },
    {
      "type": "name",
      "value": "weather"
    }
  ],
  "mcp_servers": []
}
```

The install script places the bundled
[`fae_prompt.txt`](../../docs/agents/fae_prompt.txt) at
`~/.fae/agents/fae_prompt.txt` without replacing an existing prompt. Customize
it if needed, then start an interactive session:

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

## Uninstall

Remove the currently running `fae` executable:

```bash
fae uninstall
```

This leaves `FAE_HOST` and all agent configs, prompts, skills, workflows, and
session data unchanged.

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
