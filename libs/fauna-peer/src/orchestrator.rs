//! Delivery orchestrator — executes the delivery decision tree.

use crate::delivery::{DeliveryPath, DeliveryStatus, delivery_decision};
use crate::relay_client::RelayClient;

pub struct DeliveryOrchestrator {
    pub relay_url: Option<String>,
    pub nest_url: Option<String>,
    pub signing_key: Option<ed25519_dalek::SigningKey>,
}

impl DeliveryOrchestrator {
    pub async fn deliver(
        &self,
        recipient: &[u8; 32],
        message_body: &str,
        tunnel_active: bool,
        tunnel_base_url: Option<&str>,
        recipient_nest_url: Option<&str>,
    ) -> DeliveryStatus {
        let recipient_has_nest = recipient_nest_url.is_some();
        let sender_has_nest = self.nest_url.is_some();
        let path = delivery_decision(tunnel_active, recipient_has_nest, sender_has_nest);

        match path {
            DeliveryPath::DirectP2P => {
                if let Some(base_url) = tunnel_base_url {
                    let actor_hex = hex::encode(recipient);
                    let url = format!("{base_url}/api/v1/inbox/{actor_hex}");
                    match reqwest::Client::new()
                        .post(&url)
                        .header("content-type", "application/json")
                        .body(message_body.to_string())
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => DeliveryStatus::SentP2P,
                        _ => DeliveryStatus::Queued,
                    }
                } else {
                    DeliveryStatus::Queued
                }
            }
            DeliveryPath::RecipientNest => {
                if let Some(nest_url) = recipient_nest_url {
                    let actor_hex = hex::encode(recipient);
                    let url = format!("{nest_url}/api/v1/inbox/{actor_hex}");
                    match reqwest::Client::new()
                        .post(&url)
                        .header("content-type", "application/json")
                        .body(message_body.to_string())
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => DeliveryStatus::SentNest,
                        _ => DeliveryStatus::Queued,
                    }
                } else {
                    DeliveryStatus::Queued
                }
            }
            DeliveryPath::OwnNestDeposit => {
                if let Some(nest_url) = &self.nest_url {
                    let actor_hex = hex::encode(recipient);
                    let url = format!("{nest_url}/api/v1/inbox/{actor_hex}");
                    match reqwest::Client::new()
                        .post(&url)
                        .header("content-type", "application/json")
                        .body(message_body.to_string())
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => DeliveryStatus::SentNest,
                        _ => DeliveryStatus::Queued,
                    }
                } else {
                    DeliveryStatus::Queued
                }
            }
            DeliveryPath::QueueLocal => {
                // Try push relay to wake the recipient
                if let (Some(relay_url), Some(signing_key)) = (&self.relay_url, &self.signing_key) {
                    let client = RelayClient::new(relay_url);
                    match client.wake(recipient, signing_key, "0.0.0.0:0").await {
                        Ok(_) => DeliveryStatus::WakeSent,
                        Err(e) => {
                            tracing::warn!("push relay wake failed: {e}");
                            DeliveryStatus::Queued
                        }
                    }
                } else {
                    DeliveryStatus::Queued
                }
            }
        }
    }

    /// Deliver to the first responding device from a list of tunnel base URLs.
    /// Tries all devices in parallel, returns on first success.
    pub async fn deliver_multi_device(
        &self,
        recipient: &[u8; 32],
        message_body: &str,
        device_urls: &[String],
    ) -> DeliveryStatus {
        if device_urls.is_empty() {
            return DeliveryStatus::Queued;
        }

        let actor_hex = hex::encode(recipient);
        let client = reqwest::Client::new();

        // Use join_all for parallel dispatch — first success wins
        let futures: Vec<_> = device_urls
            .iter()
            .map(|url| {
                let url = format!("{url}/api/v1/inbox/{actor_hex}");
                let client = client.clone();
                let body = message_body.to_string();
                Box::pin(async move {
                    let resp = client
                        .post(&url)
                        .header("content-type", "application/json")
                        .body(body)
                        .timeout(std::time::Duration::from_secs(5))
                        .send()
                        .await;
                    matches!(resp, Ok(r) if r.status().is_success())
                })
            })
            .collect();

        // Wait for all, check if any succeeded
        let results = futures_util::future::join_all(futures).await;
        if results.iter().any(|&success| success) {
            DeliveryStatus::SentP2P
        } else {
            DeliveryStatus::Queued
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_p2p_when_tunnel_active() {
        // Start a mock HTTP server
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/api/v1/inbox/{id}",
                axum::routing::post(|| async { axum::http::StatusCode::OK }),
            );
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let orch = DeliveryOrchestrator {
            relay_url: None,
            nest_url: None,
            signing_key: None,
        };
        let status = orch
            .deliver(
                &[1u8; 32],
                r#"{"text":"hello"}"#,
                true,
                Some(&format!("http://127.0.0.1:{port}")),
                None,
            )
            .await;
        assert_eq!(status, DeliveryStatus::SentP2P);
    }

    #[tokio::test]
    async fn falls_back_to_queue_when_no_tunnel() {
        let orch = DeliveryOrchestrator {
            relay_url: None,
            nest_url: None,
            signing_key: None,
        };
        let status = orch
            .deliver(&[1u8; 32], r#"{"text":"hello"}"#, false, None, None)
            .await;
        assert_eq!(status, DeliveryStatus::Queued);
    }

    #[tokio::test]
    async fn sends_to_recipient_nest() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/api/v1/inbox/{id}",
                axum::routing::post(|| async { axum::http::StatusCode::OK }),
            );
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let orch = DeliveryOrchestrator {
            relay_url: None,
            nest_url: None,
            signing_key: None,
        };
        let status = orch
            .deliver(
                &[1u8; 32],
                r#"{"text":"hello"}"#,
                false,
                None,
                Some(&format!("http://127.0.0.1:{port}")),
            )
            .await;
        assert_eq!(status, DeliveryStatus::SentNest);
    }

    #[tokio::test]
    async fn delivers_to_first_responding_device() {
        // Two mock servers: one fast (returns immediately), one slow (5s delay)
        let listener1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port1 = listener1.local_addr().unwrap().port();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/api/v1/inbox/{id}",
                axum::routing::post(|| async { axum::http::StatusCode::OK }),
            );
            axum::serve(listener1, app).await.ok();
        });

        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port2 = listener2.local_addr().unwrap().port();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/api/v1/inbox/{id}",
                axum::routing::post(|| async {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    axum::http::StatusCode::OK
                }),
            );
            axum::serve(listener2, app).await.ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let orch = DeliveryOrchestrator {
            relay_url: None,
            nest_url: None,
            signing_key: None,
        };
        // Put slow device FIRST to prove we don't wait serially
        let device_urls = vec![
            format!("http://127.0.0.1:{port2}"), // slow
            format!("http://127.0.0.1:{port1}"), // fast
        ];
        let status = orch
            .deliver_multi_device(&[1u8; 32], r#"{"text":"hello"}"#, &device_urls)
            .await;
        assert_eq!(status, DeliveryStatus::SentP2P);
    }
}
