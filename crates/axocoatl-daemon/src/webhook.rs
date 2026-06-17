use axocoatl_config::WebhookConfigYaml;
use axocoatl_coordination::EventNotification;
use reqwest::Client;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, error, warn};

/// Starts the background dispatcher for lattice webhooks.
///
/// This loop listens to all `EventNotification`s on the lattice bus. For every
/// event, it checks the configured list of webhooks. If a webhook's filter matches
/// the event type (or is empty for "all events"), it fires an asynchronous POST request
/// containing the event payload to the configured URL.
pub async fn run_webhook_dispatcher(
    mut rx: broadcast::Receiver<EventNotification>,
    configs: Vec<WebhookConfigYaml>,
) {
    if configs.is_empty() {
        return;
    }

    let active_configs: Vec<_> = configs.into_iter().filter(|c| c.enabled).collect();
    if active_configs.is_empty() {
        return;
    }

    // Build a resilient client with a timeout so a hanging webhook server
    // doesn't leak tokio tasks indefinitely.
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|e| {
            warn!("Failed to build webhook HTTP client: {e}");
            Client::new()
        });

    loop {
        let notif = match rx.recv().await {
            Ok(n) => n,
            // If the lattice pushes events faster than we can pull, we'll lag.
            // We must drop the skipped messages and keep going, never dying.
            Err(RecvError::Lagged(skipped)) => {
                warn!("Webhook dispatcher lagged behind, skipping {skipped} events");
                continue;
            }
            // If the sender drops, the daemon is shutting down.
            Err(RecvError::Closed) => break,
        };

        // Extract just the enum variant name, e.g. "TaskCompleted" from "TaskCompleted { task_id: \"foo\" }"
        // This gives users a simple string array to filter by in `axocoatl.yaml`.
        let event_type_str = format!("{:?}", notif.event_type);
        let base_type = event_type_str.split_whitespace().next().unwrap_or(&event_type_str);

        for config in &active_configs {
            let matches = config.events.is_empty() || config.events.iter().any(|e| e == base_type);

            if matches {
                let client = client.clone();
                let url = config.url.clone();
                let payload = notif.payload.clone();
                let name = config.name.clone();
                let event_id = notif.event_id.clone();
                let event_type = notif.event_type.clone();

                // Fire-and-forget egress. We explicitly don't block the dispatcher loop
                // waiting for external network I/O, nor do we implement retries to keep
                // the runtime lean. If strong delivery guarantees are needed, an external
                // message broker should catch these initial emissions.
                tokio::spawn(async move {
                    let body = serde_json::json!({
                        "event_id": event_id,
                        "event_type": event_type,
                        "payload": payload,
                    });
                    
                    match client.post(&url).json(&body).send().await {
                        Ok(resp) => {
                            if resp.status().is_success() {
                                debug!(webhook = %name, status = %resp.status(), "Webhook dispatched successfully");
                            } else {
                                warn!(webhook = %name, status = %resp.status(), "Webhook returned non-success status");
                            }
                        }
                        Err(e) => {
                            error!(webhook = %name, error = %e, "Failed to dispatch webhook");
                        }
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axocoatl_coordination::{EventId, EventType};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_webhook_dispatcher_matches_event() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/webhook"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;

        let config = WebhookConfigYaml {
            name: "test-webhook".to_string(),
            url: format!("{}/webhook", mock_server.uri()),
            events: vec!["TaskCompleted".to_string()],
            enabled: true,
        };

        let (tx, rx) = broadcast::channel(10);
        let _handle = tokio::spawn(run_webhook_dispatcher(rx, vec![config]));

        let notif = EventNotification {
            event_id: EventId::new("123"),
            event_type: EventType::TaskCompleted { task_id: "task-1".to_string() },
            payload: serde_json::json!({ "status": "done" }),
        };

        tx.send(notif).unwrap();

        // Give the spawned task a moment to process and make the HTTP call
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn test_webhook_dispatcher_filters_out_unmatched() {
        let mock_server = MockServer::start().await;

        // Expect 0 calls because the event type does not match
        Mock::given(method("POST"))
            .and(path("/webhook"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&mock_server)
            .await;

        let config = WebhookConfigYaml {
            name: "test-webhook".to_string(),
            url: format!("{}/webhook", mock_server.uri()),
            events: vec!["AgentFailed".to_string()],
            enabled: true,
        };

        let (tx, rx) = broadcast::channel(10);
        let _handle = tokio::spawn(run_webhook_dispatcher(rx, vec![config]));

        // We emit TaskCompleted, but the webhook is only listening for AgentFailed
        let notif = EventNotification {
            event_id: EventId::new("123"),
            event_type: EventType::TaskCompleted { task_id: "task-1".to_string() },
            payload: serde_json::json!({ "status": "done" }),
        };

        tx.send(notif).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}
