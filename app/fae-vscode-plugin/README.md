# FAE VS Code Plugin

FAE VS Code Plugin lets you chat with a Free Agent Engine agent from the VS Code sidebar. It provides a Codex-like chat surface, new conversation support, settings shortcuts, and a terminal fallback for the current FAE CLI.

## Features

- FAE activity bar entry with a chat webview.
- Send messages to the configured FAE agent.
- Start a new conversation with `--new-session`.
- Open FAE settings from the chat title bar.
- Open the same agent in a VS Code terminal when you need the native CLI UI.

## Requirements

Install and initialize FAE first:

```bash
fae init
```

The plugin uses the `fae` CLI by default:

```bash
fae --ws main agent --id fae-assistant --user master --stdio
```

## Development

```bash
npm install
npm run compile
```

Open this folder in VS Code and press `F5` to launch an Extension Development Host.

## Settings

- `fae.command`: path to the FAE CLI executable. Default: `fae`.
- `fae.workspace`: FAE workspace name. Default: `main`.
- `fae.agentId`: FAE agent id. Default: `fae-assistant`.
- `fae.userId`: FAE user id. Default: `master`.
- `fae.cwd`: launch directory. Default: current VS Code workspace folder.
- `fae.extraArgs`: extra CLI arguments appended to the launch command.
- `fae.usePseudoTerminal`: legacy `script` wrapper option. The webview chat uses `--stdio` by default and normally does not need this.

## Notes

The webview chat uses the FAE CLI `--stdio` mode so it can run without a real terminal. If you need the native terminal UI, use `FAE: Open Agent in Terminal` from the chat title bar.
