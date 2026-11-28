# fae

Codex-style terminal client for Free Agent Engine. The interface keeps a
scrollable transcript, a multiline composer, live tool and workflow status,
and streamed model output in one stable TUI.

The public envelope uses `api_version: "0.1.0"` and `event`, with raw business
`payload`. Responses contain `response_id`, top-level `code`/`msg`, and the
business value directly in `payload`. Worker bindings are accepted only on the
authenticated internal worker transport. Plan/Runtime plugins use API `0.3.0`;
existing Hook plugins use `0.2.0`. See the
[provider guide](../../docs/faeplan/fae-plan-runtime-plugin-implementation.md).

## Background service

The terminal UI and direct CLI commands are WebSocket clients. A separate
`fae` process owns engines, workflows, history, todos, memory and cron timers. Business
commands (including `init`) automatically start it at `127.0.0.1:6666` when
needed. Closing the client leaves the service and its active tasks running.

```bash
fae server start
fae server start --address 127.0.0.1 --port 2666
fae server status
fae server show
fae server stop
fae restart
```

`fae restart` stops the Feishu gateway first, restarts the background service,
then starts the gateway and waits until it is ready. It preserves a running
service's address and port; if the service is stopped, it starts at
`127.0.0.1:6666`. Without `lark.config`, it only restarts the service.
An existing gateway configuration and its Agent dependencies are checked
before stopping either component. Restart cancels active work. If gateway
startup fails, the command reports an error and the restarted service remains
running. Use `fae --fae-home /path/to/.fae restart` to select a home.

`start` waits for an authenticated readiness handshake. An occupied port is
an error; it never chooses another port. A running service is reused from
`server.config`, including a custom port. Stop it before changing listening
parameters. `stop` waits for cancellation, engine cleanup and process exit.
`show` displays the PID, address, port, versions, start time, instance ID,
executable and config path. `status`, `show`, `stop`, `--help` and `--version`
do not start a service.

Use `--fae-home` or `FAE_HOST` to isolate service instances. Each home contains
`server.config`, a private `server/server.token`, the process lock `server/server.lock`,
the start/stop lock `server/start-stop.lock` and rotating `server/server.log`.
The token authenticates both control and session connections; browser Origin
requests are rejected. No system service is
installed, so automatic startup happens on the next business command after
an OS restart. A stopped service's next implicit startup uses the default
address and port.

On Unix, start the service as the owner of the FAE home. A `sudo` launch in
another user's home is rejected because it would create root-owned private
credentials. The default port 6666 does not require privileged port access.
Explicit ports below 1024 may require privileges. Later clients reuse the
running service's configured port. To recover a service previously launched with
sudo, run `sudo fae --fae-home /absolute/path/to/.fae server stop`, then start
it as the home owner. Keep `server/server.token` at mode `0600`.

Before upgrading from the old root-level lock/token layout, stop the service
using the old executable, then start it with the new executable. Lock files
must not be moved while the service is running.

Logical sessions are keyed by the trusted user and session ID. Multiple
connections may subscribe to one session. Each Execution loads and fixes its
Agent/Workflow definitions, fingerprints and workspace; later executions reload
the definitions.
Its workspace can be changed when opening or resuming it: an explicit caller
workspace overrides the saved session workspace. Omitting the protocol `workspace`
field reuses that saved directory, including after a service restart. New sessions
without a saved directory fall back to the service's current directory.
The TUI always supplies a directory: the current directory by default, or the
value of `--workspace`. Changes apply to subsequent rounds; a running round
retains its original workspace. Conversation history stays with the session.
Session workspace defaults are saved in `session/<agent>/<user>/<session>.json`,
alongside the corresponding `<session>.jsonl` conversation history.
An active session can be observed by multiple clients. Clients can resume
within the same service instance using ordered event replay (up to 4096
events or 16 MiB). A replay gap is reported explicitly. A service restart
does not rerun interrupted work, and unacknowledged writes are never
automatically repeated. The service allows 32 retained sessions and 8 active
rounds; disconnected idle sessions expire after 30 minutes.

The implementation is split into `src/app` (CLI and transport), `src/server`
(host operations and subscriptions), `src/protocol` (transport settings) and `src/tui`
(terminal presentation).

Shared wire types live in `fae-agent::protocol`; only v4 is supported.
Create/get/subscribe do not start work, including Workflows. Start creates a new
Execution and returns a conflict while busy. Steer acknowledges acceptance;
execution/get exposes Queued/Applied/NotApplied input records. Stop checks the
expected Execution ID, and finished follows cleanup of all Invocations and
plugin calls. The Engine coordinator owns ContextData and reusable results.
Idle configuration changes, conversation clearing and archiving use standard
Session operations; archived sessions remain readable. Workspace paths use the
existing JSON files, and history remains in the existing target/user/session
partition. Update the service, clients and Python SDK together (plugin API 0.2.0).

## Feishu gateway

`fae lark` runs an independent gateway using `larksuite-oapi-sdk-rs` 0.3.13
(Rust 1.95+). It receives enterprise self-built bot messages over a long
connection and executes the configured Agent through the local FAE service.
Creating a configuration never starts the gateway; normal TUI, workflow and
server commands never start it.

Each ordinary message outside a thread creates a thread with a short acceptance
reply before Agent execution. The real Feishu `thread_id` is the session ID;
messages inside that thread continue its session, isolated by sender `open_id`.
Group messages always require @bot, including thread replies and commands.
Chats that cannot return a real thread ID do not execute the Agent.

Idle thread sessions accept `/help`, `/status`, `/session`, `/session clean`
(or `/clean`), and `/todo <message>`. Cleanup keeps the thread binding.
`/session new` asks users to send a message outside the thread; `/session id=<id>`
asks them to reply in the corresponding thread. Management commands outside
threads do not operate on a previous session. Durable bindings survive receipt
retention and actor eviction. Old chat histories and selection files are retained
but are not used for newly received messages.

While the current session runs, every incoming message is immediately treated
as steering input, including slash-command text except `/stop` and session switching.
`/stop` cancels the current round and is silently ignored when idle. `/steer <message>` explicitly
supplements that round and fails when the session is idle. There is no ingress
or per-session pending-round queue: at the concurrency limit, new rounds are
rejected with a retry notice; steering an active round bypasses that limit.
Legacy `session_queue_capacity` settings are accepted and ignored.

```bash
fae lark init
# Edit ~/.fae/lark.config: app_id, app_secret, tenant_key, agent_id, workspace.
# Lark workspace defaults to your home directory (~); override with --workspace when initializing or edit workspace in lark.config.
fae lark check
fae lark check --online  # Checks bot credentials without sending a message
fae lark show           # Redacts the app secret
fae lark start
fae lark start --foreground
fae lark status
fae lark stop           # Leaves the shared Agent service running
```

The JSON configuration is private (0600 on Unix). `app_key` is accepted as
an alias for `app_id`. `workspace` must resolve to an existing absolute
directory. Use `--fae-home`/`FAE_HOST` to change the home. Edit the configuration
and restart the gateway to apply changes. A conflicting explicit `--workspace`
is rejected. The init template contains placeholders and cannot be started
until credentials are filled in.
Thread replies are mandatory. Old `group.reply_in_thread=false` settings are
accepted with a notice and normalized to `true`.

Set `"user_id_whitelist": ["ou_alice", "ou_bob"]` in `lark.config` to restrict
Agent interactions to those senders' Feishu `open_id` values. Omit the field
or set it to `null` to allow all users; an explicit empty list `[]` denies
everyone. Users outside the whitelist receive the text `权限不足` without
opening an Agent session or executing commands. Update the whitelist without
restarting the gateway:

```bash
fae lark whitelist add ou_alice ou_bob
fae lark whitelist remove ou_bob
fae lark whitelist set ou_alice  # No IDs means deny everyone
fae lark whitelist reload       # Apply a manually edited user_id_whitelist
```

Updates are saved to `lark.config` and applied immediately when the gateway is
running; otherwise they take effect on the next start. `add` changes an
unrestricted whitelist into a list containing the supplied users. `remove`
requires a restricted list. Updates affect subsequent admission checks without
cancelling accepted rounds. `status` shows the active whitelist.

Enable bot capability, publish the application, select long-connection delivery
and subscribe to `im.message.receive_v1`. Grant
`im:message.p2p_msg:readonly`, `im:message.group_at_msg:readonly`,
`im:message:send_as_bot`, `im:message.reactions:write_only`,
`cardkit:card:write`.
The sender's Feishu `open_id` is used as the Agent `user_id`; the user ID
permission `contact:user.employee_id:readonly` is not required for this identity.
Missing open IDs produce an error instead of falling back to user ID, union ID
or master. Switching from the previous user ID identity creates separate
session histories, todo state and user memory; existing data is not migrated.
Add the bot to the group and @ it; messages from other tenants and bots are ignored.
Only text input is supported. Card JSON 2.0 needs Feishu client 7.20 or newer.

The conversation's main Agent receives a `LarkEnvironment` prompt section,
separate from the user query. It describes the application, tenant, sender
`open_id`, chat type/ID, current thread/session, and latest accepted message's
ID, reply relationships, original thread ID, timestamps, bot identity and
structured mentions. Metadata is read from the gateway's durable receipt;
older receipts may lack event fields. Steering refreshes this section on the
next model request. The section identifies the JSON as environment data;
it does not fetch contact profiles, group details or quoted message bodies.
Only the authenticated conversation's root Agent receives it, including when
a child Agent has the same name. Other sessions, delegated Agents, workflow
nodes and timer executions do not receive this environment section.

Accepted messages inside a thread receive a `Typing` reaction on a separate task.
Steering inputs receive the same reaction and keep it until the original round's
final answer has been delivered, even if the gateway restarts after confirmation.
New threads are created with a `Typing` emoji reply without an extra reaction.
Once all final output has
been delivered, the gateway withdraws that reply and removes the reaction.
Model output is grouped by Invocation and response; plugin Content is delivered
as independent notifications and retained when execution succeeds or is cancelled.
Final results follow the complete Execution, including history saving and cleanup.
Long final replies are paginated on Unicode
boundaries. Completed cards show only the answer without a generating status.
Final cards are created with their full answer and summary, without a placeholder
or follow-up streaming updates. Failures discard unsent intermediate answers.
Failures are sent with `ERROR`; CardKit failures fall back to
text with an explicit degradation notice. Outbound failures remain in the
receipt store and appear in status.

New history partitions use the real thread ID as session ID and sender open_id
as user ID. Legacy chat hashes are retained only for recovery.
The same user's distinct threads have separate histories;
user memory and the configured workspace remain shared. Todo routes are allowed:
todo state is isolated by plan ID under
`<home>/session/<agent>/<user>/todo/<plan_id>.json`, and mutations require
matching plan ownership in the runtime Context. Startup checks still load
configured Agents, sub-Agents and typed workflow dependencies, including Agents
loaded by explicit config/prompt paths and nested workflows. Dynamic
session/custom actions and delegating or custom tool routes are also allowed;
their dependencies are resolved at runtime. This is not a filesystem sandbox.

`<home>/lark/records` contains private durable receipt and delivery records.
Completed receipts are kept for seven days by default. The server persists
external request acceptance separately under `<home>/requests`; these records
are retained conservatively to prevent accidental repeated execution.
Reconnect uses the original service instance, stream and sequence.
Resuming an active Execution keeps its fixed definitions. The next Execution
reloads them and can fail if a source has disappeared. Available replay is consumed
through the Execution's finished event before the outcome is recorded.
An uncertain submission or a restarted service is reported without blindly
rerunning tools. Replay gaps use the final result to replace the buffered answer.
There is no transaction guarantee for external tool side effects.
Shutdown confirms the router and outbox have exited before final delivery.
Its delivery budget is shared across all records within a 25-second shutdown
deadline; records that remain undelivered are retried on the next start.

The gateway has a private loopback control socket and instance handshake.
Its start/stop lock is `lark/lark.lock`; `lark/gateway.lock` protects the running
process.
Its service credential (`lark/lark.token`) only opens `/v4/lark/session` and cannot
initialize agents or stop the service. Session operations are constrained by the
authenticated Lark binding. Wire protocol v4
requires restarting previously running services after upgrading.

For live acceptance, test private chat and @bot group chat, a response containing
both text and tool calls, a failed Agent round, duplicate message delivery,
concurrent users and gateway/service restarts. The repository checks use local
mocks; real Feishu permissions, streaming behavior and network recovery require
deployment verification.

## Plugins

Install the Python SDK in the interpreter used by the service:

```bash
python3 -m pip install -e ./python/fae-plugin-py
# Set this before starting the service when using a virtual environment.
export FAE_PLUGIN_PYTHON=/path/to/venv/bin/python
fae plugin install /path/to/project_context.py
fae plugin show project_context
fae plugin show '*'
fae plugin uninstall project_context
```

Install single files or directories into `<fae_home>/plugins/`. Python single files
provide `plugin_config()`; directories provide JSON `plugin.config`. Plugins
installed from directories must declare `id` in `plugin.config`, using
`[a-z][a-z0-9_]{0,63}`. The directory name can differ from this ID.
Plugins support Agent hooks, tool channels and complete/streaming model responses.
`direct` registers separate Rust execution nodes; `chain` runs one Python onion
chain in installation order.

`<fae_home>/plugins/plugins.config` is the fixed JSON installation manifest.
The `plugins` array records order, source filename, directory path, installation and enablement:

```json
{
  "plugins": [
    {"id": "project_context", "source": "project_context.py", "directory_path": "", "installed": true, "enabled": true},
    {"id": "demo", "source": "Release-v1", "directory_path": "Release-v1", "installed": true, "enabled": true}
  ]
}
```

`directory_path` is relative to `<fae_home>/plugins/` and loads that directory's
`plugin.config`. An omitted or empty path looks for `<id>.py`, then `<id>/`,
then `<id>.js`. Existing manifests remain readable. Directory installation
preserves the source directory name and records it in `directory_path`.
`source` records the original filename; loading uses `directory_path` or ID lookup.

Startup loads current sources for installed, enabled entries in array order.
Unlisted sources are not loaded. Any configuration, import, worker or mount
error fails service startup. Edit `enabled` to disable an installed plugin.
Install replaces the same ID, including file/directory changes, and preserves
its position. Uninstall retains its files and records both flags as false;
reinstalling an uninstalled plugin appends it to the end.
When the service is running, install activates the plugin immediately and uninstall
removes its runtime registrations without restarting the service. Chain changes
rebuild the remaining chain; active runs retain their original worker snapshot.
When offline, these commands update files and configuration for the next startup
without starting the service. Manual enablement edits apply at the next startup.
An activation error is reported and leaves the current runtime snapshot intact;
the copied source and installation record remain, so startup will validate them again.
Inspection does not start the service or execute plugin code; offline single-file
declarations are unknown. JavaScript is not available in this release.
`fae plugin show` defaults to the same compact list as `fae plugin show '*'`.
See the [SDK API and runnable examples](../../python/fae-plugin-py/README.md).

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
    "todo",
    "timer"
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

## Cron timers

New agent configs include `timer`; add it to `tools` in existing configs to
enable scheduling. Ask the agent to create, query, update or delete a timer.
For example, the agent can call:

```json
{
  "operation": "create",
  "idempotency_key": "daily-summary-1",
  "name": "Daily workspace summary",
  "input": "Summarize today's changes in this workspace.",
  "schedule": {
    "kind": "loop",
    "cron": "0 0 9 * * *",
    "timezone": "Asia/Shanghai"
  }
}
```

Schedules support `once`, `repeat` with a positive `max_fires`, and `loop`.
Cron accepts 5 fields (minute through weekday), 6 (seconds through weekday),
or 7 (including year), using the cron 0.16 dialect. Timezone defaults to UTC.
Query returns UTC timestamps, `next_local`, future previews and, with
`include_occurrences: true`, recent execution statuses and new session IDs.
Query supports `id`, `state`, `limit` (1–100), `cursor` and `include_deleted`.
Update requires `id` and `expected_revision`; `enabled: false` pauses and
`enabled: true` resumes without resetting the count. Supply the entire
`schedule` when changing it. Reuse the creation key when retrying a request.
Delete accepts an optional `expected_revision` and keeps a tombstone.

Each firing creates a separate chat session under the saved user and agent,
using the original workspace and freshly loaded agent configuration. It
loads user memory without copying the creating conversation. Config user
and default session changes do not override timer ownership. A missing
source, renamed agent or invalid workspace fails that occurrence.

Closing the client or cleaning session history/todos leaves timers active.
The service must remain running: stopping it, sleeping or shutting down the
machine prevents on-time execution. On recovery, `coalesce` (default) merges
missed times into one; `skip` ignores them. Executions of the same timer do
not overlap; at most one additional due time is retained and coalesced.
Counts measure durably accepted firings, including failures and cancellations.
Deleting a timer suppresses pending work but does not cancel an execution
already handed off.

State is stored separately at
`<FAE home>/timer/<agent>/<user>/state.json`, protected by `schedule/timer.lock`.
The service runs at most 2 timer rounds within its 8-round/32-session limits.
The pending queue is capped at 256; excess due times wait without consuming
counts. Definitions are limited to 100 per agent/user and 1000 per home.
Completed occurrence history retains up to 1000 entries per partition or
30 days; active entries and creation idempotency records are preserved.
Pending occurrences replay with their original session IDs after restart;
dispatching/running occurrences become `interrupted` and are not rerun.
There is no operating-system wakeup, login startup or native timer CLI.

For embedding, own one `TimerRuntime`, consume its single `watch()` stream,
and install `handle.client()` into session engines. A trusted
`TimerExecutionBindingResolver` supplies the execution source/workspace;
the default resolver uses `ContextScope.timer_execution`, inherited by task
contexts and rebound for child agents. Call `exit()` on the host runtime
when shutting down; client engine exit does not stop the shared scheduler.

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

Omit `--session-id` to create a new Session with a host-generated ID. Use the
ID shown by the TUI with `--session-id` to select an existing Session in
interactive or direct mode; an unknown ID returns 404:

```bash
cargo run -p fae -- agent --agent-id reviewer \
  --session-id "<existing-session-id>" \
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
`/session new`, `/session id=<id>`, `/stop` and `/session clean` (also `/clean`).
`/stop` cancels the active round and returns to the prompt without leaving the session;
it is ignored when idle.
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
session data unchanged. It does not start a service if none is running.

If removing the executable requires administrator privileges, use `sudo` with
an explicit FAE home so it stops the correct user's service:

```bash
sudo fae --fae-home /path/to/your/.fae uninstall
```

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
