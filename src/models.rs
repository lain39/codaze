use anyhow::Context;
use axum::http::HeaderMap;
use codex_protocol::openai_models::ModelsResponse;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MODELS_CACHE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const OPENAI_MODEL_CREATED: i64 = 0;
const OPENAI_MODEL_OWNED_BY: &str = "openai";

#[derive(Clone, Debug)]
pub(crate) struct ModelsSnapshot {
    codex_json: Value,
    openai_json: Value,
    parallel_tool_calls_by_slug: HashMap<String, bool>,
}

impl ModelsSnapshot {
    pub(crate) fn from_value(value: Value) -> anyhow::Result<Self> {
        let response: ModelsResponse =
            serde_json::from_value(value).context("decode models response as Codex models list")?;
        Self::from_response(response)
    }

    pub(crate) fn from_response(response: ModelsResponse) -> anyhow::Result<Self> {
        let codex_json =
            serde_json::to_value(&response).context("serialize cached Codex models response")?;
        let openai_json = json!({
            "object": "list",
            "data": response.models.iter().map(|model| {
                json!({
                    "id": model.slug,
                    "object": "model",
                    "created": OPENAI_MODEL_CREATED,
                    "owned_by": OPENAI_MODEL_OWNED_BY,
                })
            }).collect::<Vec<_>>(),
        });
        let parallel_tool_calls_by_slug = response
            .models
            .iter()
            .map(|model| (model.slug.clone(), model.supports_parallel_tool_calls))
            .collect();
        Ok(Self {
            codex_json,
            openai_json,
            parallel_tool_calls_by_slug,
        })
    }

    pub(crate) fn codex_json(&self) -> Value {
        self.codex_json.clone()
    }

    pub(crate) fn openai_json(&self) -> Value {
        self.openai_json.clone()
    }

    pub(crate) fn supports_parallel_tool_calls(&self, slug: &str) -> bool {
        self.parallel_tool_calls_by_slug
            .get(slug)
            .copied()
            .unwrap_or(true)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ModelsCache {
    snapshot: Option<Arc<ModelsSnapshot>>,
    response_headers: HeaderMap,
    fetched_at: Option<Instant>,
}

impl ModelsCache {
    pub(crate) fn current(&self) -> Option<Arc<ModelsSnapshot>> {
        self.snapshot.clone()
    }

    pub(crate) fn current_entry(&self) -> Option<(Arc<ModelsSnapshot>, HeaderMap)> {
        Some((self.snapshot.clone()?, self.response_headers.clone()))
    }

    pub(crate) fn fresh_entry(&self) -> Option<(Arc<ModelsSnapshot>, HeaderMap)> {
        let fetched_at = self.fetched_at?;
        if fetched_at.elapsed() <= MODELS_CACHE_TTL {
            return self.current_entry();
        }
        None
    }

    pub(crate) fn replace(&mut self, snapshot: Arc<ModelsSnapshot>, response_headers: HeaderMap) {
        self.snapshot = Some(snapshot);
        self.response_headers = response_headers;
        self.fetched_at = Some(Instant::now());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponseShape {
    Codex,
    OpenAi,
}

impl ResponseShape {
    pub(crate) fn is_codex(self) -> bool {
        matches!(self, Self::Codex)
    }
}

pub(crate) fn response_shape_for_headers(headers: &HeaderMap) -> ResponseShape {
    let Some(originator) = headers
        .get("originator")
        .and_then(|value| value.to_str().ok())
    else {
        return ResponseShape::OpenAi;
    };
    if originator.starts_with("codex") {
        ResponseShape::Codex
    } else {
        ResponseShape::OpenAi
    }
}

pub(crate) fn default_parallel_tool_calls(
    model: Option<&Value>,
    snapshot: Option<&ModelsSnapshot>,
) -> bool {
    let Some(slug) = model.and_then(Value::as_str).map(str::trim) else {
        return true;
    };
    snapshot
        .map(|snapshot| snapshot.supports_parallel_tool_calls(slug))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_originator_uses_codex_shape() {
        let mut headers = HeaderMap::new();
        headers.insert("originator", "codex-tui".parse().unwrap());
        assert_eq!(response_shape_for_headers(&headers), ResponseShape::Codex);
    }

    #[test]
    fn missing_originator_uses_openai_shape() {
        assert_eq!(
            response_shape_for_headers(&HeaderMap::new()),
            ResponseShape::OpenAi
        );
    }

    #[test]
    fn snapshot_maps_openai_models_shape() {
        let snapshot = ModelsSnapshot::from_value(json!({
            "models": [
                {
                    "slug": "gpt-5.4",
                    "display_name": "GPT-5.4",
                    "description": null,
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [],
                    "shell_type": "shell_command",
                    "visibility": "list",
                    "supported_in_api": true,
                    "priority": 1,
                    "availability_nux": null,
                    "upgrade": null,
                    "base_instructions": "",
                    "model_messages": null,
                    "supports_reasoning_summaries": false,
                    "default_reasoning_summary": "auto",
                    "support_verbosity": false,
                    "default_verbosity": null,
                    "apply_patch_tool_type": null,
                    "web_search_tool_type": "text",
                    "truncation_policy": { "mode": "bytes", "limit": 10000 },
                    "supports_parallel_tool_calls": true,
                    "supports_image_detail_original": false,
                    "context_window": 272000,
                    "auto_compact_token_limit": null,
                    "effective_context_window_percent": 95,
                    "experimental_supported_tools": [],
                    "input_modalities": ["text", "image"],
                    "used_fallback_model_metadata": false,
                    "supports_search_tool": false
                }
            ]
        }))
        .expect("snapshot");

        assert_eq!(
            snapshot.openai_json(),
            json!({
                "object": "list",
                "data": [
                    {
                        "id": "gpt-5.4",
                        "object": "model",
                        "created": 0,
                        "owned_by": "openai"
                    }
                ]
            })
        );
    }

    #[test]
    fn default_parallel_tool_calls_reads_snapshot_flags() {
        let snapshot = ModelsSnapshot::from_value(json!({
            "models": [
                {
                    "slug": "gpt-5",
                    "display_name": "GPT-5",
                    "description": null,
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [],
                    "shell_type": "shell_command",
                    "visibility": "list",
                    "supported_in_api": true,
                    "priority": 1,
                    "availability_nux": null,
                    "upgrade": null,
                    "base_instructions": "",
                    "model_messages": null,
                    "supports_reasoning_summaries": false,
                    "default_reasoning_summary": "auto",
                    "support_verbosity": false,
                    "default_verbosity": null,
                    "apply_patch_tool_type": null,
                    "web_search_tool_type": "text",
                    "truncation_policy": { "mode": "bytes", "limit": 10000 },
                    "supports_parallel_tool_calls": false,
                    "supports_image_detail_original": false,
                    "context_window": 272000,
                    "auto_compact_token_limit": null,
                    "effective_context_window_percent": 95,
                    "experimental_supported_tools": [],
                    "input_modalities": ["text", "image"],
                    "used_fallback_model_metadata": false,
                    "supports_search_tool": false
                }
            ]
        }))
        .expect("snapshot");

        assert!(!default_parallel_tool_calls(
            Some(&json!("gpt-5")),
            Some(&snapshot)
        ));
        assert!(default_parallel_tool_calls(
            Some(&json!("future-model")),
            Some(&snapshot)
        ));
    }
}
