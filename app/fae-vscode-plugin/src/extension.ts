import * as vscode from 'vscode';
import { buildTerminalCommand, FaeCliSession, getSessionOptions } from './faeCliSession';

type WebviewMessage =
  | { type: 'ready' }
  | { type: 'send'; text: string }
  | { type: 'newConversation' }
  | { type: 'openSettings' }
  | { type: 'openTerminal' };

export function activate(context: vscode.ExtensionContext): void {
  const provider = new FaeChatViewProvider(context.extensionUri);

  context.subscriptions.push(
    provider,
    vscode.window.registerWebviewViewProvider(FaeChatViewProvider.viewType, provider, {
      webviewOptions: {
        retainContextWhenHidden: true
      }
    }),
    vscode.commands.registerCommand('fae.openChat', async () => {
      await vscode.commands.executeCommand('workbench.view.extension.fae');
    }),
    vscode.commands.registerCommand('fae.newConversation', () => provider.newConversation()),
    vscode.commands.registerCommand('fae.openSettings', () => openFaeSettings()),
    vscode.commands.registerCommand('fae.openInTerminal', () => openFaeInTerminal(false))
  );
}

export function deactivate(): void {
  // Sessions are owned by the webview provider and disposed through subscriptions.
}

class FaeChatViewProvider implements vscode.WebviewViewProvider, vscode.Disposable {
  static readonly viewType = 'fae.chatView';

  private view?: vscode.WebviewView;
  private readonly session = new FaeCliSession();
  private disposables: vscode.Disposable[] = [];
  private conversationStarted = false;

  constructor(private readonly extensionUri: vscode.Uri) {
    this.disposables.push(
      this.session.onEvent(event => {
        if (!this.view) {
          return;
        }

        if (event.type === 'output') {
          this.post({ type: 'agentChunk', text: event.text });
        } else if (event.type === 'error') {
          this.post({ type: 'agentError', text: event.text });
        } else {
          this.conversationStarted = false;
          const detail = event.signal ? `signal ${event.signal}` : `code ${event.code ?? 'unknown'}`;
          this.post({ type: 'status', text: `FAE session exited (${detail}).` });
        }
      })
    );
  }

  resolveWebviewView(webviewView: vscode.WebviewView): void {
    this.view = webviewView;

    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [this.extensionUri]
    };
    webviewView.webview.html = this.getHtml(webviewView.webview);

    this.disposables.push(
      webviewView.webview.onDidReceiveMessage((message: WebviewMessage) => {
        void this.handleMessage(message);
      })
    );
  }

  newConversation(): void {
    this.conversationStarted = false;
    this.post({ type: 'reset' });
    this.startSession(true);
  }

  dispose(): void {
    this.session.dispose();
    for (const disposable of this.disposables) {
      disposable.dispose();
    }
    this.disposables = [];
  }

  private async handleMessage(message: WebviewMessage): Promise<void> {
    switch (message.type) {
      case 'ready':
        this.postConfig();
        break;
      case 'send':
        await this.sendPrompt(message.text);
        break;
      case 'newConversation':
        this.newConversation();
        break;
      case 'openSettings':
        await openFaeSettings();
        break;
      case 'openTerminal':
        openFaeInTerminal(false);
        break;
    }
  }

  private async sendPrompt(text: string): Promise<void> {
    const prompt = text.trim();
    if (!prompt) {
      return;
    }

    if (!this.conversationStarted) {
      this.startSession(false);
    }

    this.post({ type: 'userMessage', text: prompt });

    try {
      this.session.send(prompt);
      this.post({ type: 'status', text: 'Waiting for FAE...' });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.post({ type: 'agentError', text: message });
    }
  }

  private startSession(newConversation: boolean): void {
    const options = getSessionOptions();
    this.conversationStarted = true;
    this.postConfig();
    this.post({
      type: 'status',
      text: `Starting ${options.command} (${options.workspace}/${options.agentId})...`
    });

    try {
      this.session.start(options, newConversation);
    } catch (error) {
      this.conversationStarted = false;
      const message = error instanceof Error ? error.message : String(error);
      this.post({ type: 'agentError', text: message });
    }
  }

  private postConfig(): void {
    const options = getSessionOptions();
    this.post({
      type: 'config',
      command: options.command,
      workspace: options.workspace,
      agentId: options.agentId,
      userId: options.userId,
      cwd: options.cwd
    });
  }

  private post(message: Record<string, unknown>): void {
    void this.view?.webview.postMessage(message);
  }

  private getHtml(webview: vscode.Webview): string {
    const nonce = getNonce();
    const codiconsUri = webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, 'node_modules', '@vscode', 'codicons', 'dist', 'codicon.css')
    );

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; font-src ${webview.cspSource}; script-src 'nonce-${nonce}';">
  <link href="${codiconsUri}" rel="stylesheet">
  <title>FAE Chat</title>
  <style>
    :root {
      color-scheme: light dark;
    }

    * {
      box-sizing: border-box;
    }

    body {
      margin: 0;
      height: 100vh;
      color: var(--vscode-foreground);
      background: var(--vscode-sideBar-background);
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
    }

    .app {
      display: grid;
      grid-template-rows: auto 1fr auto;
      height: 100vh;
    }

    .topbar {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 8px;
      padding: 10px 12px;
      border-bottom: 1px solid var(--vscode-sideBarSectionHeader-border);
      background: var(--vscode-sideBarSectionHeader-background);
    }

    .title {
      min-width: 0;
    }

    .title strong {
      display: block;
      font-size: 13px;
    }

    .title span {
      display: block;
      margin-top: 2px;
      overflow: hidden;
      color: var(--vscode-descriptionForeground);
      font-size: 11px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .actions {
      display: flex;
      gap: 4px;
    }

    button {
      border: 0;
      border-radius: 5px;
      color: var(--vscode-button-foreground);
      background: var(--vscode-button-background);
      cursor: pointer;
      font: inherit;
    }

    button:hover {
      background: var(--vscode-button-hoverBackground);
    }

    .icon-button {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      width: 28px;
      height: 28px;
      padding: 0;
      color: var(--vscode-icon-foreground);
      background: transparent;
    }

    .icon-button:hover {
      background: var(--vscode-toolbar-hoverBackground);
    }

    .messages {
      overflow-y: auto;
      padding: 14px 12px 18px;
    }

    .empty {
      display: grid;
      gap: 10px;
      align-content: center;
      min-height: 100%;
      color: var(--vscode-descriptionForeground);
      text-align: center;
    }

    .empty h2 {
      margin: 0;
      color: var(--vscode-foreground);
      font-size: 18px;
      font-weight: 600;
    }

    .message {
      margin: 0 0 12px;
      padding: 10px 11px;
      border: 1px solid var(--vscode-editorWidget-border);
      border-radius: 10px;
      background: var(--vscode-editorWidget-background);
      white-space: pre-wrap;
      word-break: break-word;
    }

    .message.user {
      border-color: var(--vscode-focusBorder);
      background: color-mix(in srgb, var(--vscode-button-background) 18%, transparent);
    }

    .message.error {
      border-color: var(--vscode-inputValidation-errorBorder);
      background: var(--vscode-inputValidation-errorBackground);
    }

    .message .role {
      margin-bottom: 6px;
      color: var(--vscode-descriptionForeground);
      font-size: 11px;
      font-weight: 600;
      text-transform: uppercase;
      letter-spacing: 0.04em;
    }

    .status {
      min-height: 18px;
      padding: 0 12px 8px;
      color: var(--vscode-descriptionForeground);
      font-size: 11px;
    }

    .composer {
      display: grid;
      gap: 8px;
      padding: 10px 12px 12px;
      border-top: 1px solid var(--vscode-sideBarSectionHeader-border);
      background: var(--vscode-sideBar-background);
    }

    textarea {
      width: 100%;
      min-height: 72px;
      max-height: 200px;
      padding: 9px 10px;
      resize: vertical;
      border: 1px solid var(--vscode-input-border, transparent);
      border-radius: 8px;
      outline: none;
      color: var(--vscode-input-foreground);
      background: var(--vscode-input-background);
      font: inherit;
      line-height: 1.45;
    }

    textarea:focus {
      border-color: var(--vscode-focusBorder);
    }

    .send-row {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 8px;
    }

    .hint {
      color: var(--vscode-descriptionForeground);
      font-size: 11px;
    }

    .send {
      padding: 6px 12px;
      font-weight: 600;
    }
  </style>
</head>
<body>
  <main class="app">
    <header class="topbar">
      <div class="title">
        <strong>FAE Chat</strong>
        <span id="config">Loading configuration...</span>
      </div>
      <div class="actions">
        <button class="icon-button" id="new" title="New Conversation"><span class="codicon codicon-add"></span></button>
        <button class="icon-button" id="terminal" title="Open in Terminal"><span class="codicon codicon-terminal"></span></button>
        <button class="icon-button" id="settings" title="Settings"><span class="codicon codicon-settings-gear"></span></button>
      </div>
    </header>
    <section class="messages" id="messages">
      <div class="empty" id="empty">
        <h2>Ask FAE anything</h2>
        <p>Use the configured FAE agent directly from the VS Code sidebar.</p>
      </div>
    </section>
    <div>
      <div class="status" id="status"></div>
      <form class="composer" id="form">
        <textarea id="prompt" placeholder="Message FAE..." rows="3"></textarea>
        <div class="send-row">
          <span class="hint">Enter sends, Shift+Enter adds a line</span>
          <button class="send" type="submit">Send</button>
        </div>
      </form>
    </div>
  </main>
  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();
    const messages = document.getElementById('messages');
    const empty = document.getElementById('empty');
    const status = document.getElementById('status');
    const config = document.getElementById('config');
    const prompt = document.getElementById('prompt');
    const form = document.getElementById('form');
    let currentAgentMessage;

    document.getElementById('new').addEventListener('click', () => vscode.postMessage({ type: 'newConversation' }));
    document.getElementById('settings').addEventListener('click', () => vscode.postMessage({ type: 'openSettings' }));
    document.getElementById('terminal').addEventListener('click', () => vscode.postMessage({ type: 'openTerminal' }));

    form.addEventListener('submit', event => {
      event.preventDefault();
      sendPrompt();
    });

    prompt.addEventListener('keydown', event => {
      if (event.key === 'Enter' && !event.shiftKey) {
        event.preventDefault();
        sendPrompt();
      }
    });

    window.addEventListener('message', event => {
      const message = event.data;
      if (message.type === 'config') {
        config.textContent = message.workspace + '/' + message.agentId + ' · ' + message.cwd;
      } else if (message.type === 'userMessage') {
        currentAgentMessage = undefined;
        addMessage('You', message.text, 'user');
      } else if (message.type === 'agentChunk') {
        appendAgentChunk(message.text);
        status.textContent = '';
      } else if (message.type === 'agentError') {
        currentAgentMessage = undefined;
        addMessage('Error', message.text, 'error');
        status.textContent = '';
      } else if (message.type === 'status') {
        status.textContent = message.text;
      } else if (message.type === 'reset') {
        resetMessages();
      }
    });

    vscode.postMessage({ type: 'ready' });

    function sendPrompt() {
      const text = prompt.value.trim();
      if (!text) {
        return;
      }

      prompt.value = '';
      vscode.postMessage({ type: 'send', text });
    }

    function appendAgentChunk(text) {
      hideEmpty();
      if (!currentAgentMessage) {
        currentAgentMessage = addMessage('FAE', '', 'agent');
      }
      const body = currentAgentMessage.querySelector('.body');
      body.textContent = body.textContent ? body.textContent + '\\n' + text : text;
      scrollToBottom();
    }

    function addMessage(role, text, kind) {
      hideEmpty();
      const article = document.createElement('article');
      article.className = 'message ' + kind;
      const roleEl = document.createElement('div');
      roleEl.className = 'role';
      roleEl.textContent = role;
      const body = document.createElement('div');
      body.className = 'body';
      body.textContent = text;
      article.append(roleEl, body);
      messages.appendChild(article);
      scrollToBottom();
      return article;
    }

    function resetMessages() {
      currentAgentMessage = undefined;
      messages.replaceChildren(empty);
      empty.style.display = 'grid';
      status.textContent = 'Started a new FAE conversation.';
    }

    function hideEmpty() {
      empty.style.display = 'none';
    }

    function scrollToBottom() {
      messages.scrollTop = messages.scrollHeight;
    }
  </script>
</body>
</html>`;
  }
}

async function openFaeSettings(): Promise<void> {
  await vscode.commands.executeCommand('workbench.action.openSettings', '@ext:free-agent-engine.fae-vscode-plugin');
}

function openFaeInTerminal(newConversation: boolean): void {
  const options = getSessionOptions();
  const terminal = vscode.window.createTerminal({
    name: `FAE: ${options.agentId}`,
    cwd: options.cwd
  });
  terminal.show();
  terminal.sendText(buildTerminalCommand(options, newConversation), true);
}

function getNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let nonce = '';
  for (let i = 0; i < 32; i += 1) {
    nonce += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return nonce;
}
