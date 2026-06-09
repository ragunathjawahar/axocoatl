//! LLM-backed resolver for HTN frontier tasks.
//!
//! When the symbolic [`HtnPlanner`](axocoatl_coordination::HtnPlanner) reaches a
//! compound task it has no method for, that task becomes a *frontier*. This
//! resolver decomposes that single task with the model — and only that task, not
//! the whole goal — into primitive subtasks the planner can then schedule. The
//! subtasks are emitted as [`HtnTaskType::Primitive`] so re-planning converges.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axocoatl_coordination::{FrontierResolver, HtnTask, HtnTaskType};
use axocoatl_llm::{ChatRequest, LlmProvider};

/// Resolves HTN frontier tasks by asking the LLM to decompose one task.
pub struct LlmFrontierResolver {
    provider: Arc<dyn LlmProvider>,
}

impl LlmFrontierResolver {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl FrontierResolver for LlmFrontierResolver {
    async fn resolve(
        &self,
        task: &HtnTask,
        _state: &HashMap<String, serde_json::Value>,
    ) -> Result<Vec<HtnTask>, String> {
        let prompt = format!(
            "Decompose this single task into 2-5 concrete, independent primitive subtasks.\n\
             Return ONLY a JSON array; each element is an object with:\n\
             - \"name\": a short snake_case identifier\n\
             - \"description\": what the subtask does\n\
             - \"tools\": array of tool names it needs (use [] if none)\n\
             Do not include any other text.\n\n\
             Task: {}",
            task.name
        );
        let request = ChatRequest::with_system(
            "You decompose one task into primitive subtasks. Return only valid JSON.",
            prompt,
        );

        let response = self
            .provider
            .chat(request)
            .await
            .map_err(|e| format!("frontier resolver LLM call failed: {e}"))?;

        let parsed: Vec<serde_json::Value> = serde_json::from_str(response.content.trim())
            .map_err(|e| format!("frontier resolver returned invalid JSON: {e}"))?;
        if parsed.is_empty() {
            return Err(format!(
                "frontier resolver returned no subtasks for '{}'",
                task.name
            ));
        }

        Ok(parsed
            .into_iter()
            .map(|s| {
                let name = s["name"].as_str().unwrap_or("subtask").to_string();
                let mut parameters = HashMap::new();
                if let Some(desc) = s["description"].as_str() {
                    parameters.insert("description".to_string(), serde_json::json!(desc));
                }
                if let Some(tools) = s.get("tools") {
                    parameters.insert("tools".to_string(), tools.clone());
                }
                // Always primitive so re-planning converges (the resolver does
                // not emit further compound tasks).
                HtnTask {
                    name,
                    parameters,
                    task_type: HtnTaskType::Primitive,
                }
            })
            .collect())
    }
}
