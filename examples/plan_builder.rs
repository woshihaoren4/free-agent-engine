use std::path::PathBuf;

use fae_agent::{
    Ctx, Plan, PlanBuilderWithEnv, PlanNext, RT, TaskMeta, TaskReq, TaskRequest, TaskResp,
    TaskResponse, TaskType, ToolRequest, ToolRespItem, ToolResponse,
};
use fae_engine::{
    DefaultTools, EXECUTE_COMMAND, EXECUTE_PYTHON, EngineBuilder, PlanRuntime, READ_FILE,
    ToolsRuntime,
};
use serde_json::{Value, json};

const INPUT_FILE_ENV: &str = "FAE_EXAMPLE_INPUT_FILE";

#[derive(Debug)]
struct FileParseEnv {
    file_env_name: String,
}

#[derive(Debug)]
struct FileParsePlanBuilder;

#[async_trait::async_trait]
impl PlanBuilderWithEnv<FileParseEnv> for FileParsePlanBuilder {
    async fn build(&self, _rt: RT, ctx: Ctx, env: FileParseEnv) -> anyhow::Result<Box<dyn Plan>> {
        Ok(Box::new(FileParsePlan {
            ctx,
            file_env_name: env.file_env_name,
            stage: Stage::ReadEnv,
            file_content: None,
        }))
    }
}

#[derive(Debug)]
enum Stage {
    ReadEnv,
    ReadFile,
    ParseWithPython,
}

#[derive(Debug)]
struct FileParsePlan {
    ctx: Ctx,
    file_env_name: String,
    stage: Stage,
    file_content: Option<String>,
}

#[async_trait::async_trait]
impl Plan for FileParsePlan {
    fn id(&self) -> &str {
        "file_parse_plan"
    }

    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        Ok(PlanNext::Tasks(vec![self.tool_task(
            "read_env",
            format!("default__{}", EXECUTE_COMMAND).as_str(),
            json!({
                "command": read_env_command(&self.file_env_name),
                "timeout_secs": 5
            }),
        )]))
    }

    async fn next(&mut self, mut task_result: TaskResponse) -> anyhow::Result<PlanNext> {
        let tool_response = TaskResp::<ToolResponse>::try_from_response(&mut task_result)
            .ok_or_else(|| anyhow::anyhow!("expected a ToolResponse"))?;
        let output = completed_json(tool_response.resp).await?;

        match self.stage {
            Stage::ReadEnv => {
                let path = output["stdout"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                anyhow::ensure!(!path.is_empty(), "{} is empty", self.file_env_name);

                self.stage = Stage::ReadFile;
                Ok(PlanNext::Tasks(vec![self.tool_task(
                    "read_file",
                    READ_FILE,
                    json!({
                        "path": path,
                        "max_bytes": 4096
                    }),
                )]))
            }
            Stage::ReadFile => {
                let content = output["content"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("read_file did not return content"))?
                    .to_string();

                self.file_content = Some(content.clone());
                self.stage = Stage::ParseWithPython;
                Ok(PlanNext::Tasks(vec![self.tool_task(
                    "parse_with_python",
                    EXECUTE_PYTHON,
                    json!({
                        "script": python_parser_script(&content),
                        "timeout_secs": 5
                    }),
                )]))
            }
            Stage::ParseWithPython => {
                anyhow::ensure!(
                    output["success"].as_bool().unwrap_or(false),
                    "python failed: {}",
                    output["stderr"].as_str().unwrap_or_default()
                );

                let parsed = output["stdout"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("execute_python did not return stdout"))?
                    .trim()
                    .to_string();
                self.ctx.over(Box::new(parsed));

                Ok(PlanNext::End)
            }
        }
    }

    async fn abort(&mut self, code: i32, error: String) {
        eprintln!("plan aborted: code={code}, error={error}");
    }
}

impl FileParsePlan {
    fn tool_task(&self, id: impl Into<String>, tool_name: &str, arguments: Value) -> TaskRequest {
        TaskReq {
            ctx: self.ctx.clone(),
            meta: TaskMeta {
                id: id.into(),
                ty: TaskType::Tool,
                ..Default::default()
            },
            req: ToolRequest::new(tool_name.to_string(), arguments.to_string()),
        }
        .into_request()
    }
}

async fn completed_json(mut response: ToolResponse) -> anyhow::Result<Value> {
    match response.next().await? {
        ToolRespItem::Completed(output) => Ok(serde_json::from_str(&output)?),
        ToolRespItem::Streaming(output) => {
            anyhow::bail!("expected completed tool response, got streaming item: {output}")
        }
    }
}

fn read_env_command(name: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("echo %{name}%")
    }

    #[cfg(not(target_os = "windows"))]
    {
        format!("printf '%s' \"${name}\"")
    }
}

fn python_parser_script(content: &str) -> String {
    let content_json = serde_json::to_string(content).expect("string serialization cannot fail");
    format!(
        r#"
import json

content = {content_json}
result = {{
    "line_count": len(content.splitlines()),
    "word_count": len(content.split()),
    "char_count": len(content),
}}
print(json.dumps(result, ensure_ascii=False))
"#
    )
}

async fn build_engine() -> fae_engine::Engine {
    let mut builder = EngineBuilder::new();

    builder.add_runtime(PlanRuntime::new());

    let mut tools_runtime = ToolsRuntime::new();
    tools_runtime.add_tool(Box::new(DefaultTools::default()));
    builder.add_runtime(tools_runtime);

    builder.add_plan_builder(FileParsePlanBuilder);

    builder.build().await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let input_path = write_sample_input().await?;

    // SAFETY: The example sets the variable before any spawned task reads process environment.
    unsafe {
        std::env::set_var(INPUT_FILE_ENV, &input_path);
    }

    let engine = build_engine().await;
    let (ctx, parsed) = engine
        .invoke::<_, String>(FileParseEnv {
            file_env_name: INPUT_FILE_ENV.to_string(),
        })
        .await?;

    println!("{parsed}");
    println!("{:#?}", ctx.stacks());
    engine.exit().await?;

    Ok(())
}

async fn write_sample_input() -> anyhow::Result<PathBuf> {
    let path = std::env::temp_dir().join("fae_plan_builder_with_env_input.txt");
    tokio::fs::write(
        &path,
        "alpha beta gamma\nfree agent engine\npython parses this file\n",
    )
    .await?;
    Ok(path)
}
