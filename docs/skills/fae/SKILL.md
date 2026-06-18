---
name: fae
description: Guide for Free Agent Engine workspace, agent, skill, MCP, sub-agent, and built-in tool configuration.
tags: ["fae", "agent", "skill", "mcp", "configuration"]
---

# FAE Skill

Use this skill when the user asks about Free Agent Engine (FAE) workspace layout, agent creation, agent configuration, skill creation, MCP configuration, sub-agent wiring, or default tools.

## FAE 目录

FAE home defaults to `~/.fae`. If environment variable `FAE_HOME` is set, use that path instead.

Typical layout after `fae init`:

```text
$FAE_HOME/
  prompt/
    <xxx-prompt-file>
  skills/
    <skill_name>/
      SKILL.md
  mcp/
    mcp_list.json
  <workspace>/
    <agent_id>/
      config.json
      <memory-and-session-files>
```

Important paths:

- `$FAE_HOME/prompt/`: shared prompt files.
- `$FAE_HOME/skills/<skill_name>/SKILL.md`: local skills loaded by name.
- `$FAE_HOME/mcp/*.json`: MCP server definitions. All JSON files in this directory are scanned.
- `$FAE_HOME/<workspace>/<agent_id>/config.json`: agent config.
- `$FAE_HOME/<workspace>/<agent_id>/`: agent directory, also used for memory and session files. A custom agent may keep a relative prompt file here, such as `system.txt`.

Default workspace name is `main`.

Repository source map when working inside `free-agent-engine`:

- `app/fae-ctl/`: FAE CLI. Commands are defined in `src/args.rs` and implemented in `src/init_project.rs`, `src/agents.rs`, and related files.
- `crates/fae-agent/`: core agent abstractions, file-backed config, session, memory, task, skill, MCP, and environment definitions.
- `crates/fae-engine/`: engine assembly, workspace loader, runtimes, executors, built-in tools, skill loader, and MCP client executor.
- `crates/async-openai/`: OpenAI-compatible client and generated API types.
- `docs/prompt/`: default prompts copied or referenced by initialized agents.
- `docs/skills/`: distributable skills downloaded by `fae init` through `docs/site.txt`.
- `examples/`: minimal Rust examples.

## Agent 创建流程

There is no dedicated `fae create-agent` CLI command in the current CLI. `fae init` creates the default workspace resources and default agents. To create an additional file-based agent, create the agent directory and `config.json` manually, or create it through the Rust API.

Manual file-based flow:

1. Choose workspace and agent id, for example `main` and `my-agent`.
2. Create `$FAE_HOME/main/my-agent/`.
3. Create a prompt file. Use either an absolute shared prompt path such as `$FAE_HOME/prompt/my-agent.txt`, or an agent-local relative path such as `system.txt`.
4. Create `$FAE_HOME/main/my-agent/config.json`.
5. Run `fae --ws main agent --id my-agent --chat` to chat with it.

Minimal `config.json`:

```json
{
  "name": "我的 Agent",
  "description": "说明这个 Agent 擅长什么，供列表和子 Agent 选择时使用。",
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
    { "name": "execute_command", "channel": "default" },
    { "name": "read_file", "channel": "default" },
    { "name": "write_file", "channel": "default" },
    { "name": "apply_patch", "channel": "default" },
    { "name": "todo_write", "channel": "default" }
  ],
  "skills": [
    { "name": "fae", "channel": "default" }
  ],
  "mcp_servers": [],
  "sub_agents": [],
  "custom": {}
}
```

Prompt path rule:

- If `prompt_dir` starts with `/`, it is used as an absolute path.
- Otherwise it is resolved relative to the agent directory: `$FAE_HOME/<workspace>/<agent_id>/<prompt_dir>`.

Default agents created by `fae init`:

- `fae-assistant`: coordination assistant. Skills: `weather`, `fae`. Sub-agents: `fae-aicoding`, `fae-claw`, `fae-aitest`.
- `fae-aicoding`: coding assistant. Skills: `drawio-skill`, `fae`.
- `fae-claw`: computer and office automation assistant. Skills: `weather`, `drawio-skill`, `fae`.
- `fae-aitest`: testing assistant. No default skills.

## Agent 配置修改流程

Agent configuration lives at `$FAE_HOME/<workspace>/<agent_id>/config.json`.

Common fields:

- `name`: display name.
- `description`: specialty summary. This is used when another agent sees this agent as a sub-agent.
- `model.model`: model name. Defaults can come from `FAE_DEFAULT_MODEL` or `OPENAI_DEFAULT_MODEL`.
- `model.channel`: model executor channel. Default file config uses `OpenAI-Compatible API`.
- `model.max_chat_history_round`: maximum chat history rounds.
- `model.reasoning_effort`: `1` minimal, `2` low, `3` medium, `4` high.
- `prompt_dir`: prompt file path. Absolute path is used directly; relative path is resolved inside the agent directory.
- `tools`: enabled built-in tools. Format: `{ "name": "<tool_name>", "channel": "default" }`.
- `skills`: enabled skills. Format: `{ "name": "<skill_name>", "channel": "default" }`.
- `mcp_servers`: enabled MCP servers. Format: `{ "name": "<mcp_name>", "channel": "default" }`.
- `sub_agents`: list of child agent ids.
- `custom`: extra string key-value config.

Modification flow:

1. Open `$FAE_HOME/<workspace>/<agent_id>/config.json`.
2. Edit only the needed field.
3. If changing `prompt_dir`, verify the target prompt file exists before starting the agent.
4. If adding a tool, skill, MCP server, or sub-agent, verify that the referenced name exists.
5. Restart the `fae` process or recreate the engine/session after config changes. File-based agents are cached after load, so changes may not affect an already running process.

## Skill 创建流程

Skills are Markdown files loaded from `$FAE_HOME/skills/<skill_name>/SKILL.md`.

Create a skill:

1. Create directory `$FAE_HOME/skills/<skill_name>/`.
2. Create `$FAE_HOME/skills/<skill_name>/SKILL.md`.
3. Add optional YAML front matter at the top.
4. Write clear usage instructions in the Markdown body.
5. Add the skill to an agent config under `skills`.
6. Restart or reload the agent process.

Recommended `SKILL.md` shape:

```markdown
---
name: my-skill
description: What this skill helps the agent do.
version: "0.1.0"
tags: ["example"]
---

# My Skill

Use this skill when ...

## Workflow

1. ...
```

Supported front matter fields include `name`, `description`, `version`, `metadata`, `author`, `trigger`, and `tags`. If front matter is absent, FAE still loads the skill by the configured skill name, but a header is recommended because it makes the skill discoverable.

Enable a skill in an agent:

```json
{
  "skills": [
    { "name": "my-skill", "channel": "default" }
  ]
}
```

## MCP 创建流程

MCP server configuration lives in JSON files under `$FAE_HOME/mcp/`. The default file is `$FAE_HOME/mcp/mcp_list.json`, but the executor scans every `*.json` file in the MCP directory.

Config shape:

```json
{
  "mcpServers": {
    "remote_example": {
      "url": "https://example.com/mcp",
      "headers": {
        "Authorization": "Bearer token"
      }
    },
    "local_example": {
      "command": "npx",
      "args": ["-y", "some-mcp-server"],
      "env": {
        "API_KEY": "value"
      }
    }
  }
}
```

Rules:

- Remote MCP uses `url` and optional `headers`.
- Local MCP uses `command`, `args`, and optional `env`.
- `args` must be an array of strings.
- MCP server names are the keys under `mcpServers`, for example `remote_example`.

Enable MCP for an agent:

```json
{
  "mcp_servers": [
    { "name": "remote_example", "channel": "default" }
  ]
}
```

When enabled, FAE lists the server tools through MCP and exposes them to the model with names prefixed as `<mcp_name>__<tool_name>`.

## 子 Agent 添加流程

Sub-agents are configured in the parent agent's `sub_agents` field. The field name is `sub_agents`.

Flow:

1. Ensure the child agent exists at `$FAE_HOME/<workspace>/<child_agent_id>/config.json`.
2. Give the child agent a useful `description`, because the parent sees it as the child's specialty.
3. Ensure the parent has the `agent_exec_task` tool enabled.
4. Add the child id to the parent config.
5. Restart or reload the parent agent process.

Example:

```json
{
  "tools": [
    { "name": "agent_exec_task", "channel": "default" }
  ],
  "sub_agents": ["fae-aicoding", "fae-aitest"]
}
```

The parent agent delegates work by calling `agent_exec_task`. The child agent reports task progress and final result through the same task lifecycle.

## FAE 默认工具

The default engine registers these built-in tools on channel `default`.

| Tool | Purpose | Key arguments |
| --- | --- | --- |
| `execute_command` | Execute a shell command. Dangerous first words require a confirmation code. | `command`, optional `cwd`, optional `confirm_code` |
| `read_file` | Read a file. | `path`, optional `with_line_numbers` |
| `write_file` | Write content to a file in allowed directories. | `path`, `content` |
| `list_directory` | List directory entries. | `path` |
| `apply_patch` | Apply a unified diff patch in allowed directories. | `patch` |
| `send_http_request` | Send an HTTP request and return response text. | `url`, optional `method`, `headers`, `body` |
| `execute_python` | Execute a Python script with `python3`. | `script` |
| `todo_write` | Maintain a structured todo list for the current work session. | `merge`, `todos`, optional `summary` |
| `ark_web_search` | Web or image search through Volcano Engine Ark. Requires `ARK_WEB_SEARCH_APIKEY`. | `query`, `search_type`, optional filters |
| `scheduled_execution` | Submit one-time or recurring scheduled tasks with cron expressions. | `cron_expression`, `execute_once`, `task_content` |
| `agent_exec_task` | Create and update delegated agent tasks. | task lifecycle JSON from `AgentTaskStatus` |

Default `AgentConfigData` enables these tools:

```text
execute_command
read_file
write_file
apply_patch
send_http_request
execute_python
todo_write
ark_web_search
scheduled_execution
agent_exec_task
```

`list_directory` is registered by the default engine and is enabled in several default CLI-created agents, but it is not in `AgentConfigData::default()` at the time this skill was written.

Default CLI-created agent tool sets:

- `fae-assistant`: `read_file`, `write_file`, `list_directory`, `send_http_request`, `ark_web_search`, `todo_write`, `scheduled_execution`, `agent_exec_task`, `apply_patch`, `execute_command`.
- `fae-aicoding`: `execute_command`, `read_file`, `write_file`, `list_directory`, `apply_patch`, `execute_python`, `todo_write`, `agent_exec_task`.
- `fae-claw`: `execute_command`, `read_file`, `write_file`, `list_directory`, `send_http_request`, `execute_python`, `todo_write`, `ark_web_search`, `scheduled_execution`, `agent_exec_task`.
- `fae-aitest`: `execute_command`, `read_file`, `list_directory`, `send_http_request`, `execute_python`, `todo_write`, `agent_exec_task`.

## 常用 CLI

```bash
fae init
fae --ws main agent
fae --ws main agent --id fae-assistant --chat
fae --ws main agent --id fae-assistant --user master --history
fae uninstall
```

## Operational Notes

- Prefer editing JSON with valid JSON syntax; comments are not allowed in `config.json` or MCP JSON files.
- Keep tool, skill, MCP, and sub-agent names exact. FAE looks them up by string name.
- If a configured tool or skill does not exist, agent initialization can fail.
- If a configured MCP server cannot start or cannot list tools, agent initialization with that MCP can fail.
- Keep agent descriptions concise and specific because they are shown to parent agents for delegation decisions.
