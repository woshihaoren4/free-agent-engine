use crate::executors::{IdenInfo, Tool};
use async_trait::async_trait;
use fae_agent::{GLOBAL_KEY_AGENT_ID, GLOBAL_KEY_PROJECT_DIR, GLOBAL_KEY_WORKSPACE, ToolResponse};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Default, Debug)]
pub struct ApplyPatch;

#[derive(Debug)]
struct FilePatch {
    old_path: String,
    new_path: String,
    hunks: Vec<Hunk>,
}

#[derive(Debug)]
struct Hunk {
    old_start: usize,
    lines: Vec<HunkLine>,
}

#[derive(Debug)]
enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[async_trait]
impl Tool for ApplyPatch {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a standard unified diff patch to the target files. Can only modify files in the allowed directories."
    }

    fn arguments(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "A standard unified diff patch containing ---/+++ file headers and @@ hunks."
                }
            },
            "required": ["patch"]
        })
    }

    async fn call(&self, iden: IdenInfo, args: String) -> anyhow::Result<ToolResponse> {
        let args_val: serde_json::Value = serde_json::from_str(&args)?;
        let patch = args_val["patch"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("patch is required"))?;

        let file_patches = parse_patch(patch)?;
        let allowed_dirs = canonicalize_allowed_dirs(allowed_dirs(&iden));
        let mut results = Vec::new();

        for file_patch in file_patches {
            let target_path = file_patch.target_path()?;
            let final_path = resolve_allowed_path(&target_path, &allowed_dirs)?;
            let is_new_file = file_patch.old_path == "/dev/null";
            let is_deleted_file = file_patch.new_path == "/dev/null";
            let original = if is_new_file {
                String::new()
            } else {
                fs::read_to_string(&final_path).await?
            };
            let (original_lines, has_trailing_newline) = split_file_lines(&original);
            let patched_lines = apply_hunks(&original_lines, &file_patch.hunks)?;

            if is_deleted_file {
                fs::remove_file(&final_path).await?;
                results.push(format!("deleted {}", final_path.display()));
            } else {
                let content = join_file_lines(&patched_lines, has_trailing_newline || is_new_file);
                fs::write(&final_path, content).await?;
                let action = if is_new_file { "created" } else { "patched" };
                results.push(format!("{} {}", action, final_path.display()));
            }
        }

        Ok(ToolResponse::with_result(format!(
            "Successfully applied patch to {} file(s):\n{}",
            results.len(),
            results.join("\n")
        )))
    }
}

impl FilePatch {
    fn target_path(&self) -> anyhow::Result<String> {
        let path = if self.new_path == "/dev/null" {
            &self.old_path
        } else {
            &self.new_path
        };

        if path == "/dev/null" {
            return Err(anyhow::anyhow!("patch does not contain a target file path"));
        }

        Ok(strip_diff_path_prefix(path))
    }
}

fn parse_patch(patch: &str) -> anyhow::Result<Vec<FilePatch>> {
    let lines: Vec<&str> = patch
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let mut file_patches = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        if !lines[index].starts_with("--- ") {
            index += 1;
            continue;
        }

        let old_path = parse_patch_path(&lines[index][4..]);
        index += 1;
        if index >= lines.len() || !lines[index].starts_with("+++ ") {
            return Err(anyhow::anyhow!("missing +++ file header after --- header"));
        }
        let new_path = parse_patch_path(&lines[index][4..]);
        index += 1;

        let mut hunks = Vec::new();
        while index < lines.len() {
            let line = lines[index];
            if line.starts_with("--- ") {
                break;
            }

            if line.starts_with("@@") {
                let (hunk, next_index) = parse_hunk(&lines, index)?;
                hunks.push(hunk);
                index = next_index;
            } else {
                index += 1;
            }
        }

        if hunks.is_empty() {
            return Err(anyhow::anyhow!(
                "patch for {} -> {} does not contain any hunks",
                old_path,
                new_path
            ));
        }

        file_patches.push(FilePatch {
            old_path,
            new_path,
            hunks,
        });
    }

    if file_patches.is_empty() {
        return Err(anyhow::anyhow!("patch does not contain any file changes"));
    }

    Ok(file_patches)
}

fn parse_hunk(lines: &[&str], start_index: usize) -> anyhow::Result<(Hunk, usize)> {
    let header = lines[start_index];
    let (old_start, old_len, new_len) = parse_hunk_header(header)?;
    let mut hunk_lines = Vec::new();
    let mut old_line_count = 0;
    let mut new_line_count = 0;
    let mut index = start_index + 1;

    while index < lines.len() && (old_line_count < old_len || new_line_count < new_len) {
        let line = lines[index];

        if line == r"\ No newline at end of file" {
            index += 1;
            continue;
        }

        let Some(prefix) = line.chars().next() else {
            return Err(anyhow::anyhow!("invalid empty line in hunk"));
        };
        let content = line[1..].to_string();

        match prefix {
            ' ' => {
                old_line_count += 1;
                new_line_count += 1;
                hunk_lines.push(HunkLine::Context(content));
            }
            '-' => {
                old_line_count += 1;
                hunk_lines.push(HunkLine::Remove(content));
            }
            '+' => {
                new_line_count += 1;
                hunk_lines.push(HunkLine::Add(content));
            }
            _ => return Err(anyhow::anyhow!("invalid hunk line: {}", line)),
        }

        if old_line_count > old_len || new_line_count > new_len {
            return Err(anyhow::anyhow!(
                "hunk body has more lines than its header declares"
            ));
        }

        index += 1;
    }

    if old_line_count != old_len {
        return Err(anyhow::anyhow!(
            "hunk old line count mismatch: header says {}, body has {}",
            old_len,
            old_line_count
        ));
    }

    if new_line_count != new_len {
        return Err(anyhow::anyhow!(
            "hunk new line count mismatch: header says {}, body has {}",
            new_len,
            new_line_count
        ));
    }

    Ok((
        Hunk {
            old_start,
            lines: hunk_lines,
        },
        index,
    ))
}

fn parse_hunk_header(header: &str) -> anyhow::Result<(usize, usize, usize)> {
    let rest = header
        .strip_prefix("@@")
        .ok_or_else(|| anyhow::anyhow!("invalid hunk header: {}", header))?;
    let end = rest
        .find("@@")
        .ok_or_else(|| anyhow::anyhow!("invalid hunk header: {}", header))?;
    let mut parts = rest[..end].split_whitespace();
    let old_range = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing old range in hunk header: {}", header))?;
    let new_range = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing new range in hunk header: {}", header))?;
    let (old_start, old_len) = parse_range(old_range, '-')?;
    let (_new_start, new_len) = parse_range(new_range, '+')?;

    Ok((old_start, old_len, new_len))
}

fn parse_range(range: &str, prefix: char) -> anyhow::Result<(usize, usize)> {
    let range = range
        .strip_prefix(prefix)
        .ok_or_else(|| anyhow::anyhow!("invalid range: {}", range))?;
    let mut parts = range.splitn(2, ',');
    let start = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing range start: {}", range))?
        .parse::<usize>()?;
    let len = match parts.next() {
        Some(len) => len.parse::<usize>()?,
        None => 1,
    };

    Ok((start, len))
}

fn parse_patch_path(raw: &str) -> String {
    raw.trim()
        .split('\t')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_string()
}

fn strip_diff_path_prefix(path: &str) -> String {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .to_string()
}

fn apply_hunks(original: &[String], hunks: &[Hunk]) -> anyhow::Result<Vec<String>> {
    let mut output = Vec::new();
    let mut source_index = 0;

    for hunk in hunks {
        let target_index = if hunk.old_start == 0 {
            0
        } else {
            hunk.old_start - 1
        };

        if target_index < source_index {
            return Err(anyhow::anyhow!("overlapping or out-of-order hunks"));
        }
        if target_index > original.len() {
            return Err(anyhow::anyhow!(
                "hunk starts at line {}, but file only has {} lines",
                hunk.old_start,
                original.len()
            ));
        }

        output.extend_from_slice(&original[source_index..target_index]);
        let mut current_index = target_index;

        for line in &hunk.lines {
            match line {
                HunkLine::Context(expected) => {
                    let actual = original.get(current_index).ok_or_else(|| {
                        anyhow::anyhow!("context line is past end of file: {}", expected)
                    })?;
                    if actual != expected {
                        return Err(anyhow::anyhow!(
                            "context mismatch at line {}: expected {:?}, got {:?}",
                            current_index + 1,
                            expected,
                            actual
                        ));
                    }
                    output.push(actual.clone());
                    current_index += 1;
                }
                HunkLine::Remove(expected) => {
                    let actual = original.get(current_index).ok_or_else(|| {
                        anyhow::anyhow!("remove line is past end of file: {}", expected)
                    })?;
                    if actual != expected {
                        return Err(anyhow::anyhow!(
                            "remove mismatch at line {}: expected {:?}, got {:?}",
                            current_index + 1,
                            expected,
                            actual
                        ));
                    }
                    current_index += 1;
                }
                HunkLine::Add(content) => output.push(content.clone()),
            }
        }

        source_index = current_index;
    }

    output.extend_from_slice(&original[source_index..]);
    Ok(output)
}

fn split_file_lines(content: &str) -> (Vec<String>, bool) {
    let has_trailing_newline = content.ends_with('\n');
    let content = content.strip_suffix('\n').unwrap_or(content);
    if content.is_empty() {
        return (Vec::new(), has_trailing_newline);
    }

    let lines = content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();
    (lines, has_trailing_newline)
}

fn join_file_lines(lines: &[String], trailing_newline: bool) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let mut content = lines.join("\n");
    if trailing_newline {
        content.push('\n');
    }
    content
}

fn allowed_dirs(iden: &IdenInfo) -> Vec<PathBuf> {
    let fae_home_dir = fae_agent::fae_home();
    let mut allowed_dirs = vec![
        fae_home_dir.join("skills"),
        fae_home_dir.join("prompt"),
        fae_home_dir.join("mcp"),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    ];

    if let Some(ws) = iden.get(GLOBAL_KEY_WORKSPACE) {
        if let Some(aid) = iden.get(GLOBAL_KEY_AGENT_ID) {
            allowed_dirs.push(fae_home_dir.join(ws).join(aid));
        }
    }
    if let Some(project_dir) = iden.get(GLOBAL_KEY_PROJECT_DIR) {
        allowed_dirs.push(PathBuf::from(project_dir));
    }

    allowed_dirs
}

fn canonicalize_allowed_dirs(allowed_dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    allowed_dirs
        .into_iter()
        .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir))
        .collect()
}

fn resolve_allowed_path(path: &str, allowed_dirs: &[PathBuf]) -> anyhow::Result<PathBuf> {
    let target_path = Path::new(path);
    let final_path = if target_path.is_absolute() {
        target_path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(target_path)
    };

    let parent = final_path.parent().unwrap_or_else(|| Path::new(""));
    let parent_canonical = std::fs::canonicalize(parent).map_err(|e| {
        anyhow::anyhow!(
            "Failed to canonicalize parent directory {:?}: {}. Does the parent directory exist?",
            parent,
            e
        )
    })?;

    if allowed_dirs
        .iter()
        .any(|allowed_dir| parent_canonical.starts_with(allowed_dir))
    {
        Ok(final_path)
    } else {
        Err(anyhow::anyhow!(
            "Permission denied: cannot patch outside of allowed directories {:?}",
            allowed_dirs
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_unified_diff_to_lines() {
        let patch = parse_patch(
            r#"--- a/example.txt
+++ b/example.txt
@@ -1,3 +1,4 @@
 one
-two
+TWO
 three
+four
"#,
        )
        .unwrap();
        let original = lines(&["one", "two", "three"]);
        let patched = apply_hunks(&original, &patch[0].hunks).unwrap();

        assert_eq!(patched, lines(&["one", "TWO", "three", "four"]));
    }

    #[test]
    fn applies_new_file_diff() {
        let patch = parse_patch(
            r#"--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+alpha
+beta
"#,
        )
        .unwrap();
        let patched = apply_hunks(&[], &patch[0].hunks).unwrap();

        assert_eq!(patch[0].target_path().unwrap(), "new.txt");
        assert_eq!(patched, lines(&["alpha", "beta"]));
    }

    #[test]
    fn rejects_mismatched_context() {
        let patch = parse_patch(
            r#"--- a/example.txt
+++ b/example.txt
@@ -1,2 +1,2 @@
 one
-two
+TWO
"#,
        )
        .unwrap();
        let original = lines(&["one", "not-two"]);

        assert!(apply_hunks(&original, &patch[0].hunks).is_err());
    }

    #[test]
    fn allows_removed_lines_that_look_like_file_headers() {
        let patch = parse_patch(
            r#"--- a/example.txt
+++ b/example.txt
@@ -1,2 +1,2 @@
 keep
--- looks like a header
+changed
"#,
        )
        .unwrap();
        let original = lines(&["keep", "-- looks like a header"]);
        let patched = apply_hunks(&original, &patch[0].hunks).unwrap();

        assert_eq!(patched, lines(&["keep", "changed"]));
    }

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }
}
