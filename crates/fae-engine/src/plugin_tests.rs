use crate::{Engine, EngineBuilder};
use async_openai::types::chat::CreateChatCompletionRequest;
use fae_agent::hook::{SingleAgentHook, SingleAgentHookContext};
use fae_agent::{
    plugin::{PluginManager, adapter::PluginAdapter},
    *,
};
use std::{path::Path, sync::Arc};
use tokio_stream::StreamExt;

fn write(root: &Path, id: &str, mode: &str, point: &str, body: &str) {
    std::fs::create_dir_all(root.join("plugins")).unwrap();
    let config = serde_json::json!({"plugin_version":"1.0.0","description":id,"registration_mode":mode,
        "mounts":[{"point":point,"implementation":"Demo"}]});
    std::fs::write(root.join(format!("plugins/{id}.py")),format!(
        "from fae_plugin import *\nimport asyncio\ndef plugin_config():\n    return {}\n{body}\n",
        config.to_string().replace("true","True").replace("false","False").replace("null","None"))).unwrap();
    fae_agent::plugin::config::install(root, &root.join(format!("plugins/{id}.py"))).unwrap();
}
async fn setup(root: &Path) -> (Arc<PluginManager>, Engine, PluginAdapter) {
    let manager = Arc::new(PluginManager::new(root));
    let engine = EngineBuilder::new()
        .build()
        .await
        .with_plugins(manager.clone());
    let adapter = PluginAdapter::new(manager.clone());
    (manager, engine, adapter)
}
fn root(engine: &Engine, user: &str) -> (Ctx, CommonSession) {
    let (_, session) = SingleAgentEnv::from_agent_id_with_user_id("fae", "hello", user);
    let mut scope = ContextScope::root(user);
    scope.agent_id = Some("fae".into());
    scope.turn_id = Some(1);
    scope.session = Some(ContextSession {
        session_id: format!("s-{user}"),
        session: session.clone(),
    });
    let ctx = engine.ctx_with_scope(scope).unwrap();
    ctx.set_state(ContextState::Running).unwrap();
    (ctx, session)
}
async fn prompt(adapter: &PluginAdapter, ctx: &Ctx, value: &str) -> anyhow::Result<String> {
    let agent = SingleAgentInfo {
        name: "fae".into(),
        desc: "".into(),
        user_id: ctx.scope()?.user.user_id.clone(),
        session_id: "s".into(),
        metadata: Default::default(),
    };
    adapter
        .on_prompt(
            &SingleAgentHookContext {
                ctx,
                plan_id: "p",
                turn_id: 1,
                agent: &agent,
                task_id: None,
            },
            value.into(),
        )
        .await
}

#[tokio::test]
async fn install_overwrites_files_and_directories_and_uninstall_records_lifecycle() {
    use fae_agent::plugin::config::{PluginsConfig, install, uninstall};
    let dir = tempfile::tempdir().unwrap();
    let sources = tempfile::tempdir().unwrap();
    let file = sources.path().join("demo.py");
    std::fs::write(&file, "old").unwrap();
    install(dir.path(), &file).unwrap();
    let other = sources.path().join("other.py");
    std::fs::write(&other, "other").unwrap();
    install(dir.path(), &other).unwrap();
    std::fs::write(&file, "new").unwrap();
    install(dir.path(), &file).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("plugins/demo.py")).unwrap(),
        "new"
    );
    assert_eq!(
        PluginsConfig::read(dir.path()).unwrap().plugins[0].id,
        "demo"
    );
    let directory = sources.path().join("demo");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("data.txt"), "directory").unwrap();
    install(dir.path(), &directory).unwrap();
    assert!(!dir.path().join("plugins/demo.py").exists());
    assert!(dir.path().join("plugins/demo/data.txt").exists());
    std::fs::remove_file(directory.join("data.txt")).unwrap();
    std::fs::write(directory.join("new.txt"), "replacement").unwrap();
    install(dir.path(), &directory).unwrap();
    assert!(!dir.path().join("plugins/demo/data.txt").exists());
    assert!(dir.path().join("plugins/demo/new.txt").exists());
    install(dir.path(), &file).unwrap();
    assert!(!dir.path().join("plugins/demo").exists());
    uninstall(dir.path(), "demo").unwrap();
    assert!(dir.path().join("plugins/demo.py").exists());
    let config = PluginsConfig::read(dir.path()).unwrap();
    assert!(!config.plugins[0].installed);
    assert!(!config.plugins[0].enabled);
    assert!(uninstall(dir.path(), "demo").is_err());
    install(dir.path(), &file).unwrap();
    let config = PluginsConfig::read(dir.path()).unwrap();
    assert_eq!(
        config
            .plugins
            .iter()
            .map(|plugin| plugin.id.as_str())
            .collect::<Vec<_>>(),
        ["other", "demo"]
    );
    assert!(config.plugins[1].installed && config.plugins[1].enabled);
    assert!(!dir.path().join("plugin-state").exists());
}

#[tokio::test]
async fn startup_skips_disabled_plugins_and_rejects_invalid_config_and_mounts() {
    use fae_agent::plugin::config::PluginsConfig;
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "broken",
        "direct",
        "SingleAgentHook",
        "raise RuntimeError('disabled')",
    );
    let mut config = PluginsConfig::read(dir.path()).unwrap();
    config.plugins[0].enabled = false;
    config.write(dir.path()).unwrap();
    let manager = PluginManager::new(dir.path());
    manager.load_all().await.unwrap();
    assert!(manager.snapshot().nodes.is_empty());
    write(
        dir.path(),
        "tools",
        "direct",
        "ToolsHook",
        "class Demo(ToolsHook):\n    async def desc(self,ctx,tool_name): return {}\n    async def exec(self,ctx,request): return ToolResponse.text('test')",
    );
    let source = dir.path().join("plugins/tools.py");
    let code = std::fs::read_to_string(&source).unwrap().replace(
        "\"implementation\":\"Demo\"",
        "\"implementation\":\"Demo\",\"channel\":\"default\"",
    );
    std::fs::write(&source, code).unwrap();
    let error = manager.load_all().await.unwrap_err();
    assert!(
        format!("{error:#}").contains("plugin_mount_conflict"),
        "{error:#}"
    );
    std::fs::write(dir.path().join("plugins/plugins.config"), "{bad config").unwrap();
    assert!(manager.load_all().await.is_err());
}

#[tokio::test]
async fn startup_executes_direct_hooks_in_installation_order() {
    let dir = tempfile::tempdir().unwrap();
    for (id, suffix) in [("z", "Z"), ("a", "A")] {
        write(
            dir.path(),
            id,
            "direct",
            "SingleAgentHook",
            &format!(
                "class Demo(SingleAgentHook):\n    async def on_prompt(self,ctx,value):\n        return value+'{suffix}'"
            ),
        );
    }
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load_all().await.unwrap();
    assert_eq!(
        prompt(&adapter, &root(&engine, "alice").0, "")
            .await
            .unwrap(),
        "ZA"
    );
}

#[tokio::test]
async fn startup_loads_only_configured_plugins_and_propagates_failures() {
    let dir = tempfile::tempdir().unwrap();
    let manager = PluginManager::new(dir.path());
    manager.load_all().await.unwrap();
    assert!(manager.snapshot().nodes.is_empty());
    write(
        dir.path(),
        "single",
        "direct",
        "SingleAgentHook",
        "class Demo(SingleAgentHook): pass",
    );
    let directory = dir.path().join("plugins/directory");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("plugin.py"),
        "from fae_plugin import SingleAgentHook\nclass Demo(SingleAgentHook): pass\n",
    )
    .unwrap();
    fae_agent::plugin::config::install(dir.path(), &directory).unwrap();
    std::fs::write(
        directory.join("plugin.config"),
        serde_json::json!({"plugin_version":"1.0.0","description":"directory","engine":"py",
            "mounts":[{"point":"SingleAgentHook","implementation":"plugin.py:Demo"}]})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("plugins/broken.py"),
        "raise RuntimeError('broken plugin')",
    )
    .unwrap();
    std::fs::write(dir.path().join("plugins/notes.txt"), "not a plugin").unwrap();
    std::fs::create_dir_all(dir.path().join("plugins/__pycache__")).unwrap();
    manager.load_all().await.unwrap();
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.nodes.len(), 2);
    assert!(snapshot.nodes.contains_key("single"));
    assert!(snapshot.nodes.contains_key("directory"));
    assert!(manager.status("*").unwrap().get("broken").is_none());
    fae_agent::plugin::config::install(dir.path(), &dir.path().join("plugins/broken.py")).unwrap();
    let restarted = PluginManager::new(dir.path());
    assert!(restarted.load_all().await.is_err());
    assert!(restarted.snapshot().nodes.is_empty());
    assert!(!dir.path().join("plugin-state").exists());
}

#[tokio::test]
async fn startup_reads_current_sources_and_preserves_installation_order() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "z",
        "chain",
        "SingleAgentHook",
        "class Demo(ChainSingleAgentHook):\n    async def on_prompt(self,ctx,value,*,next):\n        return await next(value+'Z')",
    );
    let manager = PluginManager::new(dir.path());
    manager.load("z").await.unwrap();
    write(
        dir.path(),
        "z",
        "chain",
        "SingleAgentHook",
        "class Demo(ChainSingleAgentHook):\n    async def on_prompt(self,ctx,value,*,next):\n        return await next(value+'new')",
    );
    for (id, suffix) in [("b", "B"), ("a", "A")] {
        write(
            dir.path(),
            id,
            "chain",
            "SingleAgentHook",
            &format!(
                "class Demo(ChainSingleAgentHook):\n    async def on_prompt(self,ctx,value,*,next):\n        return await next(value+'{suffix}')"
            ),
        );
    }
    let (restored, engine, adapter) = setup(dir.path()).await;
    restored.load_all().await.unwrap();
    assert_eq!(restored.snapshot().nodes.len(), 1);
    assert_eq!(
        restored.status("z").unwrap()["z"]["chain_order"],
        serde_json::json!(["z", "b", "a"])
    );
    assert_eq!(
        prompt(&adapter, &root(&engine, "alice").0, "")
            .await
            .unwrap(),
        "newBA"
    );
}

#[tokio::test]
async fn live_reinstall_keeps_running_workers_but_restart_rejects_invalid_source() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "demo",
        "direct",
        "SingleAgentHook",
        "class Demo(SingleAgentHook):\n    async def on_prompt(self,ctx,value):\n        return value+'old'",
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager
        .install(&dir.path().join("plugins/demo.py"))
        .await
        .unwrap();
    let (old, _) = root(&engine, "alice");
    assert_eq!(prompt(&adapter, &old, "").await.unwrap(), "old");
    write(
        dir.path(),
        "demo",
        "direct",
        "SingleAgentHook",
        "class Demo(SingleAgentHook):\n    async def on_prompt(self,ctx,value):\n        return value+'new'",
    );
    manager
        .install(&dir.path().join("plugins/demo.py"))
        .await
        .unwrap();
    let (new, _) = root(&engine, "bob");
    assert_eq!(prompt(&adapter, &old, "").await.unwrap(), "old");
    assert_eq!(prompt(&adapter, &new, "").await.unwrap(), "new");
    std::fs::write(
        dir.path().join("plugins/demo.py"),
        "def plugin_config(): raise ValueError('invalid')",
    )
    .unwrap();
    assert!(
        manager
            .install(&dir.path().join("plugins/demo.py"))
            .await
            .is_err()
    );
    assert_eq!(
        prompt(&adapter, &root(&engine, "charlie").0, "")
            .await
            .unwrap(),
        "new"
    );
    // Restart reads the edited file and propagates its configuration error.
    let restored = Arc::new(PluginManager::new(dir.path()));
    assert!(restored.load_all().await.is_err());
}
#[tokio::test]
async fn chain_order_persists_and_short_circuit_preserves_onion() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "z",
        "chain",
        "SingleAgentHook",
        "class Demo(ChainSingleAgentHook):\n    async def on_prompt(self,ctx,value,*,next):\n        return (await next(value+'Z'))+'z'",
    );
    // Keep installation order distinct from the Rust node's ID sort order.
    write(
        dir.path(),
        "a",
        "chain",
        "SingleAgentHook",
        "class Demo(ChainSingleAgentHook):\n    async def on_prompt(self,ctx,value,*,next):\n        return value+'A'",
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("z").await.unwrap();
    manager.load("a").await.unwrap();
    assert_eq!(manager.snapshot().nodes.len(), 1);
    assert!(manager.snapshot().nodes.contains_key("z"));
    assert_eq!(
        prompt(&adapter, &root(&engine, "alice").0, "")
            .await
            .unwrap(),
        "ZAz"
    );
    let restored = Arc::new(PluginManager::new(dir.path()));
    restored.load_all().await.unwrap();
    let engine = EngineBuilder::new()
        .build()
        .await
        .with_plugins(restored.clone());
    assert_eq!(
        prompt(&PluginAdapter::new(restored), &root(&engine, "alice").0, "")
            .await
            .unwrap(),
        "ZAz"
    );
}
#[tokio::test]
async fn live_install_and_uninstall_publish_snapshots_and_keep_sources() {
    let dir = tempfile::tempdir().unwrap();
    for (id, mode, suffix) in [
        ("direct", "direct", "D"),
        ("z", "chain", "Z"),
        ("b", "chain", "B"),
        ("a", "chain", "A"),
    ] {
        let body = if mode == "direct" {
            format!(
                "class Demo(SingleAgentHook):\n    async def on_prompt(self,ctx,value):\n        return value+'{suffix}'"
            )
        } else {
            format!(
                "class Demo(ChainSingleAgentHook):\n    async def on_prompt(self,ctx,value,*,next):\n        return await next(value+'{suffix}')"
            )
        };
        write(dir.path(), id, mode, "SingleAgentHook", &body);
    }
    let (manager, engine, adapter) = setup(dir.path()).await;
    for id in ["direct", "z", "b", "a"] {
        manager
            .install(&dir.path().join(format!("plugins/{id}.py")))
            .await
            .unwrap();
    }
    let (old, _) = root(&engine, "old");
    assert_eq!(prompt(&adapter, &old, "").await.unwrap(), "DZBA");
    let direct = manager.snapshot().nodes["direct"].clone();
    manager.uninstall("b").await.unwrap();
    assert_eq!(manager.snapshot().order, ["direct", "z"]);
    assert_eq!(
        prompt(&adapter, &root(&engine, "middle_removed").0, "")
            .await
            .unwrap(),
        "DZA"
    );
    manager.uninstall("z").await.unwrap();
    assert!(Arc::ptr_eq(&direct, &manager.snapshot().nodes["direct"]));
    assert_eq!(manager.snapshot().order, ["direct", "a"]);
    assert_eq!(
        prompt(&adapter, &root(&engine, "new").0, "").await.unwrap(),
        "DA"
    );
    assert_eq!(prompt(&adapter, &old, "").await.unwrap(), "DZBA");
    manager.uninstall("a").await.unwrap();
    assert_eq!(manager.snapshot().order, ["direct"]);
    assert_eq!(
        prompt(&adapter, &root(&engine, "direct_only").0, "")
            .await
            .unwrap(),
        "D"
    );
    manager.uninstall("direct").await.unwrap();
    assert!(manager.snapshot().nodes.is_empty());
    assert_eq!(
        prompt(&adapter, &root(&engine, "empty").0, "")
            .await
            .unwrap(),
        ""
    );
    for id in ["direct", "z", "b", "a"] {
        assert!(dir.path().join(format!("plugins/{id}.py")).exists());
        let value = manager.status(id).unwrap();
        assert_eq!(value[id]["installed"], false);
        assert_eq!(value[id]["enabled"], false);
        assert_eq!(value[id]["loaded"], false);
    }
    let restored = PluginManager::new(dir.path());
    restored.load_all().await.unwrap();
    assert!(restored.snapshot().nodes.is_empty());
    manager
        .install(&dir.path().join("plugins/a.py"))
        .await
        .unwrap();
    assert_eq!(
        prompt(&adapter, &root(&engine, "reinstalled").0, "")
            .await
            .unwrap(),
        "A"
    );
}

#[tokio::test]
async fn failed_chain_uninstall_keeps_configuration_and_runtime() {
    let dir = tempfile::tempdir().unwrap();
    for id in ["a", "b"] {
        write(
            dir.path(),
            id,
            "chain",
            "SingleAgentHook",
            "class Demo(ChainSingleAgentHook):\n    async def on_prompt(self,ctx,value,*,next):\n        return await next(value+'ok')",
        );
    }
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load_all().await.unwrap();
    let previous = manager.snapshot();
    let config_path = dir.path().join("plugins/plugins.config");
    let config_before = std::fs::read(&config_path).unwrap();
    std::fs::write(
        dir.path().join("plugins/b.py"),
        "raise RuntimeError('broken chain')",
    )
    .unwrap();
    assert!(manager.uninstall("a").await.is_err());
    assert!(Arc::ptr_eq(&previous, &manager.snapshot()));
    assert_eq!(std::fs::read(config_path).unwrap(), config_before);
    assert_eq!(
        prompt(&adapter, &root(&engine, "alice").0, "")
            .await
            .unwrap(),
        "okok"
    );
}

#[tokio::test]
async fn offline_show_never_imports_unloaded_sources() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
    let marker = dir.path().join("imported");
    std::fs::write(dir.path().join("plugins/offline.py"),
        format!("from pathlib import Path\nPath({:?}).touch()\ndef plugin_config(): raise RuntimeError()\n", marker)).unwrap();
    fae_agent::plugin::config::install(dir.path(), &dir.path().join("plugins/offline.py")).unwrap();
    let status = PluginManager::show(dir.path(), "offline").unwrap();
    assert_eq!(status["offline"]["host"], "offline");
    assert!(status["offline"]["declaration"].is_null());
    assert!(!marker.exists());
}
#[tokio::test]
async fn cancellation_acknowledges_before_release_and_does_not_cancel_other_session() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("acknowledged");
    write(
        dir.path(),
        "cancel",
        "direct",
        "SingleAgentHook",
        &format!(
            r#"class Demo(SingleAgentHook):
    async def on_prompt(self,ctx,value):
        if value == 'cancel':
            await ctx.cancel_run('plugin cancel')
            from pathlib import Path
            Path({marker:?}).touch()
            return 'must be discarded'
        await ctx.session.send('other still alive')
        return value
"#
        ),
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("cancel").await.unwrap();
    let (a, _) = root(&engine, "alice");
    let (b, session) = root(&engine, "bob");
    let (cancel, other) = tokio::join!(prompt(&adapter, &a, "cancel"), prompt(&adapter, &b, "ok"));
    assert!(cancel.is_err());
    assert_eq!(other.unwrap(), "ok");
    assert!(a.state().current().is_cancelled());
    assert!(!b.state().current().is_terminal());
    // Synchronize with the worker through a subsequent request, rather than sleeping.
    prompt(&adapter, &b, "barrier").await.unwrap();
    assert!(
        marker.exists(),
        "the cancellation RPC did not reach its awaiting caller"
    );
    assert_eq!(session.drain_outputs().await.len(), 2);
}
#[tokio::test]
async fn tools_route_from_same_instance_and_reject_native_or_direct_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"class Demo(ToolsHook):
    async def desc(self,ctx,name):
        return {'name':name,'description':self.channel(),'parameters':{'type':'object'}}
    async def exec(self,ctx,request):
        return ToolResponse.completed(self.channel()+':'+request.arguments_json()['text'])
"#;
    write(dir.path(), "echo", "direct", "ToolsHook", body);
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("echo").await.unwrap();
    let (ctx, _) = root(&engine, "alice");
    let tool = adapter.tool(&ctx, "fae_echo__echo").unwrap();
    let desc = tool.desc(&ctx, "fae_echo__echo").await.unwrap();
    assert_eq!(desc["description"], "fae_echo");
    let request = ToolRequest::new("fae_echo__echo".into(), r#"{"text":"hello"}"#.into());
    let mut response = tool.exec(&ctx, request).await.unwrap();
    assert!(
        matches!(response.next().await.unwrap(),ToolRespItem::Completed(text) if text=="fae_echo:hello")
    );
    std::fs::create_dir_all(dir.path().join("plugins/conflict")).unwrap();
    std::fs::write(
        dir.path().join("plugins/conflict/plugin.py"),
        format!("from fae_plugin import *\n{body}"),
    )
    .unwrap();
    let mut config = serde_json::json!({"engine":"py","plugin_version":"1.0.0","description":"Conflict",
        "mounts":[{"point":"ToolsHook","implementation":"plugin.py:Demo","channel":"fae_echo"}]});
    std::fs::write(
        dir.path().join("plugins/conflict/plugin.config"),
        config.to_string(),
    )
    .unwrap();
    assert!(
        manager
            .load("conflict")
            .await
            .unwrap_err()
            .to_string()
            .contains("conflict")
    );
    config["mounts"][0]["channel"] = "default".into();
    std::fs::write(
        dir.path().join("plugins/conflict/plugin.config"),
        config.to_string(),
    )
    .unwrap();
    assert!(
        manager
            .load("conflict")
            .await
            .unwrap_err()
            .to_string()
            .contains("native")
    );
    assert_eq!(manager.snapshot().nodes.len(), 1);
}
#[tokio::test]
async fn context_bridge_fork_stacks_messages_and_child_cancel() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "bridge",
        "direct",
        "SingleAgentHook",
        r#"class Demo(SingleAgentHook):
    async def on_prompt(self,ctx,value):
        engine=await ctx.get_engine()
        assert engine.same_engine(await ctx.get_engine())
        assert (await ctx.scope()).user.user_id == ctx.user_id
        session=await ctx.get_session()
        await session.send('before')
        child=await ctx.fork(ContextChildSpec())
        await child.set_state(ContextState.running())
        view=child.state()
        await view.current()
        await child.cancel('child only')
        await view.changed()
        assert (await view.current()).is_cancelled()
        await view.aclose()
        await ctx.append_stack(ContextStackDraft(TaskMeta('t','p',TaskType(),'plugin','bridge'),ContextStackPhase.STARTED))
        assert len(await ctx.stacks(ContextStackQuery()))==1
        assert not (await ctx.state().current()).is_terminal()
        await session.send('after')
        return value+'ok'
"#,
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("bridge").await.unwrap();
    let (ctx, session) = root(&engine, "alice");
    assert_eq!(prompt(&adapter, &ctx, "").await.unwrap(), "ok");
    let outputs = session.drain_outputs().await;
    assert_eq!(outputs.len(), 2);
    assert!(
        outputs
            .iter()
            .all(|o| o.input_type == "plugin_message" && o.output["plugin_id"] == "bridge")
    );
    assert_eq!(outputs[0].output["text"], "before");
    assert_eq!(outputs[1].output["text"], "after");
    assert!(!ctx.state().current().is_terminal());
}
#[tokio::test]
async fn concurrent_sessions_use_one_worker_without_serializing() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "parallel",
        "direct",
        "SingleAgentHook",
        r#"class Demo(SingleAgentHook):
    def __init__(self):
        self.count=0
        self.ready=asyncio.Event()
    async def on_prompt(self,ctx,value):
        self.count+=1
        if self.count==2: self.ready.set()
        await self.ready.wait()
        return ctx.user_id
"#,
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("parallel").await.unwrap();
    let (a, _) = root(&engine, "alice");
    let (b, _) = root(&engine, "bob");
    let (a, b) = tokio::join!(prompt(&adapter, &a, ""), prompt(&adapter, &b, ""));
    assert_eq!(a.unwrap(), "alice");
    assert_eq!(b.unwrap(), "bob");
}
#[tokio::test]
async fn model_predicates_route_aliases_once_and_detect_conflicts_before_exec() {
    let dir = tempfile::tempdir().unwrap();
    let executions = dir.path().join("executions");
    let body = format!(
        r#"class Demo(ModelHook):
    def __init__(self):
        self.matches = {{}}
    async def match_model(self,ctx,model):
        assert (await ctx.scope()).user.user_id == ctx.user_id
        if model == 'bad-result': return 'invalid'
        self.matches[model] = self.matches.get(model,0)+1
        return model.startswith('family-')
    async def exec(self,ctx,request):
        from pathlib import Path
        with Path({executions:?}).open('a') as output:
            output.write(request.model+'\n')
        return ModelResponse.completed({{'id':'r','object':'chat.completion','created':0,
            'model':request.model,'choices':[{{'index':0,'message':{{'role':'assistant',
            'content':request.model+':'+str(self.matches[request.model])}},'finish_reason':'stop'}}]}})
"#
    );
    write(dir.path(), "alpha", "direct", "ModelHook", &body);
    write(
        dir.path(),
        "beta",
        "direct",
        "ModelHook",
        "class Demo(ModelHook):\n    async def match_model(self,ctx,model): return False\n    async def exec(self,ctx,request): raise AssertionError('unmatched model executed')",
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("alpha").await.unwrap();
    manager.load("beta").await.unwrap();
    let (old, _) = root(&engine, "alice");
    let request = |model: &str| {
        serde_json::from_value::<CreateChatCompletionRequest>(
            serde_json::json!({"model":model,"messages":[{"role":"user","content":"hi"}]}),
        )
        .unwrap()
    };
    for model in ["family-one", "family-two"] {
        let response = adapter.model(&old, request(model)).await.unwrap();
        assert!(matches!(response,ModelResponse::Completed(response)
            if response.choices[0].message.content.as_deref()==Some(&format!("{model}:1"))));
    }
    assert!(
        adapter
            .try_model(&old, request("native"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        adapter
            .model(&old, request("bad-result"))
            .await
            .unwrap_err()
            .to_string()
            .contains("must return bool")
    );
    write(dir.path(), "beta", "direct", "ModelHook", &body);
    manager.load("beta").await.unwrap();
    let (new, _) = root(&engine, "bob");
    assert!(
        adapter
            .model(&new, request("family-one"))
            .await
            .unwrap_err()
            .to_string()
            .contains("plugin_model_conflict")
    );
    assert_eq!(
        std::fs::read_to_string(&executions)
            .unwrap()
            .lines()
            .count(),
        2
    );
    // Existing runs keep the old selection predicates and worker instances.
    let response = adapter.model(&old, request("family-one")).await.unwrap();
    assert!(matches!(response,ModelResponse::Completed(response)
        if response.choices[0].message.content.as_deref()==Some("family-one:3")));
    let restored = PluginManager::new(dir.path());
    restored.load_all().await.unwrap();
    assert_eq!(restored.snapshot().nodes.len(), 2);
    assert!(
        restored.status("alpha").unwrap()["alpha"]["declaration"]["mounts"][0]
            .get("model")
            .is_none()
    );
}

#[tokio::test]
async fn model_stream_and_python_wrapper_pull_host_chunks() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "model",
        "direct",
        "ModelHook",
        r#"class Demo(ModelHook):
    async def match_model(self,ctx,model):
        return model == 'plugin:model/default'
    async def exec(self,ctx,request):
        return ModelResponse.streaming(self.chunks(ctx,request))
    async def chunks(self,ctx,request):
        await ctx.session.send('stream alive')
        base={'id':'r','object':'chat.completion.chunk','created':0,'model':request.model}
        yield {**base,'choices':[{'index':0,'delta':{'role':'assistant','content':'hello'},'finish_reason':None}]}
        yield {**base,'choices':[{'index':0,'delta':{},'finish_reason':'stop'}]}
"#,
    );
    write(
        dir.path(),
        "wrapper",
        "direct",
        "SingleAgentHook",
        r#"class Demo(SingleAgentHook):
    async def on_model_resp(self,ctx,response):
        if not response.is_streaming: return response
        return ModelResponse.streaming(self.wrap(response))
    async def wrap(self,response):
        async for chunk in response:
            if chunk['choices'][0]['delta'].get('content'):
                chunk['choices'][0]['delta']['content']+='!'
            yield chunk
"#,
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("model").await.unwrap();
    manager.load("wrapper").await.unwrap();
    let (ctx, session) = root(&engine, "alice");
    let request: CreateChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model":"plugin:model/default","messages":[{"role":"user","content":"hi"}],"stream":true}))
    .unwrap();
    let response = adapter.model(&ctx, request).await.unwrap();
    let agent = SingleAgentInfo {
        name: "fae".into(),
        desc: "".into(),
        user_id: "alice".into(),
        session_id: "s".into(),
        metadata: Default::default(),
    };
    let response = adapter
        .on_model_resp(
            &SingleAgentHookContext {
                ctx: &ctx,
                plan_id: "p",
                turn_id: 1,
                agent: &agent,
                task_id: None,
            },
            response,
        )
        .await
        .unwrap();
    let chunks = response.into_stream().unwrap().collect::<Vec<_>>().await;
    assert_eq!(chunks.len(), 2);
    assert_eq!(
        chunks[0].as_ref().unwrap().choices[0]
            .delta
            .content
            .as_deref(),
        Some("hello!")
    );
    assert!(chunks.iter().all(|c| c.is_ok()));
    assert_eq!(
        session.drain_outputs().await[0].output["text"],
        "stream alive"
    );
}

#[tokio::test]
async fn cancelling_an_idle_stream_closes_generator_without_another_poll() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "idle",
        "direct",
        "ModelHook",
        r#"class Demo(ModelHook):
    async def match_model(self,ctx,model):
        return model == 'plugin:idle/default'
    def __init__(self):
        self.closed = asyncio.Event()
    async def exec(self,ctx,request):
        if ctx.user_id == 'bob':
            await self.closed.wait()
            return ModelResponse.completed({'id':'r','object':'chat.completion','created':0,
                'model':request.model,'choices':[{'index':0,'message':{'role':'assistant','content':'closed'},
                'finish_reason':'stop'}]})
        return ModelResponse.streaming(self.chunks(request))
    async def chunks(self,request):
        try:
            yield {'id':'r','object':'chat.completion.chunk','created':0,'model':request.model,
                'choices':[{'index':0,'delta':{'content':'visible'},'finish_reason':None}]}
            await asyncio.Event().wait()
        finally:
            self.closed.set()
"#,
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("idle").await.unwrap();
    let (alice, _) = root(&engine, "alice");
    let (bob, _) = root(&engine, "bob");
    let request: CreateChatCompletionRequest = serde_json::from_value(serde_json::json!({
        "model":"plugin:idle/default","messages":[{"role":"user","content":"hi"}],"stream":true
    }))
    .unwrap();
    let mut stream = adapter
        .model(&alice, request.clone())
        .await
        .unwrap()
        .into_stream()
        .unwrap();
    assert_eq!(
        stream.next().await.unwrap().unwrap().choices[0]
            .delta
            .content
            .as_deref(),
        Some("visible")
    );
    alice
        .set_state(ContextState::cancelled("stop idle stream"))
        .unwrap();
    // Keep the stream alive without polling. Bob's request is a cleanup barrier.
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        adapter.model(&bob, request),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(response,ModelResponse::Completed(response) if response.choices[0].message.content.as_deref()==Some("closed"))
    );
    assert!(stream.next().await.unwrap().is_err());
}

#[tokio::test]
async fn agent_json_success_discards_hook_return_and_model_rejects_json_output() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "success",
        "direct",
        "SingleAgentHook",
        r#"class Demo(SingleAgentHook):
    async def on_prompt(self,ctx,value):
        await ctx.set_state(ContextState.succeeded('plugin result'))
        return 'must be discarded'
"#,
    );
    write(
        dir.path(),
        "typed",
        "direct",
        "ModelHook",
        r#"class Demo(ModelHook):
    async def match_model(self,ctx,model):
        return model == 'plugin:typed/default'
    async def exec(self,ctx,request):
        try:
            await ctx.set_state(ContextState.succeeded('invalid model output'))
            raise AssertionError('native model task accepted JSON output')
        except UnsupportedOutputType:
            pass
        return ModelResponse.completed({'id':'r','object':'chat.completion','created':0,
            'model':request.model,'choices':[{'index':0,'message':{'role':'assistant','content':'typed'},
            'finish_reason':'stop'}]})
"#,
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("success").await.unwrap();
    manager.load("typed").await.unwrap();
    let (agent, _) = root(&engine, "alice");
    assert!(prompt(&adapter, &agent, "").await.is_err());
    let state = agent.state().current();
    let ContextState::Succeeded { output } = state.as_ref() else {
        panic!("expected JSON success")
    };
    assert_eq!(
        output.json_snapshot().unwrap(),
        serde_json::json!("plugin result")
    );
    let (model, _) = root(&engine, "bob");
    let request = serde_json::from_value(serde_json::json!({
        "model":"plugin:typed/default","messages":[{"role":"user","content":"hi"}]
    }))
    .unwrap();
    let response = adapter.model(&model, request).await.unwrap();
    assert!(matches!(response,ModelResponse::Completed(response)
        if response.choices[0].message.content.as_deref()==Some("typed")));
    assert!(!model.state().current().is_terminal());
}

#[tokio::test]
async fn passthrough_native_stream_retains_lease_until_eof() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "passthrough",
        "direct",
        "SingleAgentHook",
        "class Demo(SingleAgentHook):\n    pass",
    );
    let (manager, engine, adapter) = setup(dir.path()).await;
    manager.load("passthrough").await.unwrap();
    let (ctx, _) = root(&engine, "alice");
    let agent = SingleAgentInfo {
        name: "fae".into(),
        desc: "".into(),
        user_id: "alice".into(),
        session_id: "s".into(),
        metadata: Default::default(),
    };
    let native=ModelResponse::Streaming(Box::pin(tokio_stream::iter(vec![Ok(
        serde_json::from_value(serde_json::json!({"id":"r","object":"chat.completion.chunk","model":"native","created":0,
            "choices":[{"index":0,"delta":{"content":"native"},"finish_reason":"stop"}]})).unwrap()
    )])));
    let response = adapter
        .on_model_resp(
            &SingleAgentHookContext {
                ctx: &ctx,
                plan_id: "p",
                turn_id: 1,
                agent: &agent,
                task_id: None,
            },
            native,
        )
        .await
        .unwrap();
    assert_eq!(
        manager.status("passthrough").unwrap()["passthrough"]["runtime"]["active_streams"],
        1
    );
    let mut stream = response.into_stream().unwrap();
    assert_eq!(
        stream.next().await.unwrap().unwrap().choices[0]
            .delta
            .content
            .as_deref(),
        Some("native")
    );
    assert!(stream.next().await.is_none());
    assert_eq!(
        manager.status("passthrough").unwrap()["passthrough"]["runtime"]["active_streams"],
        0
    );
}
