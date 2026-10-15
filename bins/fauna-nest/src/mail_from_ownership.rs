//! The `From:` header ownership predicate — the one rule every door that hands
//! the nest a user's message enforces (`mail-multidomain.md` § From: header
//! ownership).
//!
//! A `From:` whose domain the deployment signs for (an active local domain, or
//! a subdomain of one — the signer keys `d=` under the closest local parent)
//! must name an address the submitting actor owns: the actor's handle on an
//! active local domain, or an alias the alias resolver attributes to that
//! actor. Nothing on a subdomain can be owned (no mailbox exists there), so a
//! subdomain `From:` is never the actor's, not even for the handle. A `From:`
//! on a domain the deployment does not sign for is out of scope.
//!
//! One function, called by the app door (`fauna.email.send`) and by the
//! bridges' enqueue door (`fauna.bridges.enqueue_outbound_mail`, the MTA's
//! submissions and the MDA's iMIP fan-out). The submission MTA applies the
//! same predicate bridge-side (`assertSenderOwned`) as the earlier, cheaper
//! refusal; this one is the nest's, because the nest is the party whose DKIM
//! signature vouches for the header.

use std::sync::Arc;

use fauna_protocol::RpcError;

use crate::bridge_routing_handlers::{
    LocalRecipientOutcome, ResolvePurpose, primary_mail_domain, resolve_local_recipient_unstamped,
};
use crate::routes::AppState;

/// The predicate's answer for one `From:` mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromOwnership {
    /// The deployment does not sign for the address's domain: not ours to
    /// vouch for, outside the rule.
    NotSignedFor,
    /// A signed-for address the actor owns.
    Owned,
    /// A signed-for address the actor does not own. `handle` is the actor's
    /// handle (`None` when it has none), for a door whose refusal names it.
    NotOwned { handle: Option<String> },
}

/// Whether `actor` owns the `From:` mailbox `local@host`, under the rule this
/// module states. `host` is compared after the shared DNS-name normaliser, so
/// a caller may pass it as written.
pub async fn from_ownership(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    local: &str,
    host: &str,
) -> Result<FromOwnership, RpcError> {
    let host = fauna_core::web::normalize_dns_name(host);
    let active: Vec<String> = state
        .db
        .list_active_mail_domains()
        .await
        .map_err(crate::rpc_errors::internal)?
        .into_iter()
        .map(|d| fauna_core::web::normalize_dns_name(&d.domain_name))
        .collect();
    let locals: Vec<&str> = active.iter().map(String::as_str).collect();
    if fauna_mail::outbound::dkim::select_signing_domain(&host, &locals).is_err() {
        return Ok(FromOwnership::NotSignedFor);
    }

    // `get_handle` returns `Ok(None)` only when there is no `users` row at
    // all; a REGISTERED actor without a handle stores the empty string
    // (`create_user` writes `''`, and `fauna.admin.users.clear_handle` sets it
    // back to `''`), so an empty handle is the no-handle arm too.
    let handle = state
        .db
        .get_handle(actor)
        .await
        .map_err(|e| crate::rpc_errors::internal(format!("get_handle: {e}")))?
        .filter(|h| !h.is_empty());

    // A subdomain is signed for but holds no mailbox: nobody owns it.
    if !locals.contains(&host.as_str()) {
        return Ok(FromOwnership::NotOwned { handle });
    }
    // The handle is the actor's own address on every active local domain
    // (`mail-multidomain.md` § Multi-domain handles), compared
    // case-insensitively with no resolver call.
    if handle
        .as_deref()
        .is_some_and(|h| h.eq_ignore_ascii_case(local))
    {
        return Ok(FromOwnership::Owned);
    }
    // Not the handle: an alias the actor owns is theirs to send from too. The
    // SAME in-process resolver the MTA reaches over
    // `fauna.bridges.resolve_recipient`, and the same reading of its answer:
    // only a deliverable mailbox owned by THIS actor authorises; a Forward (an
    // admin external forwarder — no sending mailbox), a Reject, a processor
    // address, or another owner are all "not yours".
    let primary = primary_mail_domain(state).await?;
    let owned = matches!(
        resolve_local_recipient_unstamped(
            state,
            &host,
            local,
            primary.as_deref().unwrap_or(&host),
            ResolvePurpose::Probe,
        )
        .await?,
        LocalRecipientOutcome::Mailbox { actor_id: owner, .. } if owner == *actor
    );
    Ok(if owned {
        FromOwnership::Owned
    } else {
        FromOwnership::NotOwned { handle }
    })
}
