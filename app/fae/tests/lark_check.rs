//! Exercise the `lark check` CLI with complete on-disk dependency graphs.
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("fae-lark-check-{}", wd_tools::uuid::v4()));
        std::fs::create_dir_all(home.join("agents")).unwrap();
        std::fs::create_dir_all(home.join("workflows")).unwrap();
        let fixture = Self(home.canonicalize().unwrap());
        fixture.write(
            "lark.config",
            json!({
                "version":1,"app_id":"test-app","app_secret":"test-secret",
                "tenant_key":"tenant","agent_id":"parent","workspace":fixture.0
            }),
        );
        for id in ["parent", "worker"] {
            std::fs::write(
                fixture.0.join(format!("agents/{id}_prompt.txt")),
                "Test Agent",
            )
            .unwrap();
        }
        fixture
    }
    fn write(&self, file: &str, value: Value) {
        std::fs::write(self.0.join(file), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    fn agent(&self, id: &str, tools: Value, workflows: Value, sub_agents: Value) {
        self.write(
            &format!("agents/{id}_config.json"),
            json!({
                "agent":{"name":id,"user_id":"master","session_id":"fixture"},
                "model":{"model":"fixture"},"tools":tools,
                "workflows":workflows,"sub_agents":sub_agents
            }),
        );
    }
    fn workflow(&self, id: &str, actions: Vec<Value>) {
        let mut nodes = serde_json::Map::new();
        nodes.insert("start".into(), json!({"type":"start","next":"n0"}));
        let count = actions.len();
        for (index, action) in actions.into_iter().enumerate() {
            let next = if index + 1 == count {
                "end".into()
            } else {
                format!("n{}", index + 1)
            };
            nodes.insert(
                format!("n{index}"),
                json!({"type":"execute","action":action,"next":next}),
            );
        }
        nodes.insert("end".into(), json!({"type":"end"}));
        self.write(
            &format!("workflows/{id}.json"),
            json!({"id":id,"desc":"CLI fixture","nodes":nodes}),
        );
    }
    fn check(&self, expected_error: Option<&str>) {
        let output = Command::new(env!("CARGO_BIN_EXE_fae"))
            .arg("--fae-home")
            .arg(&self.0)
            .args(["lark", "check"])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        match expected_error {
            Some(error) => {
                assert!(!output.status.success(), "{text}");
                assert!(text.contains(error), "{text}");
            }
            None => assert!(output.status.success(), "{text}"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_checks_transitive_agent_and_workflow_sources_without_looping() {
    let f = Fixture::new();
    f.agent("parent", json!([]), json!(["root"]), json!([]));
    f.agent("worker", json!(["todo"]), json!([]), json!([]));
    f.workflow(
        "root",
        vec![json!({"type":"workflow","workflow_id":"child"})],
    );
    f.workflow(
        "child",
        vec![json!({"type":"single_agent","source":{"agent_id":"worker"},"input":"q"})],
    );
    f.check(None);

    f.workflow(
        "child",
        vec![json!({
            "type":"single_agent","source":{"paths":{
                "config":f.0.join("agents/worker_config.json"),
                "prompt":f.0.join("agents/worker_prompt.txt")
            }},"input":"q"
        })],
    );
    f.check(None);
    f.agent("worker", json!(["default__todo"]), json!([]), json!([]));
    f.check(None);

    // Both Agent and workflow references cycle. Checking must terminate, and
    // a harmless literal "todo" in a tool argument must not trigger rejection.
    f.agent(
        "worker",
        json!(["read_file"]),
        json!(["child"]),
        json!(["parent"]),
    );
    f.workflow(
        "child",
        vec![
            json!({"type":"single_agent","source":{"agent_id":"worker"},"input":"todo"}),
            json!({"type":"workflow","workflow_id":"root"}),
            json!({"type":"tool","tool_name":"read_file","arguments":{"path":"todo"}}),
        ],
    );
    f.check(None);
}

#[test]
fn cli_accepts_todo_and_dynamic_execution_routes() {
    let f = Fixture::new();
    f.agent(
        "parent",
        json!([
            "todo",
            "default__todo",
            "custom__read_file",
            "default__agent"
        ]),
        json!(["root"]),
        json!(["worker"]),
    );
    f.agent("worker", json!(["todo"]), json!([]), json!([]));
    for action in [
        json!({"type":"custom","task_type":"plan"}),
        json!({"type":"session","request":{"Query":{"agent_id":"worker","user_id":"alice","session_id":"{$input.session}"}}}),
        json!({"type":"tool","tool_name":"default__agent","arguments":{"agent_id":"{$input.agent}","input":"q"}}),
        json!({"type":"tool","tool_name":"workflow","arguments":{"workflow_id":"{$input.workflow}"}}),
        json!({"type":"tool","tool_name":"custom__read_file"}),
        json!({"type":"tool","tool_name":"todo"}),
        json!({"type":"tool","tool_name":"default__todo"}),
    ] {
        f.workflow("root", vec![action]);
        f.check(None);
    }
}

#[test]
fn cli_still_checks_static_dependencies() {
    let f = Fixture::new();
    f.agent("parent", json!(["todo"]), json!([]), json!(["worker"]));
    f.check(Some("load single-agent config"));

    f.agent("worker", json!(["todo"]), json!(["missing"]), json!([]));
    f.check(Some("No such file or directory"));

    f.workflow(
        "root",
        vec![json!({"type":"workflow","workflow_id":"../outside"})],
    );
    f.agent("worker", json!(["todo"]), json!(["root"]), json!([]));
    f.check(Some("must be a static workflow ID"));
}
