//! The **shared-folder** arms of [`WriteTokenBearer`] — the byte-plane bearers a
//! cross-nest folder member presents to a set's **home** nest: a writer's
//! write token on direct chunk/manifest POSTs (`federation.md`
//! § Cross-nest shared folders + channel append), and a
//! reader's read token on the GETs that miss the home store
//! (§ *Relay serving across nests*).
//!
//! The caching bearer itself lives in `fauna-client`
//! ([`fauna_client::write_token_bearer`], lifted 2026-09-09 when the
//! conversation rail became its third consumer); this module owns only what is
//! folder-specific: the mint call (`fauna.folders.write_token.get` on the
//! writer's own nest, which relays `fauna.federation.folder.write_token.mint`
//! to the home nest after the foreign-member + `access == 'writer'` gate) and
//! the D4 park-on-refusal classification of its errors.

use std::sync::Arc;

use fauna_nest_http::ApiError;
use fauna_protocol::RpcRequester;
use fauna_protocol::folders::{
    KIND_FOLDERS_READ_TOKEN_GET, KIND_FOLDERS_WRITE_TOKEN_GET, ReadTokenGetReply,
    ReadTokenGetRequest, WriteTokenGetReply, WriteTokenGetRequest,
};

use crate::access_gate::AccessGate;

pub use fauna_client::write_token_bearer::{REFRESH_BUFFER_SECS, WriteTokenBearer};

/// Turn a failed mint into the right [`ApiError`], parking the set first when
/// the home nest refused the **grant** (D4) — the writer grant for a write
/// token, the membership itself for a read token. `kind` names the relay in
/// the error text.
///
/// The distinction is load-bearing, and getting it wrong in either direction is
/// a real defect:
///
/// - A grant refusal returned as [`ApiError::Transport`] reads as a network
///   blip, so the byte plane retries it forever — the *silent* un-sync
///   `file-sync.md`'s iron rule forbids. It becomes a `403` instead: the
///   honest status for "the home nest will not authorize this write", and one
///   the transfer path already treats as a hard failure rather than a retry.
/// - Conversely, parking on a genuine transport fault would strand a
///   perfectly-granted writer on a dropped WebSocket. Only
///   [`fauna_client_sync::is_access_revoked`] flips the gate; everything else
///   keeps its old transport framing verbatim.
fn classify_mint_error(
    kind: &str,
    e: &fauna_client::NestClientError,
    gate: &AccessGate,
) -> ApiError {
    if fauna_client_sync::is_access_revoked(e) {
        if gate.revoke() {
            tracing::warn!(
                error = %e,
                "write-token mint refused: this actor's writer grant on the set is gone; \
                 parking the engine (access revoked). Local files are untouched."
            );
        }
        return ApiError::Status {
            code: 403,
            message: format!("{kind} refused: {e}"),
        };
    }
    ApiError::Transport(format!("{kind}: {e}"))
}

/// A [`WriteTokenBearer`] minting via `fauna.folders.write_token.get` on the
/// writer's OWN nest, which relays the mint to `home_nest_url` (the set's
/// home). Concrete over [`fauna_client::NestClient`] — its request future is
/// `Send` (the generic `RpcRequester` is AFIT and gives no `Send` guarantee,
/// which the boxed `#[async_trait]` `bearer()` requires).
///
/// `gate` is the engine's terminal park flag (D4). The mint is the **first**
/// place a mid-life demotion surfaces — a demoted writer meets it on its next
/// upload byte, before it has anything to record — so a typed refusal here
/// parks the set rather than looking like a byte-plane hiccup.
pub fn folder_write_token_bearer(
    own_nest: Arc<fauna_client::NestClient>,
    home_nest_url: impl Into<String>,
    channel_id_hex: impl Into<String>,
    gate: Arc<AccessGate>,
) -> WriteTokenBearer {
    let home_nest_url = home_nest_url.into();
    let channel_id_hex = channel_id_hex.into();
    WriteTokenBearer::from_minter(move || {
        let own_nest = Arc::clone(&own_nest);
        let nest_url = home_nest_url.clone();
        let channel_id = channel_id_hex.clone();
        let gate = Arc::clone(&gate);
        async move {
            let reply: WriteTokenGetReply = own_nest
                .request(
                    KIND_FOLDERS_WRITE_TOKEN_GET,
                    WriteTokenGetRequest {
                        nest_url,
                        channel_id,
                        extra: Default::default(),
                    },
                )
                .await
                .map_err(|e| classify_mint_error("write_token.get", &e, &gate))?;
            Ok((reply.token, reply.expires_at))
        }
    })
}

/// The read twin of [`folder_write_token_bearer`]: a bearer minting via
/// `fauna.folders.read_token.get` on the member's OWN nest, which relays
/// `fauna.federation.folder.read_token.mint` to `home_nest_url` behind the
/// structural member gate alone. It is a cross-nest READER's byte-plane bearer
/// — a reader holds no write grant, so the write mint refuses it and would
/// park it as a demoted writer. The home nest takes the token at the store-miss
/// arm of its chunk route and re-checks the membership at every request; every
/// bulk write route refuses it.
///
/// `gate` parks the set when the home nest refuses the mint — the membership
/// is gone, the reader's equivalent of a writer's demotion.
pub fn folder_read_token_bearer(
    own_nest: Arc<fauna_client::NestClient>,
    home_nest_url: impl Into<String>,
    channel_id_hex: impl Into<String>,
    gate: Arc<AccessGate>,
) -> WriteTokenBearer {
    let home_nest_url = home_nest_url.into();
    let channel_id_hex = channel_id_hex.into();
    WriteTokenBearer::from_minter(move || {
        let own_nest = Arc::clone(&own_nest);
        let nest_url = home_nest_url.clone();
        let channel_id = channel_id_hex.clone();
        let gate = Arc::clone(&gate);
        async move {
            let reply: ReadTokenGetReply = own_nest
                .request(
                    KIND_FOLDERS_READ_TOKEN_GET,
                    ReadTokenGetRequest {
                        nest_url,
                        channel_id,
                        extra: Default::default(),
                    },
                )
                .await
                .map_err(|e| classify_mint_error("read_token.get", &e, &gate))?;
            Ok((reply.token, reply.expires_at))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejection(code: &str) -> fauna_client::NestClientError {
        fauna_protocol::RpcError::new(code, "error.test").into()
    }

    /// D4: the home nest's `require_foreign_writer` refusal must park the set
    /// AND stop wearing a transport error's clothes.
    ///
    /// Before this, every mint failure became `ApiError::Transport` — so a
    /// revoked writer's byte plane retried the refusal forever while the UI kept
    /// claiming the folder was syncing. That is exactly the silent un-sync the
    /// iron rule forbids.
    #[test]
    fn a_forbidden_mint_parks_the_gate_and_is_not_a_transport_error() {
        let gate = AccessGate::new();
        let err = classify_mint_error(
            "write_token.get",
            &rejection("fauna.federation.forbidden"),
            &gate,
        );
        assert!(gate.is_revoked(), "the grant refusal parks the set");
        assert!(
            matches!(err, ApiError::Status { code: 403, .. }),
            "a grant refusal is a 403, never a retryable transport fault: {err:?}"
        );
    }

    /// The discriminating half. `peer_nest_outdated` rides the *same* relay as a
    /// real refusal, and a transport fault never reached a nest at all — parking
    /// on either would strand a writer whose grant is perfectly valid.
    #[test]
    fn a_version_gap_or_transport_fault_leaves_the_gate_live() {
        for e in [
            rejection("fauna.federation.peer_nest_outdated"),
            rejection("fauna.protocol.internal"),
            fauna_client::NestClientError::RpcTimeout,
        ] {
            let gate = AccessGate::new();
            let err = classify_mint_error("write_token.get", &e, &gate);
            assert!(!gate.is_revoked(), "{e} must not park the engine");
            assert!(
                matches!(err, ApiError::Transport(_)),
                "{e} keeps its retryable transport framing: {err:?}"
            );
        }
    }
}
