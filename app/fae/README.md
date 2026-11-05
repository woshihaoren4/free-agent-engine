# fae

Codex-style terminal client for Free Agent Engine. The interface keeps a
scrollable transcript, a multiline composer, live tool and workflow status,
and streamed model output in one stable TUI.

## Background service

The terminal UI and direct CLI commands are WebSocket clients. A separate
`fae` process owns engines, workflows, history, todos and memory. Business
commands (including `init`) automatically start it at `127.0.0.1:6666` when
needed. Closing the client leaves the service and its active tasks running.

```bash
fae server start
fae server start --address 127.0.0.1 --port 2666
fae server status
fae server show
fae server stop
```

`start` waits for an authenticated readiness handshake. An occupied port is
an error; it never chooses another port. A running service is reused from
`server.config`, including a custom port. Stop it before changing listening
parameters. `stop` waits for cancellation, engine cleanup and process exit.
`show` displays the PID, address, port, versions, start time, instance ID,
executable and config path. `status`, `show`, `stop`, `--help` and `--version`
do not start a service.

Use `--fae-home` or `FAE_HOST` to isolate service instances. Each home contains
`server.config`, a private `server.token`, `start.lock`, `server.lock` and
rotating `log/server.log`. The token authenticates both control and session
connections; browser Origin requests are rejected. No system service is
installed, so automatic startup happens on the next business command after
an OS restart. A stopped service's next implicit startup uses the default
address and port.

On Unix, start the service as the owner of the FAE home. A `sudo` launch in
another user's home is rejected because it would create root-owned private
credentials. The default port 6666 does not require privileged port access.
Explicit ports below 1024 may require privileges. Later clients reuse the
running service's configured port. To recover a service previously launched with
sudo, run `sudo fae --fae-home /absolute/path/to/.fae server stop`, then start
it as the home owner. Keep `server.token` at mode `0600`.

Each logical session has its own connection and fixed workspace/configuration.
An active session cannot be opened by a second client. Clients can resume
within the same service instance using ordered event replay (up to 4096
events or 16 MiB). A replay gap is reported explicitly. A service restart
does not rerun interrupted work, and unacknowledged writes are never
automatically repeated. The service allows 32 retained sessions and 8 active
rounds; disconnected idle sessions expire after 30 minutes.

The implementation is split into `src/app` (CLI and transport), `src/server`
(execution and lifecycle), `src/protocol` (shared wire types) and `src/tui`
(terminal presentation).

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
    "desc": "fae agent",
    "user_id": "master",
    "session_id": "default",
    "metadata": {}
  },
  "model": {
    "model": "gpt-4o-mini",
    "image_detail": "auto",
    "trigger_compression_size": 32000,
    "history_turns": 10,
    "max_completion_tokens": 65536,
    "temperature": null,
    "max_tool_iterations": 128
  },
  "prompt_sections": [],
  "tools": [
    "execute_command",
    "read_file",
    "write_file",
    "list_directory",
    "apply_patch",
    "send_http_request",
    "execute_python",
    "memory_update",
    "todo"
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
  "mcp_servers": [],
  "sub_agents": [],
  "workflows": []
}
```

The prompt file is wrapped in `<setting>`. Resolved capabilities are appended
before history as `<skills>`, `<mcp>`, `<sub_agent>`, and `<workflow>` sections.
Add arbitrary English-tagged sections through `prompt_sections`:

```json
{
  "prompt_sections": [
    {
      "tag": "project_context",
      "text": "Repository-specific constraints."
    }
  ],
  "sub_agents": ["reviewer", "researcher"],
  "workflows": ["release-review", "deploy"]
}
```

Configured sub-agents are exposed through the `agent` model tool. The
parent prompt receives only each sub-agent's ID and non-empty `agent.desc`.
Configured workflows are exposed through the `workflow` model tool, and
each workflow's ID and non-empty `desc` are added to the parent prompt. Both
special tools are mounted automatically and should not be listed in `tools`.

The install script places the bundled
[`fae_prompt.txt`](../../docs/agents/fae_prompt.txt) at
`~/.fae/agents/fae_prompt.txt` without replacing an existing prompt. Customize
it if needed, then start an interactive session:

```bash
cargo run -p fae
```

Passing a prompt runs one conversation without entering the TUI. The assistant
response is streamed to standard output, and the process exits when the
conversation finishes:

```bash
cargo run -p fae -- agent --agent-id fae-coding "你好"
```

Use `--workspace` to select the project workspace; it defaults to the current
directory. Before each single-agent model request, `fae` loads any existing
`.fae/workspace.md`, `.fae/rules.md`, `.fae/agents.md`, and `.fae/project.md`
files from that workspace and appends them to the system prompt:

```bash
cargo run -p fae -- --workspace ~/projects/example "review this project"
```

Use `--session-id` to select the conversation history for either interactive
or direct mode:

```bash
cargo run -p fae -- agent --agent-id reviewer \
  --session-id issue-42 \
  "review this workspace"
```

An explicit config and prompt can be used instead of an agent ID:

```bash
cargo run -p fae -- agent \
  --agent-config ./reviewer.json \
  --agent-prompt ./reviewer.txt \
  "review this workspace"
```

Use `/todo <message>` to run that message in single-agent todo mode. Available
session commands also include `/help`, `/status`, `/clear`, `/exit`,
`/session new`, `/session id=<id>` and `/session clean` (also `/clean`).
Cleaning removes this session's history and the agent/user's shared todos;
it is rejected while any related session has an active round.

Keyboard controls:

- `Enter`: submit
- `Ctrl+J` or `Shift+Enter`: insert a newline
- `Ctrl+O` or click `+ Add image`: attach one or more local images
- `Up` / `Down`: browse input history
- `PageUp` / `PageDown`: scroll the transcript
- `F2`: enter copy mode; drag to select output, then press `F2` or `Esc` to resume
- `Esc`: interrupt the active run
- `Ctrl+C`: exit while idle; cancel the current round, wait for cleanup and
  exit the client while running

On Linux, the in-app file browser uses `Up`/`Down` to navigate, `Enter` to
open a directory, `Space` to select images, `Backspace` to move to the parent
directory, and `A` or `Ctrl+Enter` to attach the selection. Other platforms
use the native file dialog.

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

This first stops the current home's service and leaves `FAE_HOST` and all agent configs, prompts, skills, workflows, and
session data unchanged.

## Workflow

Workflow metadata is loaded from
`$FAE_HOST/workflows/<workflow-id>.json`, or from
`~/.fae/workflows/<workflow-id>.json` when `FAE_HOST` is unset.

```bash
cargo run -p fae -- workflow release-review \
  --input '{"path":"Cargo.toml"}'
```

Workflows run non-interactively by default and print their final JSON result to
standard output. Use `--interactive` to show live progress in the TUI:

```bash
cargo run -p fae -- workflow release-review --interactive \
  --input '{"path":"Cargo.toml"}'
```

Use `@path` to read input JSON from a file:

```bash
cargo run -p fae -- workflow release-review --input @input.json
```

Tool, nested workflow, single-agent, session, and the default
`workflow.python` actions are registered by the application. Custom workflow
actions still require their own runtime.
