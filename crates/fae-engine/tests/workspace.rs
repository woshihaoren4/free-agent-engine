use std::path::PathBuf;

use fae_agent::{ContextChildSpec, ContextScope, Ctx, ToolRequest, ToolRespItem, Tools};
#[cfg(not(windows))]
use fae_engine::APPLY_PATCH;
use fae_engine::{
    DefaultTools, EXECUTE_COMMAND, EXECUTE_PYTHON, Engine, EngineBuilder, LIST_DIRECTORY,
    READ_FILE, WRITE_FILE,
};
use serde_json::{Value, json};

async fn invoke(tools: &DefaultTools, ctx: &Ctx, name: &str, arguments: Value) -> Value {
    let task_ctx = ctx.fork(ContextChildSpec::default()).unwrap();
    let mut response = tools
        .exec(
            &task_ctx,
            ToolRequest::new(name.into(), arguments.to_string()),
        )
        .await
        .unwrap();
    let ToolRespItem::Completed(output) = response.next().await.unwrap() else {
        panic!("expected completed tool response");
    };
    serde_json::from_str(&output).unwrap()
}

async fn exercise_workspace(engine: &Engine, root: PathBuf, text: &str) {
    tokio::fs::create_dir_all(root.join("nested"))
        .await
        .unwrap();
    let mut scope = ContextScope::root(text);
    scope.workspace = Some(root.clone());
    let ctx = engine.ctx_with_scope(scope).unwrap();
    let tools = DefaultTools::default();
    invoke(
        &tools,
        &ctx,
        WRITE_FILE,
        json!({"path": "file.txt", "content": text}),
    )
    .await;
    let result = invoke(&tools, &ctx, READ_FILE, json!({"path": "file.txt"})).await;
    assert_eq!(result["content"], text);
    let result = invoke(&tools, &ctx, LIST_DIRECTORY, json!({"path": "."})).await;
    assert!(
        result["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "file.txt")
    );
    let result = invoke(
        &tools,
        &ctx,
        EXECUTE_COMMAND,
        json!({"command": if cfg!(windows) { "cd" } else { "pwd" }}),
    )
    .await;
    assert_eq!(result["success"], true);
    assert_eq!(result["cwd"], root.display().to_string());
    assert_eq!(
        std::fs::canonicalize(result["stdout"].as_str().unwrap().trim()).unwrap(),
        std::fs::canonicalize(&root).unwrap()
    );
    let result = invoke(
        &tools,
        &ctx,
        EXECUTE_PYTHON,
        json!({"script": "from pathlib import Path; print(Path('file.txt').read_text())"}),
    )
    .await;
    assert_eq!(result["success"], true);
    assert_eq!(result["stdout"].as_str().unwrap().trim(), text);
    let result = invoke(
        &tools,
        &ctx,
        EXECUTE_COMMAND,
        json!({"command": if cfg!(windows) { "cd" } else { "pwd" }, "cwd": "nested"}),
    )
    .await;
    assert_eq!(result["cwd"], root.join("nested").display().to_string());
    assert_eq!(result["success"], true);
    #[cfg(not(windows))]
    {
        invoke(
            &tools,
            &ctx,
            WRITE_FILE,
            json!({"path": "nested/file.txt", "content": "old\n"}),
        )
        .await;
        let result = invoke(
            &tools,
            &ctx,
            APPLY_PATCH,
            json!({
                "patch": "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new\n",
                "cwd": "nested"
            }),
        )
        .await;
        assert_eq!(result["cwd"], root.join("nested").display().to_string());
        assert_eq!(result["success"], true);
        assert_eq!(
            tokio::fs::read_to_string(root.join("nested/file.txt"))
                .await
                .unwrap(),
            "new\n"
        );
    }
}

#[tokio::test]
async fn concurrent_sessions_resolve_files_and_commands_in_their_own_workspace() {
    let process_cwd = std::env::current_dir().unwrap();
    let root =
        std::env::temp_dir().join(format!("fae-session-workspaces-{}", wd_tools::uuid::v4()));
    let engine = EngineBuilder::new().build().await;
    tokio::join!(
        exercise_workspace(&engine, root.join("alice"), "alice"),
        exercise_workspace(&engine, root.join("bob"), "bob")
    );
    assert_eq!(std::env::current_dir().unwrap(), process_cwd);
    engine.exit().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
