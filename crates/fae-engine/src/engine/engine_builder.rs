use super::Engine;
use crate::engine_rt::{EngineRuntime, PlanRuntime};
use fae_agent::hook::plan_hook::PlanHookBuilder;
use fae_agent::{
    PlanBuilder, PlanBuilderWithEnv, PlanBuilderWithEnvWrapper, Runtime, RuntimeSelectExec,
    TaskType, hook::runtime_hook::RuntimeHookBuilder,
};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

pub struct EngineBuilder {
    generation: String,
    bridges: fae_agent::plugin::bridge::BridgeRegistry,
    plugin_snapshot: Option<Arc<fae_agent::plugin::PluginSnapshot>>,
    plan_routes: HashMap<String, String>,
    plan_builders: HashMap<String, Box<dyn PlanBuilder>>,
    plan_hooks: Vec<Box<dyn PlanHookBuilder>>,
    runtimes: EngineRuntime,
}

impl Debug for EngineBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineBuilder")
            .field("plan_builders", &self.plan_builders)
            .field("plan_hook_count", &self.plan_hooks.len())
            .field("runtimes", &self.runtimes)
            .finish()
    }
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self {
            generation: wd_tools::uuid::v4(),
            bridges: Default::default(),
            plugin_snapshot: None,
            plan_routes: HashMap::new(),
            plan_builders: HashMap::new(),
            plan_hooks: Vec::new(),
            runtimes: EngineRuntime::new(),
        }
    }
}

impl EngineBuilder {
    /// Add an explicit JSON boundary without changing a native Runtime's types.
    pub fn register_runtime_bridge(
        &mut self,
        id: &str,
        contracts: Vec<(
            TaskType,
            fae_agent::plugin::bridge::RuntimeOperation,
            fae_agent::plugin::bridge::RuntimeContract,
            bool,
        )>,
    ) -> anyhow::Result<()> {
        use fae_agent::plugin::bridge::BridgedRuntime;
        let runtime = self
            .runtimes
            .runtime_arc(id)
            .ok_or_else(|| anyhow::anyhow!("Runtime is not registered: {id}"))?;
        let descriptors = self.runtimes.descriptors();
        let types = descriptors
            .iter()
            .find(|d| d.id == id)
            .unwrap()
            .task_types
            .clone();
        let mut registry = self.bridges.clone();
        let mut boundaries = HashMap::new();
        for (ty, operation, contract, public) in contracts {
            anyhow::ensure!(types.contains(&ty), "unsupported Runtime bridge TaskType");
            registry.register_runtime(id.into(), ty.clone(), operation, contract.clone())?;
            if public {
                registry.publish_runtime(id.into(), ty.clone(), operation)?;
            }
            boundaries.insert((ty, operation), contract);
        }
        self.runtimes
            .add_raw_runtime_with_tys(Box::new(BridgedRuntime::new(runtime, boundaries)), types);
        self.bridges = registry;
        Ok(())
    }

    pub fn register_plan_bridge(
        &mut self,
        id: &str,
        env_type: String,
        codec: fae_agent::plugin::bridge::ValueCodec,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.plan_builders.contains_key(&env_type),
            "Plan builder is missing"
        );
        let mut bridges = self.bridges.clone();
        bridges.register_plan(id.into(), codec.clone())?;
        self.register_plan_route(id, env_type.clone())?;
        let builder = self.plan_builders.remove(&env_type).unwrap();
        self.plan_builders.insert(
            env_type,
            Box::new(fae_agent::plugin::bridge::BridgedPlanBuilder { builder, codec }),
        );
        self.bridges = bridges;
        Ok(())
    }

    pub fn register_plugins(
        &mut self,
        snapshot: &Arc<fae_agent::plugin::PluginSnapshot>,
    ) -> anyhow::Result<()> {
        use fae_agent::plugin::{PluginMount, provider::*};
        snapshot.validate_providers()?;
        let mut merged_calls = HashMap::<String, fae_agent::plugin::RuntimePublicCalls>::new();
        for node in snapshot.nodes.values() {
            for member in &node.members {
                for mount in &member.declaration.mounts {
                    if let PluginMount::Runtime {
                        id,
                        public_calls: Some(calls),
                        ..
                    } = mount
                    {
                        let merged = merged_calls.entry(id.clone()).or_insert_with(|| {
                            fae_agent::plugin::RuntimePublicCalls {
                                exec: None,
                                select: None,
                            }
                        });
                        if calls.exec.is_some() {
                            merged.exec = calls.exec.clone();
                        }
                        if calls.select.is_some() {
                            merged.select = calls.select.clone();
                        }
                    }
                }
            }
        }
        let mut endpoints = Vec::new();
        let mut runtime_ids = std::collections::HashSet::new();
        for node in snapshot.nodes.values() {
            for member in &node.members {
                for (index, mount) in member.declaration.mounts.iter().enumerate() {
                    match mount {
                        PluginMount::Plan { id, .. } => {
                            anyhow::ensure!(
                                !self.plan_routes.contains_key(id),
                                "duplicate Plan ID: {id}"
                            );
                        }
                        PluginMount::Runtime { id, .. } => {
                            anyhow::ensure!(
                                !self.runtimes.contains_runtime(id),
                                "duplicate Runtime ID: {id}"
                            );
                        }
                        _ => continue,
                    }
                    endpoints.push((
                        ProviderEndpoint {
                            node: node.clone(),
                            id: mount.location().unwrap().into(),
                            member: format!("{}:{index}", member.id),
                        },
                        mount.clone(),
                    ));
                }
            }
        }
        for (endpoint, mount) in endpoints {
            match mount {
                PluginMount::Plan { id, .. } => {
                    let builder = PluginPlanBuilder { endpoint };
                    self.bridges.register_plan(
                        id.clone(),
                        fae_agent::plugin::bridge::ValueCodec::serde::<serde_json::Value>(),
                    )?;
                    self.register_plan_route(id, builder.ty())?;
                    self.add_raw_plan_builder(Box::new(builder));
                }
                PluginMount::Runtime {
                    id,
                    task_types,
                    public_calls: _,
                    ..
                } if runtime_ids.insert(id.clone()) => {
                    use fae_agent::plugin::bridge::*;
                    let public_calls = merged_calls.get(&id);
                    let types = fae_agent::plugin::parse_task_types(&task_types)?;
                    for ty in &types {
                        for operation in [RuntimeOperation::Exec, RuntimeOperation::Select] {
                            self.bridges.register_runtime(
                                id.clone(),
                                ty.clone(),
                                operation,
                                RuntimeContract {
                                    request: ValueCodec::serde::<serde_json::Value>(),
                                    response: ValueCodec::serde::<serde_json::Value>(),
                                },
                            )?;
                        }
                    }
                    for (operation, name) in [
                        (
                            RuntimeOperation::Exec,
                            public_calls.and_then(|p| p.exec.as_ref()),
                        ),
                        (
                            RuntimeOperation::Select,
                            public_calls.and_then(|p| p.select.as_ref()),
                        ),
                    ] {
                        let ty = match name {
                            Some(name) => Some(TaskType::from_name(name)?),
                            None if types.len() == 1 && operation == RuntimeOperation::Exec => {
                                Some(types[0].clone())
                            }
                            None => None,
                        };
                        if let Some(ty) = ty {
                            anyhow::ensure!(
                                types.contains(&ty),
                                "public Runtime TaskType is unsupported"
                            );
                            self.bridges.publish_runtime(id.clone(), ty, operation)?;
                        }
                    }
                    self.add_runtime(PluginRuntimeProxy::new(
                        endpoint,
                        fae_agent::plugin::parse_task_types(&task_types)?,
                        self.generation.clone(),
                    ));
                    let runtime = self.runtimes.runtime_arc(&id).unwrap();
                    let mut native = HashMap::new();
                    for ty in &types {
                        let exec = match ty {
                            TaskType::Model => Some(fae_agent::plugin::leases::model_contract()),
                            TaskType::Tool => Some(fae_agent::plugin::leases::tool_contract()),
                            _ => None,
                        };
                        if let Some(contract) = exec {
                            native.insert((ty.clone(), RuntimeOperation::Exec), contract);
                            let select = match ty {
                                TaskType::Model => RuntimeContract {
                                    request: ValueCodec::serde::<()>(),
                                    response: ValueCodec::serde::<()>(),
                                },
                                _ => RuntimeContract {
                                    request: ValueCodec::serde::<String>(),
                                    response: ValueCodec::serde::<serde_json::Value>(),
                                },
                            };
                            native.insert((ty.clone(), RuntimeOperation::Select), select);
                        }
                    }
                    if !native.is_empty() {
                        self.runtimes.add_raw_runtime_with_tys(
                            Box::new(
                                fae_agent::plugin::bridge::BridgedRuntime::new(runtime, native)
                                    .json_runtime(),
                            ),
                            types,
                        );
                    }
                }
                _ => {}
            }
        }
        self.plugin_snapshot = Some(snapshot.clone());
        Ok(())
    }
    pub fn register_plan_route(
        &mut self,
        id: impl Into<String>,
        env_type: impl Into<String>,
    ) -> anyhow::Result<()> {
        let id = id.into();
        anyhow::ensure!(
            !id.is_empty() && id.trim() == id && !id.chars().any(char::is_control),
            "invalid Plan ID"
        );
        anyhow::ensure!(
            !self.plan_routes.contains_key(&id),
            "duplicate Plan ID: {id}"
        );
        self.plan_routes.insert(id, env_type.into());
        Ok(())
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn runtimes(&self) -> &EngineRuntime {
        &self.runtimes
    }

    pub fn runtimes_mut(&mut self) -> &mut EngineRuntime {
        &mut self.runtimes
    }

    pub fn plan_builders(&self) -> &HashMap<String, Box<dyn PlanBuilder>> {
        &self.plan_builders
    }

    pub fn plan_hooks(&self) -> &[Box<dyn PlanHookBuilder>] {
        &self.plan_hooks
    }

    pub fn plan_builder(&self, ty: &str) -> Option<&dyn PlanBuilder> {
        self.plan_builders.get(ty).map(|builder| builder.as_ref())
    }

    pub fn contains_plan_builder(&self, ty: &str) -> bool {
        self.plan_builders.contains_key(ty)
    }

    pub fn add_raw_plan_builder(
        &mut self,
        builder: Box<dyn PlanBuilder>,
    ) -> Option<Box<dyn PlanBuilder>> {
        let ty = builder.ty();
        self.plan_builders.insert(ty, builder)
    }

    pub fn add_plan_builder_with_env_box<ENV>(
        &mut self,
        builder: Box<dyn PlanBuilderWithEnv<ENV>>,
    ) -> Option<Box<dyn PlanBuilder>>
    where
        ENV: Debug + Send + Sync + 'static,
    {
        self.add_raw_plan_builder(Box::new(PlanBuilderWithEnvWrapper::new(builder)))
    }

    pub fn add_plan_builder<T, ENV>(&mut self, builder: T) -> Option<Box<dyn PlanBuilder>>
    where
        ENV: Debug + Send + Sync + 'static,
        T: PlanBuilderWithEnv<ENV>,
    {
        self.add_plan_builder_with_env_box(Box::new(builder))
    }

    pub fn remove_plan_builder(&mut self, ty: &str) -> Option<Box<dyn PlanBuilder>> {
        self.plan_builders.remove(ty)
    }

    pub fn add_plan_hook<H>(&mut self, hook: H)
    where
        H: PlanHookBuilder,
    {
        self.plan_hooks.push(Box::new(hook));
    }

    pub fn add_plan_hook_box(&mut self, hook: Box<dyn PlanHookBuilder>) {
        self.plan_hooks.push(hook);
    }

    pub fn remove_plan_hooks(&mut self) -> Vec<Box<dyn PlanHookBuilder>> {
        std::mem::take(&mut self.plan_hooks)
    }

    pub fn add_raw_runtime(&mut self, rt: Box<dyn Runtime>) -> Option<Arc<dyn Runtime>> {
        self.runtimes.add_raw_runtime(rt)
    }

    pub fn add_runtime_with_tys(
        &mut self,
        rt: Box<dyn Runtime>,
        tys: impl IntoIterator<Item = TaskType>,
    ) -> Option<Arc<dyn Runtime>> {
        self.runtimes.add_raw_runtime_with_tys(rt, tys)
    }

    pub fn add_runtime_arc<Req, Resp, Cond, Info>(
        &mut self,
        rt: Arc<dyn RuntimeSelectExec<Req, Resp, Cond, Info>>,
    ) -> Option<Arc<dyn Runtime>>
    where
        Req: Debug + Send + 'static,
        Resp: Debug + Send + 'static,
        Cond: Debug + Send + 'static,
        Info: Debug + Send + 'static,
    {
        self.runtimes.add_runtime(rt)
    }

    pub fn add_runtime<Req, Resp, Cond, Info, R>(&mut self, rt: R) -> Option<Arc<dyn Runtime>>
    where
        Req: Debug + Send + 'static,
        Resp: Debug + Send + 'static,
        Cond: Debug + Send + 'static,
        Info: Debug + Send + 'static,
        R: RuntimeSelectExec<Req, Resp, Cond, Info>,
    {
        self.add_runtime_arc(Arc::new(rt))
    }

    pub fn remove_runtime(&mut self, id: &str) -> Option<Arc<dyn Runtime>> {
        self.runtimes.remove_runtime(id)
    }

    pub fn add_runtime_hook<H>(&mut self, ty: TaskType, hook: H)
    where
        H: RuntimeHookBuilder,
    {
        self.runtimes.add_runtime_hook(ty, Box::new(hook));
    }

    pub fn add_runtime_hook_box(&mut self, ty: TaskType, hook: Box<dyn RuntimeHookBuilder>) {
        self.runtimes.add_runtime_hook(ty, hook);
    }

    pub fn remove_runtime_hooks(
        &mut self,
        ty: &TaskType,
    ) -> Option<Vec<Box<dyn RuntimeHookBuilder>>> {
        self.runtimes.remove_runtime_hooks(ty)
    }

    pub async fn build(self) -> Engine {
        self.build_with_adapters().await.engine
    }

    pub async fn build_with_adapters(mut self) -> crate::EngineAdapters {
        if !self.runtimes.contains_runtime(PlanRuntime::ID) {
            self.add_runtime(PlanRuntime::new());
        }
        let rt = self.runtimes.build().await;
        let mut engine = Engine::with_plan_hooks(self.plan_builders, self.plan_hooks, rt)
            .with_generation(self.generation)
            .with_plan_routes(self.plan_routes);
        if let Some(snapshot) = self.plugin_snapshot {
            engine = engine.with_plugin_snapshot(snapshot);
        }
        crate::EngineAdapters {
            engine,
            bridges: Arc::new(self.bridges),
        }
    }
}
