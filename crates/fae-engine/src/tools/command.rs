use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use fae_agent::{Ctx, ToolRequest, ToolResponse, Tools};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use super::{
    APPLY_PATCH, DEFAULT_CHANNEL, EXECUTE_COMMAND, effective_tool_name, ok_json, parse_arguments,
    request_tool_name, unsupported_tool,
};

#[derive(Debug, Default)]
pub struct ExecuteCommandTool;

#[derive(Debug, Deserialize)]
struct ExecuteCommandArgs {
    command: String,
    cwd: Option<PathBuf>,
    timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
struct CommandResult {
    command: String,
    cwd: Option<String>,
    status_code: Option<i32>,
    success: bool,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

#[async_trait::async_trait]
impl Tools for ExecuteCommandTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != EXECUTE_COMMAND {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": EXECUTE_COMMAND,
            "description": "Execute a shell command and return stdout, stderr, and exit status. Destructive commands may only target FAE_HOST, the process working directory, or the system temporary directory.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute." },
                    "cwd": { "type": "string", "description": "Optional working directory." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Optional command timeout in seconds. Defaults to 60." }
                },
                "required": ["command"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != EXECUTE_COMMAND {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: ExecuteCommandArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };

        if let Err(err) = validate_command_safety(&args.command, args.cwd.as_deref()) {
            return Ok(ToolResponse::with_error(403, err));
        }

        run_shell_command(args.command, args.cwd, args.timeout_secs).await
    }
}

#[derive(Debug, Default)]
pub struct ApplyPatchTool;

#[derive(Debug, Deserialize)]
struct ApplyPatchArgs {
    patch: String,
    cwd: Option<PathBuf>,
    strip: Option<u8>,
    timeout_secs: Option<u64>,
}

#[async_trait::async_trait]
impl Tools for ApplyPatchTool {
    fn channel(&self) -> &str {
        DEFAULT_CHANNEL
    }

    async fn desc(&self, _ctx: &Ctx, tool_name: &str) -> anyhow::Result<Value> {
        if effective_tool_name(tool_name) != APPLY_PATCH {
            return Err(unsupported_tool(tool_name));
        }

        Ok(json!({
            "name": APPLY_PATCH,
            "description": "Apply a unified diff patch using the system patch command.",
            "parameters": {
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "Unified diff content." },
                    "cwd": { "type": "string", "description": "Optional working directory." },
                    "strip": { "type": "integer", "minimum": 0, "description": "Path components to strip. Defaults to 1." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Optional timeout in seconds. Defaults to 60." }
                },
                "required": ["patch"]
            }
        }))
    }

    async fn exec(&self, _ctx: &Ctx, req: ToolRequest) -> anyhow::Result<ToolResponse> {
        if request_tool_name(&req) != APPLY_PATCH {
            return Err(unsupported_tool(req.get_tool_name()));
        }

        let args: ApplyPatchArgs = match parse_arguments(req.get_arguments()) {
            Ok(args) => args,
            Err(resp) => return Ok(resp),
        };

        let strip = args.strip.unwrap_or(1);
        let mut command = Command::new("patch");
        command
            .arg("--forward")
            .arg("--batch")
            .arg(format!("-p{strip}"))
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(cwd) = &args.cwd {
            command.current_dir(cwd);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => return Ok(ToolResponse::with_error(500, err.to_string())),
        };

        if let Some(mut stdin) = child.stdin.take() {
            if let Err(err) = stdin.write_all(args.patch.as_bytes()).await {
                return Ok(ToolResponse::with_error(500, err.to_string()));
            }
        }

        let timeout_secs = args.timeout_secs.unwrap_or(60);
        let output =
            match timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(err)) => return Ok(ToolResponse::with_error(500, err.to_string())),
                Err(_) => {
                    return ok_json(CommandResult {
                        command: "patch".to_string(),
                        cwd: args.cwd.map(|cwd| cwd.display().to_string()),
                        status_code: None,
                        success: false,
                        stdout: String::new(),
                        stderr: format!("command timed out after {timeout_secs}s"),
                        timed_out: true,
                    });
                }
            };

        ok_json(CommandResult {
            command: "patch".to_string(),
            cwd: args.cwd.map(|cwd| cwd.display().to_string()),
            status_code: output.status.code(),
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            timed_out: false,
        })
    }
}

async fn run_shell_command(
    command: String,
    cwd: Option<PathBuf>,
    timeout_secs: Option<u64>,
) -> anyhow::Result<ToolResponse> {
    #[cfg(target_os = "windows")]
    let mut child = {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(&command);
        cmd
    };

    #[cfg(not(target_os = "windows"))]
    let mut child = {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&command);
        cmd
    };

    child.stdout(Stdio::piped()).stderr(Stdio::piped());
    child.kill_on_drop(true);
    if let Some(cwd) = &cwd {
        child.current_dir(cwd);
    }

    let timeout_secs = timeout_secs.unwrap_or(60);
    let output = match timeout(Duration::from_secs(timeout_secs), child.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Ok(ToolResponse::with_error(500, err.to_string())),
        Err(_) => {
            return ok_json(CommandResult {
                command,
                cwd: cwd.map(|cwd| cwd.display().to_string()),
                status_code: None,
                success: false,
                stdout: String::new(),
                stderr: format!("command timed out after {timeout_secs}s"),
                timed_out: true,
            });
        }
    };

    ok_json(CommandResult {
        command,
        cwd: cwd.map(|cwd| cwd.display().to_string()),
        status_code: output.status.code(),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        timed_out: false,
    })
}

const DESTRUCTIVE_COMMANDS: &[&str] = &[
    "rm", "rmdir", "unlink", "mv", "chmod", "chown", "chgrp", "truncate", "shred", "dd", "fdisk",
];

fn validate_command_safety(command: &str, cwd: Option<&Path>) -> Result<(), String> {
    let process_cwd = std::env::current_dir()
        .map_err(|err| format!("cannot determine current directory: {err}"))?;
    let command_cwd = cwd.unwrap_or(&process_cwd);
    let safe_roots = [
        process_cwd.clone(),
        crate::default_fae_host(),
        std::env::temp_dir(),
    ]
    .map(|path| normalize_path(&path, &process_cwd));

    validate_shell(command, command_cwd, &safe_roots)
}

fn validate_shell(command: &str, cwd: &Path, safe_roots: &[PathBuf]) -> Result<(), String> {
    if (command.contains("$(") || command.contains('`')) && mentions_destructive_command(command) {
        return Err("destructive commands cannot be used with command substitution".to_string());
    }

    let segments = split_shell_segments(command)?;
    if segments.len() > 1
        && segments.iter().any(|segment| {
            shell_words(segment)
                .is_ok_and(|words| command_name(&words).is_some_and(|(_, name)| name == "cd"))
        })
        && segments
            .iter()
            .any(|segment| segment_is_destructive(segment))
    {
        return Err(
            "changing directories inside a shell expression containing a destructive command is not allowed; use the cwd argument instead"
                .to_string(),
        );
    }

    for segment in segments {
        validate_segment(segment, cwd, safe_roots)?;
    }
    Ok(())
}

fn validate_segment(segment: &str, cwd: &Path, safe_roots: &[PathBuf]) -> Result<(), String> {
    let words = shell_words(segment)?;
    let Some((command_index, name)) = command_name(&words) else {
        return Ok(());
    };

    if name == "eval" {
        return validate_shell(&words[command_index + 1..].join(" "), cwd, safe_roots);
    }

    if matches!(name, "sh" | "bash" | "zsh" | "dash" | "ksh") {
        if let Some(index) = words[command_index + 1..].iter().position(|word| {
            word == "-c"
                || word
                    .strip_prefix('-')
                    .is_some_and(|flags| !flags.starts_with('-') && flags.contains('c'))
        }) {
            let script_index = command_index + index + 2;
            let script = words
                .get(script_index)
                .ok_or_else(|| "shell -c is missing the command string".to_string())?;
            return validate_shell(script, cwd, safe_roots);
        }
    }

    if name == "xargs" && mentions_destructive_command(segment) {
        return Err(
            "xargs cannot invoke destructive commands because its targets are dynamic".to_string(),
        );
    }

    if name == "find" {
        return validate_find(&words[command_index + 1..], cwd, safe_roots);
    }

    if !is_destructive_name(name) {
        return Ok(());
    }

    let args = &words[command_index + 1..];
    let targets = destructive_targets(name, args)?;
    for target in targets {
        validate_target(name, target, cwd, safe_roots)?;
    }
    Ok(())
}

fn destructive_targets<'a>(name: &str, args: &'a [String]) -> Result<Vec<&'a str>, String> {
    if name == "dd" {
        return Ok(args
            .iter()
            .filter_map(|arg| arg.strip_prefix("of="))
            .collect());
    }

    let mut operands = Vec::new();
    let mut options_done = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if !options_done && arg == "--" {
            options_done = true;
        } else if name == "mv" && matches!(arg.as_str(), "-t" | "--target-directory") {
            index += 1;
            let target = args
                .get(index)
                .ok_or_else(|| format!("`{arg}` is missing its directory argument"))?;
            operands.push(target.as_str());
        } else if name == "mv" && arg.starts_with("--target-directory=") {
            operands.push(arg.trim_start_matches("--target-directory="));
        } else if !options_done && arg.starts_with('-') {
        } else {
            operands.push(arg.as_str());
        }
        index += 1;
    }

    let skipped = match name {
        "chmod" | "chown" | "chgrp" | "diskutil" => 1,
        _ => 0,
    };
    Ok(operands.into_iter().skip(skipped).collect())
}

fn validate_find(args: &[String], cwd: &Path, safe_roots: &[PathBuf]) -> Result<(), String> {
    let destructive = args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
        )
    });
    if !destructive {
        return Ok(());
    }
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
    {
        return Err("find -exec/-ok is not allowed with destructive operations".to_string());
    }

    let roots: Vec<&str> = args
        .iter()
        .take_while(|arg| !arg.starts_with('-') && arg.as_str() != "!")
        .map(String::as_str)
        .collect();
    for root in roots
        .iter()
        .copied()
        .chain((roots.is_empty()).then_some("."))
    {
        validate_target("find -delete", root, cwd, safe_roots)?;
    }
    Ok(())
}

fn validate_target(
    command: &str,
    target: &str,
    cwd: &Path,
    safe_roots: &[PathBuf],
) -> Result<(), String> {
    if target.contains('$') || target.contains('`') || target.starts_with('~') {
        return Err(format!(
            "`{command}` target `{target}` cannot be resolved safely"
        ));
    }

    let target_path = normalize_path(Path::new(target), cwd);
    if safe_roots.iter().any(|root| target_path.starts_with(root)) {
        return Ok(());
    }
    Err(format!(
        "`{command}` target `{}` is outside the allowed directories",
        target_path.display()
    ))
}

fn normalize_path(path: &Path, base: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    canonicalize_existing_ancestor(&normalized)
}

fn canonicalize_existing_ancestor(path: &Path) -> PathBuf {
    let mut ancestor = path;
    let mut missing = Vec::<OsString>::new();
    loop {
        if let Ok(canonical) = ancestor.canonicalize() {
            return missing
                .into_iter()
                .rev()
                .fold(canonical, |path, component| path.join(component));
        }
        let Some(name) = ancestor.file_name() else {
            return path.to_path_buf();
        };
        missing.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return path.to_path_buf();
        };
        ancestor = parent;
    }
}

fn split_shell_segments(command: &str) -> Result<Vec<&str>, String> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;

    for (index, ch) in command.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if matches!(ch, ';' | '\n' | '|' | '&' | '(' | ')') {
            let segment = command[start..index].trim();
            if !segment.is_empty() {
                segments.push(segment);
            }
            start = index + ch.len_utf8();
        }
    }
    if quote.is_some() || escaped {
        return Err("invalid shell quoting".to_string());
    }
    let segment = command[start..].trim();
    if !segment.is_empty() {
        segments.push(segment);
    }
    Ok(segments)
}

fn shell_words(segment: &str) -> Result<Vec<String>, String> {
    shlex::split(segment).ok_or_else(|| "invalid shell quoting".to_string())
}

fn command_name(words: &[String]) -> Option<(usize, &str)> {
    let mut index = words
        .iter()
        .position(|word| !word.contains('=') || word.starts_with('/'))?;
    while let wrapper @ ("command" | "builtin" | "nohup" | "sudo" | "doas" | "env" | "nice"
    | "time" | "timeout" | "setsid" | "ionice" | "chrt" | "stdbuf") =
        command_basename(words.get(index)?)
    {
        index += 1;
        while let Some(word) = words.get(index) {
            if word.contains('=') {
                index += 1;
                continue;
            }
            if !word.starts_with('-') {
                break;
            }
            let option_takes_value = matches!(
                (wrapper, word.as_str()),
                (
                    "sudo",
                    "-u" | "--user"
                        | "-g"
                        | "--group"
                        | "-h"
                        | "--host"
                        | "-p"
                        | "--prompt"
                        | "-C"
                        | "--close-from"
                        | "-R"
                        | "--chroot"
                        | "-T"
                        | "--command-timeout"
                ) | ("doas", "-u")
                    | (
                        "env",
                        "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string"
                    )
                    | ("nice", "-n" | "--adjustment")
                    | ("timeout", "-k" | "--kill-after" | "-s" | "--signal")
                    | (
                        "ionice",
                        "-c" | "--class" | "-n" | "--classdata" | "-t" | "--ignore"
                    )
                    | ("chrt", "-p" | "--pid" | "-r" | "--max")
                    | (
                        "stdbuf",
                        "-i" | "--input" | "-o" | "--output" | "-e" | "--error"
                    )
            );
            index += 1;
            if option_takes_value {
                index += 1;
            }
        }
    }
    Some((index, command_basename(words.get(index)?)))
}

fn command_basename(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

fn is_destructive_name(name: &str) -> bool {
    DESTRUCTIVE_COMMANDS.contains(&name) || name.starts_with("mkfs") || name == "diskutil"
}

fn mentions_destructive_command(command: &str) -> bool {
    command
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')))
        .any(|word| is_destructive_name(word))
}

fn segment_is_destructive(segment: &str) -> bool {
    let Ok(words) = shell_words(segment) else {
        return false;
    };
    command_name(&words).is_some_and(|(_, name)| {
        is_destructive_name(name)
            || name == "find"
                && words.iter().any(|arg| {
                    matches!(
                        arg.as_str(),
                        "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
                    )
                })
            || name == "xargs"
                && words
                    .iter()
                    .any(|arg| is_destructive_name(command_basename(arg)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/workspace"),
            PathBuf::from("/home/user/.fae"),
            PathBuf::from("/tmp"),
        ]
        .into_iter()
        .map(|path| normalize_path(&path, Path::new("/")))
        .collect()
    }

    #[test]
    fn allows_destructive_targets_under_safe_roots() {
        validate_shell(
            "rm -rf target /tmp/cache; chmod 600 /home/user/.fae/config",
            Path::new("/workspace"),
            &roots(),
        )
        .unwrap();
    }

    #[test]
    fn rejects_absolute_and_traversing_targets() {
        let safe_roots = roots();
        assert!(validate_shell("rm -rf /etc", Path::new("/workspace"), &safe_roots).is_err());
        assert!(
            validate_shell(
                "mv file ../../outside",
                Path::new("/workspace"),
                &safe_roots
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_dynamic_destructive_targets() {
        let safe_roots = roots();
        assert!(
            validate_shell("rm -rf \"$TARGET\"", Path::new("/workspace"), &safe_roots).is_err()
        );
        assert!(validate_shell("xargs rm -rf", Path::new("/workspace"), &safe_roots).is_err());
        assert!(validate_shell("echo $(rm -rf /)", Path::new("/workspace"), &safe_roots).is_err());
        assert!(
            validate_shell(
                "sudo -u root rm -rf /etc",
                Path::new("/workspace"),
                &safe_roots
            )
            .is_err()
        );
        assert!(
            validate_shell(
                "bash -lc 'rm -rf /etc'",
                Path::new("/workspace"),
                &safe_roots
            )
            .is_err()
        );
        assert!(
            validate_shell(
                "xargs sh -c 'rm -rf /etc'",
                Path::new("/workspace"),
                &safe_roots
            )
            .is_err()
        );
        assert!(
            validate_shell("eval 'rm -rf /etc'", Path::new("/workspace"), &safe_roots).is_err()
        );
    }

    #[test]
    fn validates_nested_shell_and_find_delete() {
        let safe_roots = roots();
        assert!(
            validate_shell("sh -c 'rm -rf /etc'", Path::new("/workspace"), &safe_roots).is_err()
        );
        validate_shell("find ./build -delete", Path::new("/workspace"), &safe_roots).unwrap();
        assert!(validate_shell("find /etc -delete", Path::new("/workspace"), &safe_roots).is_err());
        assert!(
            validate_shell(
                "find . -exec rm -rf {} +",
                Path::new("/workspace"),
                &safe_roots
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_cd_before_destructive_command() {
        assert!(validate_shell("cd / && rm -rf tmp", Path::new("/workspace"), &roots()).is_err());
    }

    #[test]
    fn validates_mv_target_directory_option() {
        let safe_roots = roots();
        validate_shell(
            "mv -t /workspace/archive file",
            Path::new("/workspace"),
            &safe_roots,
        )
        .unwrap();
        assert!(
            validate_shell(
                "mv --target-directory=/etc file",
                Path::new("/workspace"),
                &safe_roots
            )
            .is_err()
        );
    }
}
