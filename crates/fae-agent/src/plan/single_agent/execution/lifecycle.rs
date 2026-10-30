use super::*;

#[async_trait::async_trait]
impl Plan for SingleAgentPlan {
    fn id(&self) -> &str {
        &self.id
    }

    async fn init(&mut self) -> anyhow::Result<PlanNext> {
        if self.session.cancel_requested() {
            self.finish_on_drop = true;
            return Ok(PlanNext::End);
        }
        self.emit(
            self.template.agent.name.clone(),
            SessionEventData::TurnStarted {
                input: self.input.clone(),
            },
        )
        .await?;
        Ok(PlanNext::Tasks(vec![self.memory_task()]))
    }

    async fn next(&mut self, mut task_result: TaskResponse) -> anyhow::Result<PlanNext> {
        if let Some(todo_plan) = &mut self.todo_plan {
            return todo_plan.next(task_result).await;
        }
        if self.session.cancel_requested() {
            self.finish_on_drop = true;
            return Ok(PlanNext::End);
        }
        match self.stage {
            SingleAgentStage::Memory => {
                let task_id = task_result.meta.id.clone();
                let response = TaskResp::<UserMemoryResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| {
                        anyhow::anyhow!("expected UserMemoryResponse for memory query")
                    })?;
                let UserMemoryResponse::Memories { memories, .. } = response.resp else {
                    anyhow::bail!("expected memory query response");
                };
                self.user_memory = self
                    .template
                    .hook
                    .on_memory(&self.hook_context(Some(&task_id)), memories)
                    .await?;
                self.stage = SingleAgentStage::History;
                Ok(PlanNext::Tasks(vec![self.history_task()]))
            }
            SingleAgentStage::History => {
                let task_id = task_result.meta.id.clone();
                let response = TaskResp::<SessionResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected SessionResponse for history query"))?;
                let SessionResponse::History { messages, .. } = response.resp else {
                    anyhow::bail!("expected history query response");
                };
                let messages = self
                    .template
                    .hook
                    .on_history(&self.hook_context(Some(&task_id)), messages)
                    .await?;
                self.prepare_messages(messages, Some(&task_id)).await?;
                Ok(PlanNext::Tasks(vec![self.next_model_task().await?]))
            }
            SingleAgentStage::Compression => {
                let response =
                    TaskResp::<WorkflowActionResponse>::try_from_response(&mut task_result)
                        .ok_or_else(|| {
                            anyhow::anyhow!("expected WorkflowActionResponse after compression")
                        })?;
                let content = response.resp.output.as_str().ok_or_else(|| {
                    anyhow::anyhow!("compression runtime returned a non-string output")
                })?;
                Ok(PlanNext::Tasks(vec![
                    self.apply_compression(content.to_string()).await?,
                ]))
            }
            SingleAgentStage::Model => {
                let task_id = task_result.meta.id.clone();
                let response = TaskResp::<ModelResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected ModelResponse"))?;
                let response = self
                    .template
                    .hook
                    .on_model_resp(&self.hook_context(Some(&task_id)), response.resp)
                    .await?;
                self.handle_model_response(response).await
            }
            SingleAgentStage::Tools { .. } => {
                let task_id = task_result.meta.id.clone();
                let kind = self
                    .pending_tools
                    .get(&task_id)
                    .map(|pending| pending.kind.clone())
                    .ok_or_else(|| anyhow::anyhow!("unknown tool task `{task_id}`"))?;
                match kind {
                    PendingCallKind::Tool => {
                        let response =
                            TaskResp::<ToolResponse>::try_from_response(&mut task_result)
                                .ok_or_else(|| anyhow::anyhow!("expected ToolResponse"))?;
                        self.handle_tool_response(task_id, response.resp).await
                    }
                    PendingCallKind::Mcp => {
                        let response = TaskResp::<McpResponse>::try_from_response(&mut task_result)
                            .ok_or_else(|| anyhow::anyhow!("expected McpResponse"))?;
                        self.handle_mcp_response(task_id, response.resp).await
                    }
                }
            }
            SingleAgentStage::Save => {
                let response = TaskResp::<SessionResponse>::try_from_response(&mut task_result)
                    .ok_or_else(|| anyhow::anyhow!("expected SessionResponse after save"))?;
                anyhow::ensure!(
                    matches!(response.resp, SessionResponse::Added { .. }),
                    "expected session add response"
                );
                self.unsaved_messages.clear();

                if let Some(pending) = self.session.finish_or_take_pending() {
                    self.append_user_inputs(pending).await?;
                    Ok(PlanNext::Tasks(vec![self.next_model_task().await?]))
                } else {
                    self.finish_on_drop = true;
                    self.ctx
                        .set_state(crate::ContextState::succeeded(Value::String(
                            self.final_output.clone(),
                        )))?;
                    self.emit(
                        self.template.agent.name.clone(),
                        SessionEventData::Completed {
                            content: self.final_output.clone(),
                        },
                    )
                    .await?;
                    Ok(PlanNext::End)
                }
            }
        }
    }

    async fn abort(&mut self, _code: i32, error: String) {
        let _ = self
            .ctx
            .set_state(crate::ContextState::failed(error.clone()));
        if let Some(todo_plan) = &mut self.todo_plan {
            todo_plan.abort(_code, error).await;
            return;
        }
        self.session.abort_turn();
        self.owns_active_turn = false;
        let _ = self
            .emit(
                self.template.agent.name.clone(),
                SessionEventData::Failed { error },
            )
            .await;
    }
}

impl Drop for SingleAgentPlan {
    fn drop(&mut self) {
        if self.owns_active_turn {
            if self.finish_on_drop {
                self.session.finish_turn();
            } else {
                self.session.abort_turn();
            }
        }
    }
}
