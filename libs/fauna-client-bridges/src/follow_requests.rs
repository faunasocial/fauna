//! The row model of a bridge card's *Follow requests* list (`bridges.md`
//! § Follow requests) — which of a [`BridgeFollowRequest`]'s fields a row
//! shows, decided once here so apps paint two strings and forward two clicks
//! (priorities #2/#3).

use fauna_protocol::Value;
use fauna_protocol::bridges_ui::BridgeFollowRequest;

/// The `extra` key a provider carries the requester's typed address under
/// (`@user@host` on the Fediverse) when the nest knows it.
pub const FOLLOW_REQUEST_HANDLE_KEY: &str = "handle";

/// One row of the *Follow requests* list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowRequestRow {
    /// What Approve and Refuse hand back to the nest
    /// (`BridgesClient::approve_follow_request` / `refuse_follow_request`).
    pub id: String,
    /// The requester's address on that network: the provider's `handle` when
    /// it sent one, otherwise the `id` itself.
    pub address: String,
    /// The requester's display name, when the nest knows it.
    pub name: Option<String>,
}

/// Build the row a [`BridgeFollowRequest`] paints as.
pub fn follow_request_row(request: &BridgeFollowRequest) -> FollowRequestRow {
    let handle = match &request.extra {
        Some(Value::Map(map)) => match map.get(FOLLOW_REQUEST_HANDLE_KEY) {
            Some(Value::String(h)) if !h.trim().is_empty() => Some(h.clone()),
            _ => None,
        },
        _ => None,
    };
    FollowRequestRow {
        id: request.id.clone(),
        address: handle.unwrap_or_else(|| request.id.clone()),
        name: request
            .name
            .clone()
            .filter(|n| !n.trim().is_empty() && *n != request.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn request(name: Option<&str>, extra: Option<Value>) -> BridgeFollowRequest {
        BridgeFollowRequest {
            id: "https://remote.example/users/bob".into(),
            name: name.map(Into::into),
            requested_at: Some(1_700_000_000),
            extra,
            unknown_keys: Default::default(),
        }
    }

    fn handle(h: &str) -> Option<Value> {
        Some(Value::Map(BTreeMap::from([(
            FOLLOW_REQUEST_HANDLE_KEY.to_string(),
            Value::String(h.into()),
        )])))
    }

    #[test]
    fn a_row_shows_the_handle_and_keeps_the_id_for_the_answer() {
        let row = follow_request_row(&request(Some("Bob"), handle("@bob@remote.example")));
        assert_eq!(row.id, "https://remote.example/users/bob");
        assert_eq!(row.address, "@bob@remote.example");
        assert_eq!(row.name.as_deref(), Some("Bob"));
    }

    #[test]
    fn a_row_without_a_handle_shows_the_id() {
        for extra in [
            None,
            handle("  "),
            Some(Value::String("not a map".into())),
            Some(Value::Map(BTreeMap::new())),
        ] {
            let row = follow_request_row(&request(None, extra));
            assert_eq!(row.address, "https://remote.example/users/bob");
            assert_eq!(row.name, None);
        }
    }

    #[test]
    fn a_blank_name_is_no_name() {
        let row = follow_request_row(&request(Some(" "), None));
        assert_eq!(row.name, None);
    }
}
