use super::*;

impl SingleAgentPlan {
    async fn consume_model(
        &mut self,
        response: ModelResponse,
    ) -> anyhow::Result<(
        String,
        Vec<ChatCompletionMessageToolCalls>,
        Option<FinishReason>,
    )> {
        match response {
            ModelResponse::Completed(response) => {
                let choice = response
                    .choices
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("model returned no choices"))?;
                let finish_reason = choice.finish_reason;
                let content = choice.message.content.unwrap_or_default();
                if !content.is_empty() {
                    self.emit(
                        self.template.model.model.clone(),
                        SessionEventData::ModelOutput {
                            content: content.clone(),
                        },
                    )
                    .await?;
                }
                Ok((
                    content,
                    choice.message.tool_calls.unwrap_or_default(),
                    finish_reason,
                ))
            }
            ModelResponse::Streaming(mut stream) => {
                let mut content = String::new();
                let mut tool_calls = BTreeMap::<u32, ToolCallAccumulator>::new();
                let mut finish_reason = None;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    for choice in chunk.choices {
                        if choice.finish_reason.is_some() {
                            finish_reason = choice.finish_reason;
                        }
                        if let Some(delta) = choice.delta.content.filter(|delta| !delta.is_empty())
                        {
                            content.push_str(&delta);
                            self.emit(
                                self.template.model.model.clone(),
                                SessionEventData::ModelOutput { content: delta },
                            )
                            .await?;
                        }
                        if let Some(reasoning) = choice
                            .delta
                            .reasoning_content
                            .filter(|reasoning| !reasoning.is_empty())
                        {
                            self.emit(
                                self.template.model.model.clone(),
                                SessionEventData::ModelReasoning { content: reasoning },
                            )
                            .await?;
                        }
                        for call in choice.delta.tool_calls.unwrap_or_default() {
                            tool_calls.entry(call.index).or_default().merge(call);
                        }
                    }
                }
                let tool_calls = tool_calls
                    .into_values()
                    .map(ToolCallAccumulator::finish)
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok((content, tool_calls, finish_reason))
            }
        }
    }

    pub(super) async fn handle_model_response(
        &mut self,
        response: ModelResponse,
    ) -> anyhow::Result<PlanNext> {
        let (content, tool_calls, finish_reason) = self.consume_model(response).await?;
        match finish_reason {
            Some(FinishReason::Length) => anyhow::bail!(
                "model output reached max_completion_tokens before producing a complete answer; \
                 increase model.max_completion_tokens or reduce the request context"
            ),
            Some(FinishReason::ContentFilter) => {
                anyhow::bail!("model output was blocked by the content filter")
            }
            Some(FinishReason::FunctionCall) => {
                anyhow::bail!("legacy model function calls are not supported")
            }
            Some(FinishReason::ToolCalls) if tool_calls.is_empty() => {}
            Some(FinishReason::Stop | FinishReason::ToolCalls) | None => {}
        }

        if tool_calls.is_empty() {
            if content.trim().is_empty() {
                if self.empty_model_retries < MAX_EMPTY_MODEL_RETRIES {
                    self.empty_model_retries += 1;
                    self.messages.push(ChatCompletionRequestMessage::User(
                        ChatCompletionRequestUserMessage {
                            content: EMPTY_MODEL_RETRY_PROMPT.into(),
                            ..Default::default()
                        },
                    ));
                    return Ok(PlanNext::Tasks(vec![self.next_model_task().await?]));
                }
                anyhow::bail!(
                    "model completed without assistant output after {} retry \
                     (finish_reason: {:?})",
                    self.empty_model_retries,
                    finish_reason
                );
            }
            self.empty_model_retries = 0;
            self.final_output = content.clone();
            self.messages.push(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    content: Some(content.clone().into()),
                    ..Default::default()
                },
            ));
            self.unsaved_messages
                .push(SessionMessage::assistant(content));

            let pending = self.session.take_pending_inputs();
            if !pending.is_empty() {
                self.append_user_inputs(pending).await?;
                return Ok(PlanNext::Tasks(vec![self.next_model_task().await?]));
            }

            if self.persist_session {
                self.stage = SingleAgentStage::Save;
                return Ok(PlanNext::Tasks(vec![self.save_task().await?]));
            }

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
            return Ok(PlanNext::End);
        }

        self.empty_model_retries = 0;
        self.tool_iterations += 1;
        anyhow::ensure!(
            self.tool_iterations <= self.template.model.max_tool_iterations,
            "model exceeded max_tool_iterations ({})",
            self.template.model.max_tool_iterations
        );

        let assistant_message = ChatCompletionRequestAssistantMessage {
            content: (!content.is_empty())
                .then_some(ChatCompletionRequestAssistantMessageContent::Text(content)),
            tool_calls: Some(tool_calls.clone()),
            ..Default::default()
        };
        self.messages
            .push(ChatCompletionRequestMessage::Assistant(assistant_message));

        let mut tasks = Vec::with_capacity(tool_calls.len());
        let mut available_tools = self
            .template
            .tool_routes
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        available_tools.sort();
        for call in tool_calls {
            let ChatCompletionMessageToolCalls::Function(call) = call else {
                anyhow::bail!("custom tool calls are not supported");
            };
            let call_id = call.id;
            let tool_name = call.function.name;
            let arguments = call.function.arguments;
            let enters_todo_mode = self.terminal_tool_completion.is_none()
                && tool_name == TODO_TOOL_NAME
                && serde_json::from_str::<serde_json::Value>(&arguments)
                    .ok()
                    .and_then(|arguments| {
                        arguments
                            .get("operation")
                            .and_then(serde_json::Value::as_str)
                            .map(|operation| operation == "create")
                    })
                    .unwrap_or(false);
            self.emit(
                tool_name.clone(),
                SessionEventData::ToolCall {
                    call_id: call_id.clone(),
                    arguments: arguments.clone(),
                },
            )
            .await?;
            let Some(route) = self.template.tool_routes.get(&tool_name).cloned() else {
                let output = serde_json::json!({
                    "error": {
                        "type": "unavailable_tool",
                        "message": format!(
                            "Tool `{tool_name}` is unavailable. Call one of the available tools \
                             using its exact name."
                        ),
                        "requested_tool": tool_name,
                        "available_tools": available_tools,
                    }
                })
                .to_string();
                self.emit(
                    tool_name,
                    SessionEventData::ToolOutput {
                        call_id: call_id.clone(),
                        output: output.clone(),
                        completed: true,
                    },
                )
                .await?;
                self.messages.push(ChatCompletionRequestMessage::Tool(
                    ChatCompletionRequestToolMessage {
                        content: ChatCompletionRequestToolMessageContent::Text(output),
                        tool_call_id: call_id,
                    },
                ));
                continue;
            };
            let routed = match route {
                CallableRoute::Tool(runtime_tool_name) => {
                    let request = ToolRequest::new(runtime_tool_name, arguments);
                    Some((self.tool_task(request).await?, PendingCallKind::Tool))
                }
                CallableRoute::Mcp { server, tool_name } => Some((
                    self.task(TaskType::Mcp, McpRequest::new(server, tool_name, arguments)),
                    PendingCallKind::Mcp,
                )),
                CallableRoute::Agent { sources } => {
                    let call: SubAgentCall = serde_json::from_str(&arguments).map_err(|error| {
                        anyhow::anyhow!("invalid `{AGENT_TOOL_NAME}` arguments: {error}")
                    })?;
                    anyhow::ensure!(
                        !call.input.trim().is_empty(),
                        "sub-agent input cannot be empty"
                    );
                    let source = sources.get(&call.agent_id).cloned().ok_or_else(|| {
                        anyhow::anyhow!("sub-agent `{}` is not configured", call.agent_id)
                    })?;
                    let invocation = AgentToolInvocation::for_sub_agent(
                        call.agent_id,
                        source,
                        self.template.agent.name.clone(),
                        self.template.ancestor_agents.clone(),
                    );
                    Some((
                        self.tool_task(
                            ToolRequest::new(AGENT_TOOL_NAME.to_string(), arguments)
                                .with_invocation(ToolInvocation::Agent(invocation)),
                        )
                        .await?,
                        PendingCallKind::Tool,
                    ))
                }
                CallableRoute::Workflow { workflow_ids } => {
                    let call: WorkflowCall = serde_json::from_str(&arguments).map_err(|error| {
                        anyhow::anyhow!("invalid `{WORKFLOW_TOOL_NAME}` arguments: {error}")
                    })?;
                    anyhow::ensure!(
                        workflow_ids.contains(&call.workflow_id),
                        "workflow `{}` is not configured",
                        call.workflow_id
                    );
                    Some((
                        self.tool_task(ToolRequest::new(WORKFLOW_TOOL_NAME.to_string(), arguments))
                            .await?,
                        PendingCallKind::Tool,
                    ))
                }
                CallableRoute::Orchestrated { message } => {
                    self.emit(
                        tool_name.clone(),
                        SessionEventData::ToolOutput {
                            call_id: call_id.clone(),
                            output: message.clone(),
                            completed: true,
                        },
                    )
                    .await?;
                    self.messages.push(ChatCompletionRequestMessage::Tool(
                        ChatCompletionRequestToolMessage {
                            content: ChatCompletionRequestToolMessageContent::Text(message),
                            tool_call_id: call_id.clone(),
                        },
                    ));
                    None
                }
            };
            let Some((task, kind)) = routed else {
                continue;
            };
            self.pending_tools.insert(
                task.meta.id.clone(),
                PendingCall {
                    call_id,
                    tool_name,
                    kind,
                    enters_todo_mode,
                },
            );
            tasks.push(task);
        }
        if tasks.is_empty() {
            return Ok(PlanNext::Tasks(vec![self.next_model_task().await?]));
        }
        self.stage = SingleAgentStage::Tools {
            remaining: tasks.len(),
        };
        Ok(PlanNext::Tasks(tasks))
    }

    pub(super) async fn handle_tool_response(
        &mut self,
        task_id: String,
        response: ToolResponse,
    ) -> anyhow::Result<PlanNext> {
        let mut response = self
            .template
            .hook
            .on_tools_resp(&self.hook_context(Some(&task_id)), response)
            .await?;
        let pending = self
            .pending_tools
            .remove(&task_id)
            .ok_or_else(|| anyhow::anyhow!("unknown tool task `{task_id}`"))?;

        let completed_output = loop {
            match response.next().await? {
                ToolRespItem::Streaming(output) => {
                    self.emit(
                        pending.tool_name.clone(),
                        SessionEventData::ToolOutput {
                            call_id: pending.call_id.clone(),
                            output,
                            completed: false,
                        },
                    )
                    .await?;
                }
                ToolRespItem::Completed(output) => {
                    self.emit(
                        pending.tool_name.clone(),
                        SessionEventData::ToolOutput {
                            call_id: pending.call_id.clone(),
                            output: output.clone(),
                            completed: true,
                        },
                    )
                    .await?;
                    break output;
                }
            }
        };

        self.finish_tool_call(pending, completed_output).await
    }

    pub(super) async fn handle_mcp_response(
        &mut self,
        task_id: String,
        response: McpResponse,
    ) -> anyhow::Result<PlanNext> {
        let pending = self
            .pending_tools
            .remove(&task_id)
            .ok_or_else(|| anyhow::anyhow!("unknown MCP task `{task_id}`"))?;
        self.emit(
            pending.tool_name.clone(),
            SessionEventData::ToolOutput {
                call_id: pending.call_id.clone(),
                output: response.output.clone(),
                completed: true,
            },
        )
        .await?;
        self.finish_tool_call(pending, response.output).await
    }

    async fn finish_tool_call(
        &mut self,
        pending: PendingCall,
        output: String,
    ) -> anyhow::Result<PlanNext> {
        if pending.enters_todo_mode {
            anyhow::ensure!(
                self.pending_todo_output.is_none(),
                "todo create was called more than once in one agent step"
            );
            self.pending_todo_output = Some(output.clone());
        }
        let terminal_output = self
            .terminal_tool_completion
            .as_ref()
            .filter(|terminal| terminal.tool_name == pending.tool_name)
            .map(|terminal| (terminal.output_formatter)(&output))
            .transpose()?;
        self.messages.push(ChatCompletionRequestMessage::Tool(
            ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(output),
                tool_call_id: pending.call_id,
            },
        ));

        let all_tools_completed = {
            let SingleAgentStage::Tools { remaining } = &mut self.stage else {
                anyhow::bail!("received tool response outside tool stage");
            };
            *remaining -= 1;
            *remaining == 0
        };
        if let Some(content) = terminal_output {
            anyhow::ensure!(
                self.pending_terminal_output.is_none(),
                "terminal tool was called more than once in one agent step"
            );
            self.pending_terminal_output = Some(content);
        }
        if !all_tools_completed {
            return Ok(PlanNext::Tasks(Vec::new()));
        }

        if let Some(content) = self.pending_terminal_output.take() {
            self.terminal_tool_completion = None;
            self.final_output = content.clone();
            self.finish_on_drop = true;
            self.ctx
                .set_state(crate::ContextState::succeeded(Value::String(
                    content.clone(),
                )))?;
            self.emit(
                self.template.agent.name.clone(),
                SessionEventData::Completed { content },
            )
            .await?;
            return Ok(PlanNext::End);
        }
        if let Some(output) = self.pending_todo_output.take() {
            let (todo_plan, next) = SingleAgentTodoPlan::resume_from_created_todos(
                self.ctx.clone(),
                self.template.clone(),
                self.input.clone(),
                self.images.clone(),
                self.turn_id,
                self.session.clone(),
                output,
            )
            .await?;
            self.owns_active_turn = false;
            self.todo_plan = Some(Box::new(todo_plan));
            return Ok(next);
        }
        Ok(PlanNext::Tasks(vec![self.next_model_task().await?]))
    }
}
