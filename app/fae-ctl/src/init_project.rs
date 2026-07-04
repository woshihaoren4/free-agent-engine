use fae_agent::{AgentConfigData, SkillConfig, ToolConfig};
use std::env;
use std::fs;
use std::path::PathBuf;

const PROMPT_ASSISTANT: &str = include_str!("../../../docs/prompt/assistant.txt");
const PROMPT_AICODING: &str = include_str!("../../../docs/prompt/aicoding.txt");
const PROMPT_CLAW: &str = include_str!("../../../docs/prompt/claw.txt");
const PROMPT_AITEST: &str = include_str!("../../../docs/prompt/aitest.txt");
const MCP_LIST_JSON: &str = r#"{
	"mcpServers": {
		"mcp_name1": {
			"url": "https://mcp.xxxx.com/mcp",
			"headers": {}
		},
		"mcp_name2": {
			"command": "npx",
			"args": ""
		}
	}
}"#;

struct AgentTemplate {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    prompt_file: &'static str,
    tools: &'static [&'static str],
    skills: &'static [&'static str],
    sub_agents: &'static [&'static str],
}

const DEFAULT_AGENTS: &[AgentTemplate] = &[
    AgentTemplate {
        id: "fae-assistant",
        name: "风筝任务协调助手",
        description: "风筝任务协调助手负责理解、拆解、分配和监督多个 Agent 协作完成任务。",
        prompt_file: "assistant.txt",
        tools: &[
            "read_file",
            "write_file",
            "list_directory",
            "send_http_request",
            "ark_web_search",
            "todo_write",
            "scheduled_execution",
            "agent_exec_task",
            "apply_patch",
            "execute_command",
        ],
        skills: &["weather", "fae"],
        sub_agents: &["fae-aicoding", "fae-claw", "fae-aitest"],
    },
    AgentTemplate {
        id: "fae-aicoding",
        name: "风筝编程助手",
        description: "风筝编程助手用于项目开发、脚本编写、错误修复、代码优化和工程实现。",
        prompt_file: "aicoding.txt",
        tools: &[
            "execute_command",
            "read_file",
            "write_file",
            "list_directory",
            "apply_patch",
            "execute_python",
            "todo_write",
            "agent_exec_task",
        ],
        skills: &["drawio-skill", "fae"],
        sub_agents: &[],
    },
    AgentTemplate {
        id: "fae-claw",
        name: "风筝电脑管家",
        description: "风筝电脑管家用于计算机自动化、文件处理、系统操作、办公任务和通用问答。",
        prompt_file: "claw.txt",
        tools: &[
            "execute_command",
            "read_file",
            "write_file",
            "list_directory",
            "send_http_request",
            "execute_python",
            "todo_write",
            "ark_web_search",
            "scheduled_execution",
            "agent_exec_task",
        ],
        skills: &["weather", "drawio-skill", "fae"],
        sub_agents: &[],
    },
    AgentTemplate {
        id: "fae-aitest",
        name: "风筝测试助手",
        description: "风筝测试助手负责审查实现、设计测试、执行验证并判断代码是否满足需求。",
        prompt_file: "aitest.txt",
        tools: &[
            "execute_command",
            "read_file",
            "list_directory",
            "send_http_request",
            "execute_python",
            "todo_write",
            "agent_exec_task",
        ],
        skills: &[],
        sub_agents: &[],
    },
];

fn tool_configs(tools: &[&str]) -> Vec<ToolConfig> {
    tools.iter().copied().map(ToolConfig::new).collect()
}

fn skill_configs(skills: &[&str]) -> Vec<SkillConfig> {
    skills.iter().copied().map(SkillConfig::new).collect()
}

pub struct InitProject {}

impl InitProject {
    pub fn get_workspace_dir(ws: &str) -> PathBuf {
        let fae_dir = fae_agent::fae_home();
        fae_dir.join(ws)
    }
    pub async fn init(ws: String) {
        wd_log::log_info_ln!("start init project...");

        let fae_dir = fae_agent::fae_home();

        // 设置环境变量FAE_WORKSPACE为～/.fae
        unsafe {
            wd_log::log_info_ln!("set env FAE_WORKSPACE to {}", fae_dir.display());
            env::set_var("FAE_WORKSPACE", fae_dir.to_str().unwrap());
        }
        // 下载site.txt
        wd_log::log_info_ln!("downloading from site.txt...");
        let site_txt_url = "https://woshihaoren4.github.io/free-agent-engine/site.txt";
        match reqwest::get(site_txt_url).await {
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(site_content) = resp.text().await {
                        for line in site_content.lines() {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            let file_path = fae_dir.join(line);
                            if !file_path.exists() {
                                let url = format!(
                                    "https://woshihaoren4.github.io/free-agent-engine/{}",
                                    line
                                );
                                wd_log::log_info_ln!("downloading {} ...", url);
                                match reqwest::get(&url).await {
                                    Ok(resp) => {
                                        if resp.status().is_success() {
                                            if let Ok(content) = resp.bytes().await {
                                                if let Some(parent) = file_path.parent() {
                                                    if !parent.exists() {
                                                        if let Err(e) = fs::create_dir_all(parent) {
                                                            wd_log::log_error_ln!(
                                                                "failed to create dir {}: {}",
                                                                parent.display(),
                                                                e
                                                            );
                                                        }
                                                    }
                                                }
                                                if let Err(e) = fs::write(&file_path, content) {
                                                    wd_log::log_error_ln!(
                                                        "failed to write file {}: {}",
                                                        file_path.display(),
                                                        e
                                                    );
                                                } else {
                                                    wd_log::log_info_ln!(
                                                        "downloaded and saved {}",
                                                        file_path.display()
                                                    );
                                                    if file_path
                                                        .extension()
                                                        .and_then(|s| s.to_str())
                                                        == Some("zip")
                                                    {
                                                        wd_log::log_info_ln!(
                                                            "extracting zip file {}...",
                                                            file_path.display()
                                                        );
                                                        match fs::File::open(&file_path) {
                                                            Ok(file) => {
                                                                match zip::ZipArchive::new(file) {
                                                                    Ok(mut archive) => {
                                                                        let target_dir = file_path
                                                                            .parent()
                                                                            .unwrap();
                                                                        if let Err(e) = archive
                                                                            .extract(target_dir)
                                                                        {
                                                                            wd_log::log_error_ln!(
                                                                                "failed to extract zip file {}: {}",
                                                                                file_path.display(),
                                                                                e
                                                                            );
                                                                        } else {
                                                                            wd_log::log_info_ln!(
                                                                                "extracted zip file {}",
                                                                                file_path.display()
                                                                            );
                                                                            if let Err(e) =
                                                                                fs::remove_file(
                                                                                    &file_path,
                                                                                )
                                                                            {
                                                                                wd_log::log_error_ln!(
                                                                                    "failed to remove zip file {}: {}",
                                                                                    file_path
                                                                                        .display(),
                                                                                    e
                                                                                );
                                                                            } else {
                                                                                wd_log::log_info_ln!(
                                                                                    "removed zip file {}",
                                                                                    file_path
                                                                                        .display()
                                                                                );
                                                                            }
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        wd_log::log_error_ln!(
                                                                            "failed to read zip archive {}: {}",
                                                                            file_path.display(),
                                                                            e
                                                                        )
                                                                    }
                                                                }
                                                            }
                                                            Err(e) => wd_log::log_error_ln!(
                                                                "failed to open zip file {}: {}",
                                                                file_path.display(),
                                                                e
                                                            ),
                                                        }
                                                    }
                                                }
                                            }
                                        } else {
                                            wd_log::log_error_ln!(
                                                "failed to download {}: status {}",
                                                url,
                                                resp.status()
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        wd_log::log_error_ln!("failed to download {}: {}", url, e);
                                    }
                                }
                            } else {
                                wd_log::log_info_ln!(
                                    "file {} already exists, skip",
                                    file_path.display()
                                );
                            }
                        }
                    }
                } else {
                    wd_log::log_error_ln!("failed to fetch site.txt: status {}", resp.status());
                }
            }
            Err(e) => {
                wd_log::log_error_ln!("failed to fetch site.txt: {}", e);
            }
        }
        // 创建～/.fae/prompt目录
        let prompt_dir = fae_dir.join("prompt");
        if !prompt_dir.exists() {
            wd_log::log_info_ln!("create directory {}", prompt_dir.display());
            fs::create_dir_all(&prompt_dir).expect("Failed to create prompt directory");
        } else {
            wd_log::log_info_ln!("directory {} already exists", prompt_dir.display());
        }

        // 检查并创建 ～/.fae/mcp/mcp_list.json文件,写入：MCP_LIST_JSON
        let mcp_dir = fae_dir.join("mcp");
        if !mcp_dir.exists() {
            wd_log::log_info_ln!("create directory {}", mcp_dir.display());
            fs::create_dir_all(&mcp_dir).expect("Failed to create mcp directory");
        } else {
            wd_log::log_info_ln!("directory {} already exists", mcp_dir.display());
        }

        let mcp_list_path = mcp_dir.join("mcp_list.json");
        if !mcp_list_path.exists() {
            wd_log::log_info_ln!("create file {}", mcp_list_path.display());
            fs::write(&mcp_list_path, MCP_LIST_JSON).expect("Failed to write mcp_list.json");
        } else {
            wd_log::log_info_ln!("file {} already exists", mcp_list_path.display());
        }

        // 检查并在～/.fae/prompt目录下创建默认 prompt 文件
        for (prompt_file, prompt_content) in [
            ("assistant.txt", PROMPT_ASSISTANT),
            ("aicoding.txt", PROMPT_AICODING),
            ("claw.txt", PROMPT_CLAW),
            ("aitest.txt", PROMPT_AITEST),
        ] {
            let prompt_path = prompt_dir.join(prompt_file);
            if !prompt_path.exists() {
                wd_log::log_info_ln!("create file {}", prompt_path.display());
                fs::write(&prompt_path, prompt_content)
                    .unwrap_or_else(|_| panic!("Failed to write {}", prompt_file));
            } else {
                wd_log::log_info_ln!("file {} already exists", prompt_path.display());
            }
        }

        // 创建～/.fae/{ws}目录
        let ws_dir = fae_dir.join(&ws);
        if !ws_dir.exists() {
            wd_log::log_info_ln!("create directory {}", ws_dir.display());
            fs::create_dir_all(&ws_dir).expect("Failed to create workspace directory");
        } else {
            wd_log::log_info_ln!("directory {} already exists", ws_dir.display());
        }

        // 检查并在～/.fae/{ws}目录下创建默认 agent
        for agent in DEFAULT_AGENTS {
            let agent_dir = ws_dir.join(agent.id);
            if !agent_dir.exists() {
                wd_log::log_info_ln!("create agent {} in {}", agent.id, ws_dir.display());
                fs::create_dir_all(&agent_dir)
                    .unwrap_or_else(|_| panic!("Failed to create {} agent directory", agent.id));
                let mut config = AgentConfigData::default()
                    .set_name(agent.name)
                    .set_description(agent.description)
                    .set_prompt_path(format!(
                        "{}/prompt/{}",
                        fae_dir.display(),
                        agent.prompt_file
                    ));
                config.tools = tool_configs(agent.tools);
                config.skills = skill_configs(agent.skills);
                config.sub_agents = agent
                    .sub_agents
                    .iter()
                    .map(|agent_id| agent_id.to_string())
                    .collect();
                config
                    .init(agent.id, &ws_dir)
                    .await
                    .unwrap_or_else(|_| panic!("Failed to init {} agent config", agent.id));
            } else {
                wd_log::log_info_ln!("agent {} already exists in {}", agent.id, ws_dir.display());
            }
        }

        wd_log::log_info_ln!("init project success.");
    }
}
