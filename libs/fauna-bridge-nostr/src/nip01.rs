use crate::types::{Event, Filter};

/// A message sent from a Nostr client to a relay.
#[derive(Debug, Clone)]
pub enum ClientMessage {
    /// Publish an event.
    Event(Event),
    /// Subscribe with filters.
    Req {
        subscription_id: String,
        filters: Vec<Filter>,
    },
    /// Close a subscription.
    Close(String),
    /// NIP-42 authentication response.
    Auth(Event),
    /// NIP-45: request a count of matching events instead of the events
    /// themselves. Same filter shape as `REQ`, but one-shot — no live
    /// subscription is registered.
    Count {
        subscription_id: String,
        filters: Vec<Filter>,
    },
}

/// A message sent from a relay to a client.
#[derive(Debug, Clone)]
pub enum RelayMessage {
    /// An event matching a subscription.
    Event {
        subscription_id: String,
        event: Event,
    },
    /// Response to an EVENT submission.
    Ok {
        event_id: String,
        accepted: bool,
        message: String,
    },
    /// End of stored events for a subscription.
    Eose(String),
    /// Human-readable notice.
    Notice(String),
    /// NIP-42 authentication challenge.
    Auth(String),
    /// NIP-45: the count of events matching a `COUNT` request.
    Count {
        subscription_id: String,
        count: usize,
        /// `true` when the count hit the relay's internal scan ceiling and
        /// is a lower bound, not exact (NIP-45's optional `"approximate"`
        /// field).
        approximate: bool,
    },
    /// NIP-01 `CLOSED` — the relay refused or ended a subscription
    /// (REQ/COUNT) server-side. `message` carries the same machine-readable
    /// prefixes as `OK` (`invalid:`, `rate-limited:`, `error:`, …).
    Closed {
        subscription_id: String,
        message: String,
    },
}

impl ClientMessage {
    /// Parse a client message from a NIP-01 JSON array string.
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        let arr: Vec<serde_json::Value> =
            serde_json::from_str(s).map_err(|e| anyhow::anyhow!("invalid JSON array: {e}"))?;

        let msg_type = arr
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing message type"))?;

        match msg_type {
            "EVENT" => {
                let event: Event = serde_json::from_value(
                    arr.get(1)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("missing event"))?,
                )?;
                Ok(ClientMessage::Event(event))
            }
            "REQ" => {
                let sub_id = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing subscription ID"))?
                    .to_string();
                let filters: Vec<Filter> = arr[2..]
                    .iter()
                    .map(|v| serde_json::from_value(v.clone()))
                    .collect::<Result<_, _>>()?;
                Ok(ClientMessage::Req {
                    subscription_id: sub_id,
                    filters,
                })
            }
            "CLOSE" => {
                let sub_id = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing subscription ID"))?
                    .to_string();
                Ok(ClientMessage::Close(sub_id))
            }
            "COUNT" => {
                let sub_id = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing subscription ID"))?
                    .to_string();
                let filters: Vec<Filter> = arr[2..]
                    .iter()
                    .map(|v| serde_json::from_value(v.clone()))
                    .collect::<Result<_, _>>()?;
                Ok(ClientMessage::Count {
                    subscription_id: sub_id,
                    filters,
                })
            }
            "AUTH" => {
                let event: Event = serde_json::from_value(
                    arr.get(1)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("missing auth event"))?,
                )?;
                Ok(ClientMessage::Auth(event))
            }
            other => Err(anyhow::anyhow!("unknown message type: {other}")),
        }
    }

    /// Serialize to a NIP-01 JSON array string.
    pub fn to_json(&self) -> String {
        match self {
            ClientMessage::Event(event) => {
                let event_val = serde_json::to_value(event).unwrap();
                serde_json::to_string(&serde_json::json!(["EVENT", event_val])).unwrap()
            }
            ClientMessage::Req {
                subscription_id,
                filters,
            } => {
                let mut arr: Vec<serde_json::Value> =
                    vec![serde_json::json!("REQ"), serde_json::json!(subscription_id)];
                for f in filters {
                    arr.push(serde_json::to_value(f).unwrap());
                }
                serde_json::to_string(&arr).unwrap()
            }
            ClientMessage::Close(sub_id) => {
                serde_json::to_string(&serde_json::json!(["CLOSE", sub_id])).unwrap()
            }
            ClientMessage::Count {
                subscription_id,
                filters,
            } => {
                let mut arr: Vec<serde_json::Value> = vec![
                    serde_json::json!("COUNT"),
                    serde_json::json!(subscription_id),
                ];
                for f in filters {
                    arr.push(serde_json::to_value(f).unwrap());
                }
                serde_json::to_string(&arr).unwrap()
            }
            ClientMessage::Auth(event) => {
                let event_val = serde_json::to_value(event).unwrap();
                serde_json::to_string(&serde_json::json!(["AUTH", event_val])).unwrap()
            }
        }
    }
}

impl RelayMessage {
    /// Serialize to a NIP-01 JSON array string.
    pub fn to_json(&self) -> String {
        match self {
            RelayMessage::Event {
                subscription_id,
                event,
            } => {
                let event_val = serde_json::to_value(event).unwrap();
                serde_json::to_string(&serde_json::json!(["EVENT", subscription_id, event_val]))
                    .unwrap()
            }
            RelayMessage::Ok {
                event_id,
                accepted,
                message,
            } => serde_json::to_string(&serde_json::json!(["OK", event_id, accepted, message]))
                .unwrap(),
            RelayMessage::Eose(sub_id) => {
                serde_json::to_string(&serde_json::json!(["EOSE", sub_id])).unwrap()
            }
            RelayMessage::Notice(msg) => {
                serde_json::to_string(&serde_json::json!(["NOTICE", msg])).unwrap()
            }
            RelayMessage::Auth(challenge) => {
                serde_json::to_string(&serde_json::json!(["AUTH", challenge])).unwrap()
            }
            RelayMessage::Count {
                subscription_id,
                count,
                approximate,
            } => {
                let mut payload = serde_json::json!({ "count": count });
                if *approximate {
                    payload["approximate"] = serde_json::json!(true);
                }
                serde_json::to_string(&serde_json::json!(["COUNT", subscription_id, payload]))
                    .unwrap()
            }
            RelayMessage::Closed {
                subscription_id,
                message,
            } => serde_json::to_string(&serde_json::json!(["CLOSED", subscription_id, message]))
                .unwrap(),
        }
    }

    /// Parse a relay message from a NIP-01 JSON array string.
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        let arr: Vec<serde_json::Value> =
            serde_json::from_str(s).map_err(|e| anyhow::anyhow!("invalid JSON array: {e}"))?;

        let msg_type = arr
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing message type"))?;

        match msg_type {
            "EVENT" => {
                let sub_id = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing subscription ID"))?
                    .to_string();
                let event: Event = serde_json::from_value(
                    arr.get(2)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("missing event"))?,
                )?;
                Ok(RelayMessage::Event {
                    subscription_id: sub_id,
                    event,
                })
            }
            "OK" => {
                let event_id = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing event ID"))?
                    .to_string();
                let accepted = arr
                    .get(2)
                    .and_then(|v| v.as_bool())
                    .ok_or_else(|| anyhow::anyhow!("missing accepted bool"))?;
                let message = arr
                    .get(3)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(RelayMessage::Ok {
                    event_id,
                    accepted,
                    message,
                })
            }
            "EOSE" => {
                let sub_id = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing subscription ID"))?
                    .to_string();
                Ok(RelayMessage::Eose(sub_id))
            }
            "NOTICE" => {
                let msg = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing notice message"))?
                    .to_string();
                Ok(RelayMessage::Notice(msg))
            }
            "AUTH" => {
                let challenge = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing auth challenge"))?
                    .to_string();
                Ok(RelayMessage::Auth(challenge))
            }
            "COUNT" => {
                let sub_id = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing subscription ID"))?
                    .to_string();
                let payload = arr
                    .get(2)
                    .ok_or_else(|| anyhow::anyhow!("missing count payload"))?;
                let count = payload
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| anyhow::anyhow!("missing count field"))?
                    as usize;
                let approximate = payload
                    .get("approximate")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Ok(RelayMessage::Count {
                    subscription_id: sub_id,
                    count,
                    approximate,
                })
            }
            "CLOSED" => {
                let sub_id = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing subscription ID"))?
                    .to_string();
                let message = arr
                    .get(2)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(RelayMessage::Closed {
                    subscription_id: sub_id,
                    message,
                })
            }
            other => Err(anyhow::anyhow!("unknown relay message type: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    fn sample_event() -> Event {
        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1000,
            kind: 1,
            tags: vec![Tag::new(vec!["p".into(), "c".repeat(64)])],
            content: "hello".into(),
            sig: "d".repeat(128),
        }
    }

    #[test]
    fn parse_event_message() {
        let event = sample_event();
        let json = format!(r#"["EVENT",{}]"#, serde_json::to_string(&event).unwrap());
        let msg = ClientMessage::from_json(&json).unwrap();
        match msg {
            ClientMessage::Event(e) => assert_eq!(e.content, "hello"),
            _ => panic!("expected Event"),
        }
    }

    #[test]
    fn parse_req_message() {
        let json = r#"["REQ","sub1",{"kinds":[1],"limit":10}]"#;
        let msg = ClientMessage::from_json(json).unwrap();
        match msg {
            ClientMessage::Req {
                subscription_id,
                filters,
            } => {
                assert_eq!(subscription_id, "sub1");
                assert_eq!(filters.len(), 1);
                assert_eq!(filters[0].kinds, Some(vec![1]));
                assert_eq!(filters[0].limit, Some(10));
            }
            _ => panic!("expected Req"),
        }
    }

    #[test]
    fn parse_req_with_nip50_search() {
        // A real NIP-50 client's string-valued `search` key must parse as the
        // named field — before the field existed it landed in the
        // `Vec<String>` tag flatten and errored the whole REQ.
        let json = r#"["REQ","sub1",{"kinds":[1],"search":"picnic photos"}]"#;
        let msg = ClientMessage::from_json(json).unwrap();
        match msg {
            ClientMessage::Req { filters, .. } => {
                assert_eq!(filters[0].search.as_deref(), Some("picnic photos"));
            }
            _ => panic!("expected Req"),
        }
    }

    #[test]
    fn parse_close_message() {
        let json = r#"["CLOSE","sub1"]"#;
        let msg = ClientMessage::from_json(json).unwrap();
        match msg {
            ClientMessage::Close(sub_id) => assert_eq!(sub_id, "sub1"),
            _ => panic!("expected Close"),
        }
    }

    #[test]
    fn parse_auth_message() {
        let event = sample_event();
        let json = format!(r#"["AUTH",{}]"#, serde_json::to_string(&event).unwrap());
        let msg = ClientMessage::from_json(&json).unwrap();
        match msg {
            ClientMessage::Auth(e) => assert_eq!(e.kind, 1),
            _ => panic!("expected Auth"),
        }
    }

    #[test]
    fn relay_ok_roundtrip() {
        let msg = RelayMessage::Ok {
            event_id: "abc".into(),
            accepted: true,
            message: "".into(),
        };
        let json = msg.to_json();
        let parsed = RelayMessage::from_json(&json).unwrap();
        match parsed {
            RelayMessage::Ok {
                event_id,
                accepted,
                message,
            } => {
                assert_eq!(event_id, "abc");
                assert!(accepted);
                assert_eq!(message, "");
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn relay_eose_roundtrip() {
        let msg = RelayMessage::Eose("sub1".into());
        let json = msg.to_json();
        let parsed = RelayMessage::from_json(&json).unwrap();
        match parsed {
            RelayMessage::Eose(sub_id) => assert_eq!(sub_id, "sub1"),
            _ => panic!("expected Eose"),
        }
    }

    #[test]
    fn parse_count_message() {
        let json = r#"["COUNT","sub1",{"kinds":[1]}]"#;
        let msg = ClientMessage::from_json(json).unwrap();
        match msg {
            ClientMessage::Count {
                subscription_id,
                filters,
            } => {
                assert_eq!(subscription_id, "sub1");
                assert_eq!(filters.len(), 1);
                assert_eq!(filters[0].kinds, Some(vec![1]));
            }
            _ => panic!("expected Count"),
        }
    }

    #[test]
    fn count_client_message_roundtrip() {
        let msg = ClientMessage::Count {
            subscription_id: "sub1".into(),
            filters: vec![Filter {
                kinds: Some(vec![1, 7]),
                ..Default::default()
            }],
        };
        let json = msg.to_json();
        let parsed = ClientMessage::from_json(&json).unwrap();
        match parsed {
            ClientMessage::Count {
                subscription_id,
                filters,
            } => {
                assert_eq!(subscription_id, "sub1");
                assert_eq!(filters[0].kinds, Some(vec![1, 7]));
            }
            _ => panic!("expected Count"),
        }
    }

    #[test]
    fn relay_count_roundtrip_exact() {
        let msg = RelayMessage::Count {
            subscription_id: "sub1".into(),
            count: 42,
            approximate: false,
        };
        let json = msg.to_json();
        assert!(!json.contains("approximate"), "exact count omits the flag");
        let parsed = RelayMessage::from_json(&json).unwrap();
        match parsed {
            RelayMessage::Count {
                subscription_id,
                count,
                approximate,
            } => {
                assert_eq!(subscription_id, "sub1");
                assert_eq!(count, 42);
                assert!(!approximate);
            }
            _ => panic!("expected Count"),
        }
    }

    #[test]
    fn relay_count_roundtrip_approximate() {
        let msg = RelayMessage::Count {
            subscription_id: "sub1".into(),
            count: 50_000,
            approximate: true,
        };
        let json = msg.to_json();
        let parsed = RelayMessage::from_json(&json).unwrap();
        match parsed {
            RelayMessage::Count {
                count, approximate, ..
            } => {
                assert_eq!(count, 50_000);
                assert!(approximate);
            }
            _ => panic!("expected Count"),
        }
    }

    #[test]
    fn relay_closed_roundtrip() {
        let msg = RelayMessage::Closed {
            subscription_id: "sub1".into(),
            message: "invalid: search query exceeds relay caps".into(),
        };
        let json = msg.to_json();
        let parsed = RelayMessage::from_json(&json).unwrap();
        match parsed {
            RelayMessage::Closed {
                subscription_id,
                message,
            } => {
                assert_eq!(subscription_id, "sub1");
                assert!(message.starts_with("invalid:"));
            }
            _ => panic!("expected Closed"),
        }
    }

    #[test]
    fn relay_notice_roundtrip() {
        let msg = RelayMessage::Notice("rate limited".into());
        let json = msg.to_json();
        let parsed = RelayMessage::from_json(&json).unwrap();
        match parsed {
            RelayMessage::Notice(m) => assert_eq!(m, "rate limited"),
            _ => panic!("expected Notice"),
        }
    }

    #[test]
    fn relay_auth_roundtrip() {
        let msg = RelayMessage::Auth("challenge123".into());
        let json = msg.to_json();
        let parsed = RelayMessage::from_json(&json).unwrap();
        match parsed {
            RelayMessage::Auth(c) => assert_eq!(c, "challenge123"),
            _ => panic!("expected Auth"),
        }
    }

    #[test]
    fn relay_event_roundtrip() {
        let event = sample_event();
        let msg = RelayMessage::Event {
            subscription_id: "sub1".into(),
            event: event.clone(),
        };
        let json = msg.to_json();
        let parsed = RelayMessage::from_json(&json).unwrap();
        match parsed {
            RelayMessage::Event {
                subscription_id,
                event: e,
            } => {
                assert_eq!(subscription_id, "sub1");
                assert_eq!(e.content, "hello");
            }
            _ => panic!("expected Event"),
        }
    }

    #[test]
    fn unknown_message_type_errors() {
        let json = r#"["UNKNOWN","data"]"#;
        assert!(ClientMessage::from_json(json).is_err());
        assert!(RelayMessage::from_json(json).is_err());
    }
}
