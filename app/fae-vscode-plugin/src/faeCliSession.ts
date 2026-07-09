import { ChildProcessWithoutNullStreams, spawn } from 'child_process';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';

export interface FaeSessionOptions {
  command: string;
  workspace: string;
  agentId: string;
  userId: string;
  cwd: string;
  extraArgs: string[];
  usePseudoTerminal: boolean;
}

export type FaeSessionEvent =
  | { type: 'output'; text: string }
  | { type: 'error'; text: string }
  | { type: 'exit'; code: number | null; signal: NodeJS.Signals | null };

export class FaeCliSession {
  private child?: ChildProcessWithoutNullStreams;
  private readonly listeners = new Set<(event: FaeSessionEvent) => void>();
  private lastOptions?: FaeSessionOptions;

  onEvent(listener: (event: FaeSessionEvent) => void): vscode.Disposable {
    this.listeners.add(listener);
    return new vscode.Disposable(() => this.listeners.delete(listener));
  }

  start(options: FaeSessionOptions, newConversation = false): void {
    this.dispose();
    this.lastOptions = options;

    const args = this.buildArgs(options, newConversation);
    const command = options.command.trim() || 'fae';
    const spawnSpec = this.buildSpawnSpec(command, args, false);

    this.child = spawn(spawnSpec.command, spawnSpec.args, {
      cwd: options.cwd,
      env: {
        ...process.env,
        TERM: process.env.TERM || 'xterm-256color',
        COLORTERM: process.env.COLORTERM || 'truecolor'
      },
      shell: false
    });

    this.child.stdout.on('data', chunk => this.emitOutput(chunk.toString()));
    this.child.stderr.on('data', chunk => this.emit({ type: 'error', text: cleanTerminalText(chunk.toString()) }));
    this.child.on('error', error => this.emit({ type: 'error', text: error.message }));
    this.child.on('exit', (code, signal) => {
      this.emit({ type: 'exit', code, signal });
      this.child = undefined;
    });
  }

  restart(newConversation = false): void {
    if (this.lastOptions) {
      this.start(this.lastOptions, newConversation);
    }
  }

  send(message: string): void {
    if (!this.child || this.child.killed) {
      throw new Error('FAE session is not running.');
    }

    this.child.stdin.write(`${message.replace(/\r\n/g, '\n')}\n`);
  }

  dispose(): void {
    if (!this.child) {
      return;
    }

    const child = this.child;
    this.child = undefined;

    try {
      child.stdin.write('/exit\n');
    } catch {
      // Ignore shutdown races.
    }

    setTimeout(() => {
      if (!child.killed) {
        child.kill();
      }
    }, 500);
  }

  private buildArgs(options: FaeSessionOptions, newConversation: boolean): string[] {
    const args = [
      '--ws',
      options.workspace,
      'agent',
      '--id',
      options.agentId,
      '--user',
      options.userId,
      '--stdio'
    ];

    if (newConversation) {
      args.push('--new-session');
    }

    args.push(...options.extraArgs);
    return args;
  }

  private buildSpawnSpec(command: string, args: string[], usePseudoTerminal: boolean): { command: string; args: string[] } {
    const platform = os.platform();
    if (!usePseudoTerminal || platform === 'win32') {
      return { command, args };
    }

    const quoted = [command, ...args].map(shellQuote).join(' ');
    if (platform === 'linux') {
      return {
        command: 'script',
        args: ['-q', '-c', quoted, '/dev/null']
      };
    }

    return {
      command: 'script',
      args: ['-q', '/dev/null', quoted]
    };
  }

  private emitOutput(text: string): void {
    const clean = cleanTerminalText(text);
    if (clean.trim().length > 0) {
      this.emit({ type: 'output', text: clean });
    }
  }

  private emit(event: FaeSessionEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }
}

export function getSessionOptions(): FaeSessionOptions {
  const config = vscode.workspace.getConfiguration('fae');
  const workspaceFolder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  const configuredCwd = config.get<string>('cwd', '').trim();

  return {
    command: config.get<string>('command', 'fae'),
    workspace: config.get<string>('workspace', 'main'),
    agentId: config.get<string>('agentId', 'fae-assistant'),
    userId: config.get<string>('userId', 'master'),
    cwd: expandHome(configuredCwd || workspaceFolder || process.cwd()),
    extraArgs: config.get<string[]>('extraArgs', []),
    usePseudoTerminal: config.get<boolean>('usePseudoTerminal', false)
  };
}

export function buildTerminalCommand(options: FaeSessionOptions, newConversation = false): string {
  const args = [
    '--ws',
    options.workspace,
    'agent',
    '--id',
    options.agentId,
    '--user',
    options.userId,
    '--chat',
    ...(newConversation ? ['--new-session'] : []),
    ...options.extraArgs
  ];

  return [options.command, ...args].map(shellQuote).join(' ');
}

function expandHome(value: string): string {
  if (value === '~') {
    return os.homedir();
  }

  if (value.startsWith(`~${path.sep}`)) {
    return path.join(os.homedir(), value.slice(2));
  }

  return value;
}

function shellQuote(value: string): string {
  if (/^[A-Za-z0-9_./:=@-]+$/.test(value)) {
    return value;
  }

  return `'${value.replace(/'/g, `'\\''`)}'`;
}

function cleanTerminalText(value: string): string {
  return value
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
    .replace(/\x1b\][^\x07]*(\x07|\x1b\\)/g, '')
    .replace(/\r\n/g, '\n')
    .replace(/\r/g, '\n')
    .replace(/\n{3,}/g, '\n\n')
    .replace(/[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]\s+[A-Za-z]+\.{3}/g, '')
    .trimEnd();
}
