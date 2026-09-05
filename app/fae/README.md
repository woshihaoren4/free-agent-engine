# fae

Codex-style terminal client for Free Agent Engine. The interface keeps a
scrollable transcript, a multiline composer, live tool and workflow status,
and streamed model output in one stable TUI.

## Single agent

Set the model and start an interactive session:

```bash
export FAE_DEFAULT_MODEL=<model>
cargo run -p fae
```

Pass the first message directly:

```bash
cargo run -p fae -- "review the current workspace"
```

The explicit form supports the same options:

```bash
cargo run -p fae -- agent \
  --session code-review \
  --tool read_file \
  --tool execute_command \
  --skill weather \
  "check today's release"
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
