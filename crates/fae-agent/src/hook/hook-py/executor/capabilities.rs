use super::*;

impl Worker {
    pub(super) fn call_for(&self, request: &Message) -> Option<Arc<PluginCall>> {
        let payload = request.as_request().ok()?;
        let value = serde_json::to_value(payload).ok()?;
        let call_id = value
            .get("call_id")
            .or_else(|| value.get("access")?.get("call_id"));
        let calls = self.calls.lock().unwrap();
        if let Some(id) = call_id.and_then(Value::as_str) {
            return calls.get(id).and_then(Weak::upgrade);
        }
        match payload {
            RequestPayload::SessionExecutionSteer(_)
            | RequestPayload::SessionExecutionStop(_)
            | RequestPayload::SessionExecutionGet(_) => {
                calls.values().filter_map(Weak::upgrade).find(|call| {
                    !*call.released.borrow()
                        && call.ctx.scope().ok().is_some_and(|scope| {
                            scope.execution.as_ref().is_some_and(|binding| {
                                Some(binding.session.key().session_id) == request.session_id
                                    && value["execution_id"].as_str()
                                        == Some(binding.execution_id.as_str())
                            })
                        })
                })
            }
            _ => None,
        }
    }
}

impl PluginCall {
    fn access(
        &self,
        request: &Message,
        access: Option<&protocol::PluginAccess>,
    ) -> anyhow::Result<Ctx> {
        anyhow::ensure!(
            !*self.released.borrow() && !self.release_requested.load(Ordering::SeqCst),
            "HandleExpired"
        );
        let ctx = if let Some(access) = access {
            anyhow::ensure!(
                access.call_id.as_str() == self.id && self.members.contains_key(&access.member),
                "HandleExpired"
            );
            let handle = &access.binding.handle;
            anyhow::ensure!(
                handle == &access.member
                    || handle.starts_with(&format!("{}/child/", access.member)),
                "HandleExpired"
            );
            let ctx = self
                .contexts
                .lock()
                .unwrap()
                .get(handle)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
            let expected = plugin::codec::context(&ctx, handle).binding;
            anyhow::ensure!(
                access.binding.execution_id == expected.execution_id
                    && access.binding.invocation_id == expected.invocation_id,
                "HandleExpired"
            );
            ctx
        } else {
            self.ctx.clone()
        };
        anyhow::ensure!(
            !ctx.state().current().is_terminal() && !self.root.state().current().is_terminal(),
            "ContextFinished"
        );
        let scope = ctx.scope().ok();
        let binding = scope.as_ref().and_then(|s| s.execution.as_ref());
        anyhow::ensure!(
            request.session_id == binding.map(|e| e.session.key().session_id),
            "HandleExpired"
        );
        if let Some(binding) = binding {
            let view = binding.session.execution(Some(&binding.execution_id))?;
            anyhow::ensure!(!view.status.is_terminal(), "ContextFinished");
            let invocation = view
                .invocations
                .iter()
                .find(|i| i.invocation_id == binding.invocation_id)
                .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
            anyhow::ensure!(!invocation.status.is_terminal(), "ContextFinished");
        }
        Ok(ctx)
    }

    pub(super) async fn host(&self, request: &Message) -> anyhow::Result<Value> {
        use protocol::RequestPayload::*;
        let payload = request.as_request()?;
        let access = match payload {
            PluginContextGet(p)
            | PluginContextExport(p)
            | PluginHostEngine(p)
            | PluginHostSession(p) => Some(p),
            PluginContextFork(p) => Some(&p.access),
            PluginContextCancel(p) => Some(&p.access),
            PluginOutputSend(p) => Some(&p.access),
            PluginNext(p) => Some(&p.access),
            PluginContextStacks(p) => Some(&p.access),
            PluginStreamNext(p) | PluginStreamClose(p) => p.access.as_ref(),
            SessionExecutionSteer(_) | SessionExecutionStop(_) | SessionExecutionGet(_) => None,
            _ => anyhow::bail!("unsupported worker capability"),
        };
        let ctx = self.access(request, access)?;
        let scope = ctx.scope().ok();
        let execution = scope.as_ref().and_then(|s| s.execution.as_ref());
        let require_execution = || execution.ok_or_else(|| anyhow::anyhow!("SessionUnavailable"));
        if let PluginNext(p) = payload {
            anyhow::ensure!(
                p.point == self.point && p.method == self.method,
                "next hook mismatch"
            );
            protocol::validate_hook_value(&self.point, &self.method, &p.argument, false)?;
            anyhow::ensure!(
                p.access.binding.handle == p.access.member
                    && self
                        .next_members
                        .lock()
                        .unwrap()
                        .insert(p.access.member.clone()),
                "next already used or not owned"
            );
            // Do not hold the receipt lock across onion execution: inner layers may call the host.
            let result = self
                .worker
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?
                .rpc(
                    RequestPath::PluginNext,
                    serde_json::to_value(p)?,
                    self.deadline,
                )
                .await?;
            protocol::validate_hook_value(&self.point, &self.method, &result, true)?;
            return Ok(result);
        }
        let mut receipts = self.receipts.lock().await;
        let id = request.request_id.as_ref().unwrap().as_str();
        if let Some(receipt) = receipts.get(id) {
            anyhow::ensure!(
                receipt["request"] == serde_json::to_value(request)?,
                "request_id conflict"
            );
            return Ok(receipt["result"].clone());
        }
        anyhow::ensure!(receipts.len() < 256, "plugin_busy: receipts");
        let result = match payload {
            PluginContextGet(_) => {
                serde_json::to_value(require_execution()?.session.read_summary())?
            }
            PluginContextExport(_) => {
                serde_json::to_value(require_execution()?.session.export_stored_context().await?)?
            }
            PluginContextStacks(p) => {
                anyhow::ensure!(
                    p.query.limit.is_none_or(|limit| limit <= 256),
                    "invalid stack limit"
                );
                serde_json::to_value(ctx.stacks(p.query.clone())?)?
            }
            PluginHostEngine(_) => json!({"handle":format!("{}:engine",self.id)}),
            PluginHostSession(_) => plugin::codec::session(&ctx, &access.unwrap().binding.handle)?,
            PluginOutputSend(p) => {
                anyhow::ensure!(!p.content.is_empty(), "invalid empty output");
                let binding = require_execution()?;
                let output_id = format!("{}:{id}", self.id);
                binding.session.report(
                    &binding.execution_id,
                    ExecutionReport::Output {
                        invocation_id: binding.invocation_id.clone(),
                        output_id: output_id.clone(),
                        source: protocol::OutputSource::Plugin {
                            plugin_id: self.members[&p.access.member].0.clone(),
                        },
                        content: p.content.clone(),
                    },
                )?;
                json!({"output_id":output_id})
            }
            SessionExecutionSteer(p) => {
                let binding = require_execution()?;
                anyhow::ensure!(p.execution_id == binding.execution_id, "HandleExpired");
                binding.session.steer(
                    &p.execution_id,
                    request.request_id.clone().unwrap(),
                    p.input.clone(),
                )?;
                json!({"execution_id":p.execution_id,"request_id":request.request_id,"accepted":true})
            }
            SessionExecutionStop(p) => {
                let binding = require_execution()?;
                anyhow::ensure!(p.execution_id == binding.execution_id, "HandleExpired");
                let accepted = binding.session.stop(
                    &p.execution_id,
                    p.reason
                        .clone()
                        .unwrap_or_else(|| "Stopped by plugin".into()),
                )?;
                json!({"execution_id":p.execution_id,"accepted":accepted,"status":binding.session.execution(Some(&p.execution_id))?.status})
            }
            SessionExecutionGet(p) => {
                let binding = require_execution()?;
                let view = binding.session.execution(p.execution_id.as_ref())?;
                let acceptance = p
                    .input_request_id
                    .as_ref()
                    .map(|id| protocol::AcceptanceFact {
                        request_id: id.clone(),
                        certainty: protocol::AcceptanceCertainty::Known,
                        execution_id: Some(binding.execution_id.clone()),
                        input: view.inputs.iter().find(|i| &i.request_id == id).cloned(),
                        outcome: view.outcome.clone(),
                    });
                json!({"execution":view,"acceptance":acceptance})
            }
            PluginContextCancel(p) => {
                // Local cancellation is restricted to owned children. Root control uses stop.
                anyhow::ensure!(
                    p.access.binding.handle.contains("/child/"),
                    "root cancellation requires execution stop"
                );
                ctx.set_state(ContextState::cancelled(
                    p.reason.as_deref().unwrap_or("Cancelled by plugin"),
                ))?;
                if let Some(binding) = execution {
                    let mut invocation = binding
                        .session
                        .execution(Some(&binding.execution_id))?
                        .invocations
                        .into_iter()
                        .find(|i| i.invocation_id == binding.invocation_id)
                        .unwrap();
                    invocation.status = InvocationStatus::Cancelled;
                    binding.session.report(
                        &binding.execution_id,
                        ExecutionReport::InvocationChanged(invocation),
                    )?;
                }
                json!({})
            }
            PluginContextFork(p) => {
                let binding = require_execution()?;
                anyhow::ensure!(
                    !p.spec.operation.trim().is_empty() && p.spec.operation.len() <= 256,
                    "invalid child operation"
                );
                anyhow::ensure!(
                    self.contexts.lock().unwrap().len() < 128,
                    "plugin_busy: contexts"
                );
                let child = ctx.fork(ContextChildSpec::default())?;
                let child_scope = child.scope()?;
                let child_binding = child_scope.execution.as_ref().unwrap();
                let view = binding.session.execution(Some(&binding.execution_id))?;
                let parent = view
                    .invocations
                    .iter()
                    .find(|i| i.invocation_id == binding.invocation_id)
                    .unwrap();
                let invocation = InvocationView {
                    invocation_id: child_binding.invocation_id.clone(),
                    parent_invocation_id: child_binding.parent_invocation_id.clone(),
                    kind: InvocationKind::Runtime,
                    plan_id: parent.plan_id.clone(),
                    task_id: None,
                    runtime_id: Some(p.spec.operation.clone()),
                    agent_id: child_scope.agent_id.clone(),
                    workflow_id: child_scope.workflow_id.clone(),
                    node_id: child_scope.node_id.clone(),
                    model: None,
                    status: InvocationStatus::Running,
                };
                binding.session.report(
                    &binding.execution_id,
                    ExecutionReport::InvocationRegistered(invocation),
                )?;
                child.set_state(ContextState::Running)?;
                let handle = format!("{}/child/{}", p.access.member, wd_tools::uuid::v4());
                let result = serde_json::to_value(plugin::codec::context(&child, &handle))?;
                self.contexts.lock().unwrap().insert(handle, child);
                result
            }
            PluginStreamNext(p) => {
                anyhow::ensure!(p.call_id.as_str() == self.id, "HandleExpired");
                let mut streams = self.streams.lock().await;
                let stream = streams
                    .get_mut(&p.stream_id)
                    .ok_or_else(|| anyhow::anyhow!("HandleExpired"))?;
                match stream.next().await {
                    Some(chunk) => json!({"eof":false,"chunk":chunk?}),
                    None => json!({"eof":true}),
                }
            }
            PluginStreamClose(p) => {
                anyhow::ensure!(p.call_id.as_str() == self.id, "HandleExpired");
                self.streams.lock().await.remove(&p.stream_id);
                json!({})
            }
            _ => unreachable!(),
        };
        receipts.insert(id.into(), json!({"request":request,"result":result}));
        Ok(result)
    }
}
