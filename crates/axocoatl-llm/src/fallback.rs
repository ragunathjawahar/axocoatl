//! Rate-limit fallback wrapper.
//!
//! [`FallbackProvider`] wraps a primary provider with an optional backup. When
//! the primary returns [`ProviderError::RateLimited`] — which providers return
//! at request time, before any token has streamed — the request is retried once
//! on the backup, rewritten to use the backup's model. Every other error, and
//! every successful call, passes through the primary untouched. It is opt-in:
//! with no target it is a transparent pass-through.

use std::pin::Pin;
use std::sync::Arc;

use tokio_stream::Stream;

use crate::error::ProviderError;
use crate::provider::{ChatRequest, ChatResponse, LlmProvider, ProviderCapabilities, StreamEvent};

/// A backup provider and the model to use on it.
pub struct FallbackTarget {
    pub provider: Arc<dyn LlmProvider>,
    pub model: String,
}

/// A provider that falls back to a backup on rate-limit. See the module docs.
pub struct FallbackProvider {
    primary: Arc<dyn LlmProvider>,
    fallback: Option<FallbackTarget>,
}

impl FallbackProvider {
    pub fn new(primary: Arc<dyn LlmProvider>, fallback: Option<FallbackTarget>) -> Self {
        Self { primary, fallback }
    }

    /// Point a request at the backup's model. `model_override` is the only
    /// model field on a request and is what every provider reads.
    fn retarget(mut request: ChatRequest, model: &str) -> ChatRequest {
        request.model_override = Some(model.to_string());
        request
    }
}

#[async_trait::async_trait]
impl LlmProvider for FallbackProvider {
    fn provider_id(&self) -> &str {
        self.primary.provider_id()
    }

    fn model_id(&self) -> &str {
        self.primary.model_id()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.primary.capabilities()
    }

    fn count_tokens(&self, request: &ChatRequest) -> usize {
        self.primary.count_tokens(request)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let Some(fb) = &self.fallback else {
            return self.primary.chat(request).await;
        };
        match self.primary.chat(request.clone()).await {
            Err(ProviderError::RateLimited { provider, .. }) => {
                tracing::warn!(
                    primary = %provider,
                    fallback = %fb.provider.provider_id(),
                    model = %fb.model,
                    "primary rate-limited; falling back",
                );
                fb.provider.chat(Self::retarget(request, &fb.model)).await
            }
            other => other,
        }
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>, ProviderError>
    {
        let Some(fb) = &self.fallback else {
            return self.primary.chat_stream(request).await;
        };
        match self.primary.chat_stream(request.clone()).await {
            Err(ProviderError::RateLimited { provider, .. }) => {
                tracing::warn!(
                    primary = %provider,
                    fallback = %fb.provider.provider_id(),
                    model = %fb.model,
                    "primary rate-limited; falling back",
                );
                fb.provider
                    .chat_stream(Self::retarget(request, &fb.model))
                    .await
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::FinishReason;
    use axocoatl_core::TokenUsageStats;
    use std::sync::Mutex;

    enum Behavior {
        Ok(String),
        RateLimited,
        Auth,
    }

    /// A provider that behaves as configured and records the `model_override`
    /// of the last request it received (so tests can assert the model swap).
    struct Mock {
        id: String,
        behavior: Behavior,
        seen_model: Arc<Mutex<Option<String>>>,
    }

    impl Mock {
        fn build(
            id: &str,
            behavior: Behavior,
        ) -> (Arc<dyn LlmProvider>, Arc<Mutex<Option<String>>>) {
            let seen = Arc::new(Mutex::new(None));
            let provider: Arc<dyn LlmProvider> = Arc::new(Self {
                id: id.to_string(),
                behavior,
                seen_model: seen.clone(),
            });
            (provider, seen)
        }

        fn record(&self, request: &ChatRequest) {
            *self.seen_model.lock().unwrap() = request.model_override.clone();
        }

        fn response(&self, content: &str) -> ChatResponse {
            ChatResponse {
                content: content.to_string(),
                tool_calls: vec![],
                finish_reason: FinishReason::Stop,
                usage: TokenUsageStats::new(1, 1),
                model: "mock-model".to_string(),
                provider: self.id.clone(),
            }
        }

        fn error(&self) -> ProviderError {
            match self.behavior {
                Behavior::RateLimited => ProviderError::RateLimited {
                    provider: self.id.clone(),
                    retry_after_secs: None,
                },
                Behavior::Auth => ProviderError::AuthError {
                    provider: self.id.clone(),
                },
                Behavior::Ok(_) => unreachable!(),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for Mock {
        fn provider_id(&self) -> &str {
            &self.id
        }
        fn model_id(&self) -> &str {
            "mock-model"
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
            self.record(&request);
            match &self.behavior {
                Behavior::Ok(content) => Ok(self.response(content)),
                _ => Err(self.error()),
            }
        }
        async fn chat_stream(
            &self,
            request: ChatRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>,
            ProviderError,
        > {
            self.record(&request);
            match &self.behavior {
                Behavior::Ok(content) => {
                    let events = vec![
                        Ok(StreamEvent::TextDelta {
                            delta: content.clone(),
                        }),
                        Ok(StreamEvent::Done {
                            finish_reason: FinishReason::Stop,
                        }),
                    ];
                    Ok(Box::pin(tokio_stream::iter(events)))
                }
                _ => Err(self.error()),
            }
        }
    }

    fn target(provider: Arc<dyn LlmProvider>, model: &str) -> FallbackTarget {
        FallbackTarget {
            provider,
            model: model.to_string(),
        }
    }

    #[tokio::test]
    async fn primary_success_does_not_touch_fallback() {
        let (primary, _) = Mock::build("openai", Behavior::Ok("primary".into()));
        let (backup, backup_seen) = Mock::build("anthropic", Behavior::Ok("backup".into()));
        let fp = FallbackProvider::new(primary, Some(target(backup, "claude-x")));

        let resp = fp.chat(ChatRequest::simple("hi")).await.unwrap();
        assert_eq!(resp.content, "primary");
        assert!(
            backup_seen.lock().unwrap().is_none(),
            "backup must not be called when the primary succeeds"
        );
    }

    #[tokio::test]
    async fn rate_limited_falls_back_with_backup_model() {
        let (primary, _) = Mock::build("openai", Behavior::RateLimited);
        let (backup, backup_seen) = Mock::build("anthropic", Behavior::Ok("backup".into()));
        let fp = FallbackProvider::new(primary, Some(target(backup, "claude-x")));

        let resp = fp.chat(ChatRequest::simple("hi")).await.unwrap();
        assert_eq!(resp.content, "backup");
        assert_eq!(
            backup_seen.lock().unwrap().as_deref(),
            Some("claude-x"),
            "the backup must be called with its own model"
        );
    }

    #[tokio::test]
    async fn rate_limited_without_fallback_propagates() {
        let (primary, _) = Mock::build("openai", Behavior::RateLimited);
        let fp = FallbackProvider::new(primary, None);
        assert!(matches!(
            fp.chat(ChatRequest::simple("hi")).await,
            Err(ProviderError::RateLimited { .. })
        ));
    }

    #[tokio::test]
    async fn non_rate_limit_error_is_not_retried() {
        let (primary, _) = Mock::build("openai", Behavior::Auth);
        let (backup, backup_seen) = Mock::build("anthropic", Behavior::Ok("backup".into()));
        let fp = FallbackProvider::new(primary, Some(target(backup, "claude-x")));

        assert!(matches!(
            fp.chat(ChatRequest::simple("hi")).await,
            Err(ProviderError::AuthError { .. })
        ));
        assert!(
            backup_seen.lock().unwrap().is_none(),
            "a non-rate-limit error must not fall back"
        );
    }

    #[tokio::test]
    async fn streaming_falls_back_on_rate_limit() {
        use tokio_stream::StreamExt;
        let (primary, _) = Mock::build("openai", Behavior::RateLimited);
        let (backup, backup_seen) = Mock::build("anthropic", Behavior::Ok("streamed".into()));
        let fp = FallbackProvider::new(primary, Some(target(backup, "claude-x")));

        let mut stream = fp.chat_stream(ChatRequest::simple("hi")).await.unwrap();
        let mut text = String::new();
        while let Some(ev) = stream.next().await {
            if let Ok(StreamEvent::TextDelta { delta }) = ev {
                text.push_str(&delta);
            }
        }
        assert_eq!(text, "streamed");
        assert_eq!(backup_seen.lock().unwrap().as_deref(), Some("claude-x"));
    }
}
