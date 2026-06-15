---
name: fae
description: Work on the free-agent-engine (FAE) project. Use when you need to understand FAE directory structure, initialize workspaces, create or configure agents, add skills, configure MCP servers, edit agent config.json, or run and verify fae-ctl / Rust examples.
---

# FAE Project SOP

Use this skill when working inside the `free-agent-engine` repository or an installed FAE home directory.

## First Pass

1. Identify whether the task is about repository code or runtime data.
   - Repository code lives in the Rust workspace.
   - Runtime data lives under `$FAE_HOME`, defaulting to `~/.fae`.
2. Inspect the relevant file before editing. Prefer `rg`, `rg --files`, `sed -n`, and focused reads.
3. Keep changes scoped. Do not rewrite prompts, generated docs, or configs unrelated to the requested agent, skill, MCP, or crate.
4. After editing Rust code, run a targeted check or test when feasible:
   ```bash
   cargo check -p fae-agent
   cargo check -p fae-engine
   cargo check -p fae-ctl
   cargo run -p examples --example basic
   ```

## Repository Map

- `app/fae-ctl/`: CLI entrypoint. Commands are defined in `src/args.rs`, dispatched in `src/main.rs`, and implemented in `src/init_project.rs` / `src/agents.rs`.
- `crates/fae-agent/`: core agent abstractions, file-backed agent config, sessions, memory, task types, environment selectors, skill and MCP config types.
- `crates/fae-engine/`: engine assembly, workspace loader, runtimes, executors, built-in tools, skill loader, MCP client executor.
- `crates/async-openai/`: vendored OpenAI-compatible client and generated API types.
- `examples/`: minimal workspace and single-agent examples.
- `docs/prompt/`: default prompt files copied or referenced by initialized agents.
- `docs/skills/`: distributable skills. `docs/site.txt` lists files downloaded by `fae init`.

Important runtime paths:

- `$FAE_HOME`: environment variable; defaults to `~/.fae`.
- `$FAE_WORKSPACE`: workspace name; CLI defaults to `main`.
- `$FAE_HOME/<workspace>/<agent_id>/config.json`: file-backed agent config.
- `$FAE_HOME/<workspace>/<agent_id>/<prompt_file>`: relative prompt location when `prompt_dir` is relative.
- `$FAE_HOME/prompt/*.txt`: shared prompt files created by `fae init`.
- `$FAE_HOME/skills/<skill_name>/SKILL.md`: default skill lookup path.
- `$FAE_HOME/mcp/*.json`: MCP config files scanned by the MCP executor.

## CLI Workflow

Initialize runtime files and default agents:

```bash
cargo run -p fae-ctl -- --ws main init
```

This creates `$FAE_HOME`, downloads files listed in `docs/site.txt`, creates `$FAE_HOME/prompt`, `$FAE_HOME/mcp/mcp_list.json`, `$FAE_HOME/main/main`, and `$FAE_HOME/main/fae_coding`.

List agents:

```bash
cargo run -p fae-ctl -- --ws main agent
```

Chat with an agent:

```bash
cargo run -p fae-ctl -- --ws main agent --id main --chat
cargo run -p fae-ctl -- --ws main agent --id fae_coding --chat
```

Show chat history for a user:

```bash
cargo run -p fae-ctl -- --ws main agent --id main --user master --history
```

## Agent Runtime Model

FAE loads agents from `$FAE_HOME/<workspace>/<agent_id>/`.

An agent directory must contain:

- `config.json`
- the prompt file referenced by `config.json.prompt_dir`
- generated memory/session data created during chat

`prompt_dir` rules:

- Absolute path: used directly, for example `/Users/me/.fae/prompt/aicoding.txt`.
- Relative path: resolved under the agent directory, for example `system.txt` resolves to `$FAE_HOME/<workspace>/<agent_id>/system.txt`.

Core agent config shape:

```json
{
  "name": "Research Assistant",
  "description": "Specialized agent for research tasks.",
  "model": {
    "model": "gpt-4o",
    "channel": "OpenAI-Compatible API",
    "max_chat_history_round": 20,
    "reasoning_effort": 2,
    "max_completion_tokens": null,
    "min_compact_window_size": 65536,
    "temperature": 1.0,
    "top_p": 1.0
  },
  "prompt_dir": "system.txt",
  "tools": [
    { "name": "read_file", "channel": "default" },
    { "name": "write_file", "channel": "default" },
    { "name": "apply_patch", "channel": "default" },
    { "name": "execute_command", "channel": "default" }
  ],
  "skills": [
    { "name": "fae", "channel": "default" }
  ],
  "mcp_servers": [],
  "sub_agents": [],
  "custom": {}
}
```

When adding tools to `config.json`, verify they are registered in `AgentsEngine::default()` or in the custom engine builder. If chat fails with `no tools found`, compare the config list with the registered `ToolSetImplMap`.

## Create An Agent

Use the CLI initializer for the default agents. For a custom file-backed agent, create the directory, prompt, and config:

```bash
mkdir -p "$FAE_HOME/main/researcher"
$EDITOR "$FAE_HOME/main/researcher/system.txt"
$EDITOR "$FAE_HOME/main/researcher/config.json"
```

Minimum valid config:

```json
{
  "name": "Researcher",
  "description": "Searches, reads, and summarizes project information.",
  "model": {
    "model": "gpt-4o",
    "channel": "OpenAI-Compatible API",
    "max_chat_history_round": 10,
    "reasoning_effort": 2,
    "min_compact_window_size": 65536,
    "temperature": 1.0,
    "top_p": 1.0
  },
  "prompt_dir": "system.txt",
  "tools": [
    { "name": "read_file", "channel": "default" },
    { "name": "send_http_request", "channel": "default" },
    { "name": "ark_web_search", "channel": "default" }
  ],
  "skills": [],
  "mcp_servers": [],
  "sub_agents": [],
  "custom": {}
}
```

Validate by listing or chatting:

```bash
cargo run -p fae-ctl -- --ws main agent
cargo run -p fae-ctl -- --ws main agent --id researcher --chat
```

For Rust-side creation, follow `examples/single_agent.rs`: build `AgentsEngine::default()`, get or build a workspace, create an `AgentConfigData`, call `ws.create_single_agent(agent_id, config.into_agent_config())`, then create a session with `session_call_stream`.

## Add A Skill

Runtime skill lookup uses:

```text
$FAE_HOME/skills/<skill_name>/SKILL.md
```

Repository-distributed skills live in:

```text
docs/skills/<skill_name>/SKILL.md
```

To add a skill for distribution:

1. Create `docs/skills/<skill_name>/SKILL.md`.
2. Use YAML frontmatter with at least `name` and `description`.
3. Keep the body procedural: what to inspect, what to edit, what commands to run, and failure checks.
4. If adding files under `docs/skills/`, regenerate `docs/site.txt`:
   ```bash
   cd docs
   python3 generate_site.py
   ```
5. Enable the skill in an agent:
   ```json
   "skills": [
     { "name": "weather", "channel": "default" },
     { "name": "fae", "channel": "default" }
   ]
   ```

Skill loading only reads frontmatter initially. The agent prompt will tell the model to call `read_file(path=$SKILL_PATH)` when a configured skill matches the task, so descriptions must be clear and trigger-specific.

## Configure MCP

MCP server configs are scanned from every JSON file under:

```text
$FAE_HOME/mcp/
```

Use this shape:

```json
{
  "mcpServers": {
    "remote_name": {
      "url": "https://example.com/mcp",
      "headers": {
        "Authorization": "Bearer <token>"
      }
    },
    "local_name": {
      "command": "npx",
      "args": ["-y", "@vendor/mcp-server"],
      "env": {
        "API_KEY": "<token>"
      }
    }
  }
}
```

Important details:

- Local `args` must be an array of strings.
- Remote MCP uses HTTP POST JSON-RPC and accepts `application/json, text/event-stream`.
- Enable MCP per agent through `config.json`:
  ```json
  "mcp_servers": [
    { "name": "remote_name", "channel": "default" }
  ]
  ```
- Exposed MCP tool names are prefixed as `<mcp_name>__<tool_name>`. For example, `gaode` tool `maps_text_search` becomes `gaode__maps_text_search`.
- If an MCP tool is unavailable, check that the server name is present in `$FAE_HOME/mcp/*.json`, the agent has it in `mcp_servers`, and the server can answer `initialize` and `tools/list`.

## Sub-Agents

Use `sub_agents` in `config.json` to expose specialist agents to the current agent:

```json
"sub_agents": ["researcher", "coder"]
```

The listed agents must exist in the same workspace. To make sub-agent calls work, ensure the parent agent has the `agent_exec_task` tool configured and the engine has that tool registered. The source prompt text may mention `sub_agent`, but the actual config field is `sub_agents`.

## Built-In Tools

Common tool config names:

- `execute_command`
- `read_file`
- `write_file`
- `list_directory`
- `apply_patch`
- `send_http_request`
- `execute_python`
- `todo_write`
- `ark_web_search`
- `scheduled_execution`
- `agent_exec_task`

Tool config uses unprefixed names. During model calls, FAE exposes tools as `<channel>__<tool_name>`, usually `default__read_file` or `default__execute_command`.

When adding a new Rust tool:

1. Implement `Tool` in `crates/fae-engine/src/tools/<name>.rs`.
2. Export it from `crates/fae-engine/src/tools/mod.rs`.
3. Register it in `AgentsEngine::default()` or the relevant custom engine builder.
4. Add it to an agent's `tools` list only after it is registered.
5. Run `cargo check -p fae-engine`.

## Model Configuration

Default model is read from `FAE_DEFAULT_MODEL`, then `OPENAI_DEFAULT_MODEL`, then falls back to `gpt-4o`.

OpenAI-compatible API configuration:

- `OPENAI_API_KEY`: consumed by the OpenAI client.
- `OPENAI_API_URL`: optional API base override.
- Model executor channel: `OpenAI-Compatible API`.

Reasoning effort values:

- `1`: minimal
- `2`: low
- `3`: medium
- `4`: high

`min_compact_window_size` controls when the agent asks the model to compact the conversation context.

## Verification Checklist

- For docs or skill-only edits, validate frontmatter and read the Markdown:
  ```bash
  python3 /Users/bytedance/.codex/skills/.system/skill-creator/scripts/quick_validate.py docs/skills/fae
  sed -n '1,240p' docs/skills/fae/SKILL.md
  ```
- For CLI behavior, run:
  ```bash
  cargo run -p fae-ctl -- --ws main agent
  ```
- For engine wiring, run:
  ```bash
  cargo check -p fae-engine
  cargo check -p fae-agent
  cargo check -p fae-ctl
  ```
- For a minimal runtime smoke test, run:
  ```bash
  cargo run -p examples --example basic
  ```

## Failure Triage

- `Agent config file not found`: create `$FAE_HOME/<workspace>/<agent_id>/config.json`.
- `Prompt file not found`: fix `prompt_dir`; remember relative paths are resolved under the agent directory.
- `Skill not found`: copy or create `$FAE_HOME/skills/<name>/SKILL.md` and ensure the agent `skills` entry uses the same `name`.
- `MCP Server config '<name>' not found`: add it to a JSON file under `$FAE_HOME/mcp/` and enable it in the agent `mcp_servers`.
- `Invalid mcp tool name format`: MCP calls must use names exposed by FAE, normally `<mcp_name>__<tool_name>`.
- `tools not found`: add the tool to the engine's `ToolSetImplMap` or remove it from the agent config.
