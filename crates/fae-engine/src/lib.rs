mod context;
mod engine;
mod engine_rt;
mod tools;

pub use context::*;
pub use engine::*;
pub use engine_rt::*;
pub use tools::*;

impl Engine {
    pub async fn default() -> Self {
        let mut builder = EngineBuilder::new();
        
        let mut tools_runtime = ToolsRuntime::new();
        tools_runtime.add_tool(Box::new(DefaultTools::default()));
        builder.add_runtime(tools_runtime);

        builder.build().await
    }
}

#[cfg(test)]
mod tests {}
