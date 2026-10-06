//! BridgeProvider implementation for Nostr.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use fauna_bridge_nostr::nip19;
use fauna_bridge_nostr::nip42::{self, AuthEventTemplate};
use fauna_bridge_nostr::nip46::{BunkerUrl, NostrConnectClient};
use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::Event;
use fauna_protocol::Value;

use crate::bridge_management::*;
use crate::nostr::{db, key_crypto, store};
use crate::routes::AppState;

pub struct NostrProvider;

// ── Proof of possession (`nip07` / `remote`) ───────────────────────
//
// `docs/goal/ui/nostr.md` § Errors & edge cases → *Proof of possession*. A
// pubkey held OUTSIDE the nest is linked only once its holder has signed this
// nest's challenge: a NIP-42 kind-22242 AUTH event over a nonce, bound to this
// nest's relay URL (`nip42::auth_event_template`). `nip07` has the browser
// extension sign it (two calls: `link_challenge`, then `link` carrying the
// signed event); `remote` has the nest complete the NIP-46 handshake and ask
// the bunker to sign it, inside the one `link` call. Either way the row is
// written with the pubkey the SIGNATURE proves, never the one the caller typed.

/// How long a minted `nip07` challenge stays signable. Long enough for the
/// extension's approval prompt, short enough that a leaked nonce is worthless.
pub const LINK_CHALLENGE_TTL_SECS: u64 = 300;

/// Every network wait of the `remote` handshake (relay dial, `connect`,
/// `get_public_key`, `sign_event`) is bounded by this individually, so the
/// whole arm stays inside `fauna.bridges.link`'s 30 s deadline.
const REMOTE_STEP_TIMEOUT: Duration = Duration::from_secs(7);

/// One outstanding `nip07` challenge (`NostrState::link_challenges`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkChallenge {
    /// The nonce, 32 hex chars.
    pub challenge: String,
    /// The relay URL the template's `relay` tag names — this nest's own.
    pub relay_url: String,
    /// The template's `created_at`.
    pub created_at: u64,
    /// Unix seconds; `link` refuses the challenge from here on.
    pub expires_at: u64,
}

impl LinkChallenge {
    /// The unsigned event this challenge asks the signer for.
    pub fn template(&self) -> AuthEventTemplate {
        nip42::auth_event_template(&self.relay_url, &self.challenge, self.created_at)
    }
}

/// This nest's own relay URL — what a proof is bound to. NIP-42 semantics: a
/// proof minted for this relay is dead at every other.
fn own_relay_url(state: &AppState) -> String {
    format!("wss://{}/nostr", state.handle_domain())
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// Mint a fresh challenge for `actor_id`, superseding any outstanding one, and
/// sweep expired entries while the lock is held.
fn mint_challenge(state: &AppState, actor_id: &str, now: u64) -> LinkChallenge {
    let challenge = LinkChallenge {
        challenge: fauna_core::identity::random_hex(16),
        relay_url: own_relay_url(state),
        created_at: now,
        expires_at: now + LINK_CHALLENGE_TTL_SECS,
    };
    let mut pending = state
        .nostr
        .link_challenges
        .lock()
        .expect("link_challenges lock");
    pending.retain(|_, c| c.expires_at > now);
    pending.insert(actor_id.to_string(), challenge.clone());
    challenge
}

/// Take `actor_id`'s outstanding challenge — single use: it leaves the map
/// whether or not the proof then verifies. `None` when there is none or it
/// has expired.
fn take_challenge(state: &AppState, actor_id: &str, now: u64) -> Option<LinkChallenge> {
    let mut pending = state
        .nostr
        .link_challenges
        .lock()
        .expect("link_challenges lock");
    pending.remove(actor_id).filter(|c| c.expires_at > now)
}

/// The template as the `LinkChallengeReply::payload` map — the four NIP-01
/// fields `window.nostr.signEvent` takes, under their NIP-01 names.
fn template_payload(template: &AuthEventTemplate) -> Value {
    let tags = template
        .tags
        .iter()
        .map(|t| Value::List(t.0.iter().cloned().map(Value::String).collect()))
        .collect();
    Value::Map(BTreeMap::from([
        ("kind".to_string(), Value::Integer(template.kind as i128)),
        (
            "created_at".to_string(),
            Value::Integer(template.created_at as i128),
        ),
        ("tags".to_string(), Value::List(tags)),
        (
            "content".to_string(),
            Value::String(template.content.clone()),
        ),
    ]))
}

/// The unsigned event as NIP-01 JSON for a NIP-46 `sign_event` — the template
/// plus the user pubkey the bunker just reported.
fn template_json_for(template: &AuthEventTemplate, user_pubkey: &str) -> String {
    let mut v = serde_json::to_value(template).expect("template serializes");
    v["pubkey"] = serde_json::Value::String(user_pubkey.to_string());
    v.to_string()
}

/// The `LinkReply` every arm answers once a row is written: the linked
/// pubkey's npub as the identity.
fn linked_reply(pubkey_hex: &str) -> Result<LinkReply, BridgeError> {
    let bytes = hex::decode(pubkey_hex)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| BridgeError::invalid_params("pubkey must be 64-char hex"))?;
    let npub = nip19::encode_npub(&bytes);
    Ok(LinkReply {
        extra: Default::default(),
        linked: true,
        identity: Some(BridgeIdentity {
            extra: Default::default(),
            label: "Public Key".into(),
            value: npub.clone(),
            display: format!("{}...{}", &npub[..12], &npub[npub.len() - 4..]),
        }),
        redirect_url: None,
    })
}

/// Seal a deposited nsec under the deployment seed `nest_keypair` holds, read on
/// `conn` — the connection the deposit is then written on, under the same hold
/// of the database guard. Never a serving generation's copy
/// (`crate::nest_kek`'s module docs): a generation not yet torn down after a
/// deployment-seed rotation still holds the retired seed, and a deposit sealed
/// under it would never open again.
fn seal_deposit(conn: &rusqlite::Connection, secret: &[u8; 32]) -> Result<Vec<u8>, BridgeError> {
    crate::nest_kek::require_deployment_seed(conn)
        .and_then(|seed| key_crypto::encrypt_nostr_privkey(&seed, secret))
        .map_err(|e| BridgeError::provider_error(&format!("encryption error: {e}")))
}

/// Every link arm's write refusal. A pubkey another account holds is
/// `identity_in_use` — the writer's one-pubkey-one-actor rule keeps a link,
/// proven or not, from reaching another user's account.
fn link_refusal(e: db::LinkAccountError) -> BridgeError {
    match e {
        db::LinkAccountError::PubkeyHeld => BridgeError::identity_in_use(),
        db::LinkAccountError::ActorLinked => BridgeError::already_linked(),
        db::LinkAccountError::Other(e) => BridgeError::provider_error(&format!("{e:#}")),
    }
}

// ── Per-mode link params + settings (typed) ────────────────────────

#[derive(Deserialize)]
struct ImportLinkParams {
    nsec: String,
}

#[derive(Deserialize)]
struct RemoteLinkParams {
    bunker_url: String,
}

#[derive(Deserialize)]
struct Nip07LinkParams {
    /// The pubkey the extension reported (`window.nostr.getPublicKey`).
    pubkey: String,
    /// The challenge from `fauna.bridges.link_challenge`, signed by the
    /// extension, as NIP-01 wire JSON — the same carriage as
    /// `nostr.events.publish_signed`'s `event_json`: an external-protocol
    /// object the nest re-parses and signature-verifies. Absent (a caller that skipped
    /// the challenge) → `proof_required`, never a silent link.
    #[serde(default)]
    proof_json: Option<String>,
}

#[derive(Deserialize, Default)]
struct NostrSettingsWire {
    #[serde(default)]
    relay_list: Option<String>,
    #[serde(default)]
    expose_content: Option<bool>,
    #[serde(default)]
    auto_publish: Option<bool>,
    #[serde(default)]
    publish_replies: Option<bool>,
    #[serde(default)]
    publish_reactions: Option<bool>,
    #[serde(default)]
    inbound_to_feed: Option<bool>,
}

#[derive(Deserialize, Default)]
struct FollowExtraWire {
    #[serde(default)]
    relay_hints: Option<Value>,
}

/// The store-time half of the relay dial guard (`nest/network-exposure.md`
/// § Rulings F7): refuse, before anything is stored or dialed, a user-supplied
/// relay URL whose text alone shows the dial could never reach it — the shared
/// [`fauna_protocol::nostr_relay::relay_url_refusal`] every app already runs,
/// here under this nest's own dial policy, so the e2e/in-process loopback
/// affordance (F7(c)) admits here exactly what the dial admits. `what` names
/// the source in the `invalid_params` message.
fn refuse_undialable_relays<'a>(
    urls: impl IntoIterator<Item = &'a str>,
    state: &AppState,
    what: &str,
) -> Result<(), BridgeError> {
    use fauna_protocol::nostr_relay::{RelayUrlRefusal, relay_url_refusal};
    let policy = state.nostr.relay_dial_policy;
    for url in urls {
        let rule = match relay_url_refusal(url, |ip| policy.permits(ip)) {
            None => continue,
            Some(RelayUrlRefusal::Malformed) => "must be a ws:// or wss:// URL with a host",
            Some(RelayUrlRefusal::PrivateAddress) => {
                "names a private-network, loopback, link-local or cloud-metadata address, which \
                 no relay dial may reach"
            }
        };
        return Err(BridgeError::invalid_params(&format!(
            "{what} {url:?} {rule}"
        )));
    }
    Ok(())
}

#[async_trait]
impl BridgeProvider for NostrProvider {
    fn id(&self) -> &str {
        "nostr"
    }
    fn name(&self) -> &str {
        "Nostr"
    }

    async fn available(&self, state: &AppState) -> bool {
        crate::nostr::nostr_bridging_available(state).await
    }

    async fn available_for_link(&self, state: &AppState, mode: &str) -> bool {
        // "generate"/"import" are the ONLY production writers of
        // `encrypted_privkey` (the deposit `available()` checks for) — they
        // must be reachable precisely when no deposit exists yet, or the
        // box's first deposit is impossible. "remote"/"nip07" deposit
        // nothing and stay gated on the real availability check, exactly as
        // every other bridging act does.
        if mode == "generate" || mode == "import" {
            return true;
        }
        self.available(state).await
    }

    fn supports_follows(&self) -> bool {
        true
    }

    fn link_modes(&self) -> Vec<BridgeLinkMode> {
        vec![
            BridgeLinkMode {
                extra: Default::default(),
                mode: "generate".into(),
                label: "Generate new keypair".into(),
                client_action: None,
                platform: None,
                fields: vec![],
            },
            BridgeLinkMode {
                extra: Default::default(),
                mode: "import".into(),
                label: "Import existing nsec".into(),
                client_action: None,
                platform: None,
                fields: vec![BridgeLinkField {
                    extra: Default::default(),
                    key: "nsec".into(),
                    label: "Private key (nsec)".into(),
                    field_type: "secret".into(),
                    placeholder: Some("nsec1...".into()),
                }],
            },
            BridgeLinkMode {
                extra: Default::default(),
                mode: "remote".into(),
                label: "NIP-46 remote signer".into(),
                client_action: None,
                platform: None,
                fields: vec![BridgeLinkField {
                    extra: Default::default(),
                    key: "bunker_url".into(),
                    label: "Bunker URL".into(),
                    field_type: "text".into(),
                    placeholder: Some("bunker://pubkey?relay=wss://...".into()),
                }],
            },
            BridgeLinkMode {
                extra: Default::default(),
                mode: "nip07".into(),
                label: "Sign with browser extension (NIP-07)".into(),
                client_action: Some("nip07".into()),
                platform: Some("web".into()),
                fields: vec![],
            },
        ]
    }

    async fn status(&self, state: &AppState, actor_id: &str) -> Result<BridgeStatus, BridgeError> {
        let conn = state.db.conn().await;
        let acct = db::get_account(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        drop(conn);

        match acct {
            Some(a) => {
                let npub = hex::decode(&a.nostr_pubkey)
                    .ok()
                    .and_then(|b| {
                        if b.len() == 32 {
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&b);
                            Some(arr)
                        } else {
                            None
                        }
                    })
                    .map(|b| nip19::encode_npub(&b));

                Ok(BridgeStatus {
                    linked: true,
                    identity: Some(BridgeIdentity {
                        extra: Default::default(),
                        label: "Public Key".into(),
                        value: npub.clone().unwrap_or_else(|| a.nostr_pubkey.clone()),
                        display: npub
                            .as_ref()
                            .map(|n| format!("{}...{}", &n[..12], &n[n.len() - 4..]))
                            .unwrap_or_else(|| format!("{}...", &a.nostr_pubkey[..12])),
                    }),
                    mode: Some(a.signing_mode),
                    settings: vec![
                        BridgeSetting {
                            extra: Default::default(),
                            key: "expose_content".into(),
                            label: "Expose content on relay".into(),
                            setting_type: "bool".into(),
                            value: Value::Bool(a.expose_content),
                            options: None,
                        },
                        BridgeSetting {
                            extra: Default::default(),
                            key: "auto_publish".into(),
                            label: "Auto-publish to relays".into(),
                            setting_type: "bool".into(),
                            value: Value::Bool(a.auto_publish),
                            options: None,
                        },
                        BridgeSetting {
                            extra: Default::default(),
                            key: "publish_replies".into(),
                            label: "Include replies".into(),
                            setting_type: "bool".into(),
                            value: Value::Bool(a.publish_replies),
                            options: None,
                        },
                        BridgeSetting {
                            extra: Default::default(),
                            key: "publish_reactions".into(),
                            label: "Include reactions".into(),
                            setting_type: "bool".into(),
                            value: Value::Bool(a.publish_reactions),
                            options: None,
                        },
                        BridgeSetting {
                            extra: Default::default(),
                            key: "inbound_to_feed".into(),
                            label: "Show inbound in feed".into(),
                            setting_type: "bool".into(),
                            value: Value::Bool(a.inbound_to_feed),
                            options: None,
                        },
                        BridgeSetting {
                            extra: Default::default(),
                            key: "relay_list".into(),
                            label: "Relay list".into(),
                            setting_type: "text".into(),
                            value: Value::String(a.relay_list.unwrap_or_default()),
                            options: None,
                        },
                    ],
                    link_modes: None,
                })
            }
            None => Ok(BridgeStatus {
                linked: false,
                identity: None,
                mode: None,
                settings: vec![],
                link_modes: Some(self.link_modes()),
            }),
        }
    }

    async fn link(
        &self,
        state: &AppState,
        actor_id: &str,
        mode: &str,
        params: Value,
    ) -> Result<LinkReply, BridgeError> {
        // Check not already linked
        let conn = state.db.conn().await;
        if db::get_account(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?
            .is_some()
        {
            return Err(BridgeError::already_linked());
        }
        drop(conn);

        match mode {
            "generate" => {
                let kp = Keypair::generate();
                let pubkey_hex = kp.public_key_hex();
                let conn = state.db.conn().await;
                let encrypted = seal_deposit(&conn, &kp.secret_bytes())?;
                db::link_account(
                    &conn,
                    actor_id,
                    &pubkey_hex,
                    "generated",
                    Some(&encrypted),
                    None,
                    None,
                )
                .map_err(link_refusal)?;
                linked_reply(&pubkey_hex)
            }
            "import" => {
                let p: ImportLinkParams = deserialize_value(&params).map_err(|e| {
                    BridgeError::invalid_params(&format!("nsec required for import mode: {e}"))
                })?;
                let secret_bytes = nip19::decode_nsec(&p.nsec)
                    .map_err(|e| BridgeError::invalid_params(&format!("invalid nsec: {e}")))?;
                let kp = Keypair::from_secret_bytes(secret_bytes)
                    .map_err(|e| BridgeError::invalid_params(&format!("invalid key: {e}")))?;
                let pubkey_hex = kp.public_key_hex();
                let conn = state.db.conn().await;
                let encrypted = seal_deposit(&conn, &secret_bytes)?;
                db::link_account(
                    &conn,
                    actor_id,
                    &pubkey_hex,
                    "imported",
                    Some(&encrypted),
                    None,
                    None,
                )
                .map_err(link_refusal)?;
                linked_reply(&pubkey_hex)
            }
            "remote" => {
                let p: RemoteLinkParams = deserialize_value(&params).map_err(|e| {
                    BridgeError::invalid_params(&format!(
                        "bunker_url required for remote mode: {e}"
                    ))
                })?;
                let bunker = BunkerUrl::parse(&p.bunker_url)
                    .map_err(|e| BridgeError::invalid_params(&format!("bunker URL: {e:#}")))?;
                refuse_undialable_relays(
                    bunker.relays.iter().map(String::as_str),
                    state,
                    "bunker relay",
                )?;

                // The handshake IS the proof: the bunker the user pointed us at
                // must accept `connect` (the invite secret), name the user key,
                // and sign this nest's challenge as that key. Reachability
                // failures are the relay's (`provider_error`, the "relay
                // connection failure" of § Errors & edge cases); every refusal
                // past the dial is a missing proof.
                // The `relay=` parameters are the user's paste: dialed under
                // the nest's relay policy like every other relay.
                let mut client = NostrConnectClient::dial(
                    &bunker,
                    REMOTE_STEP_TIMEOUT,
                    state.nostr.relay_dial_policy,
                )
                .await
                .map_err(|e| {
                    BridgeError::provider_error(&format!("bunker relay unreachable: {e:#}"))
                })?;
                let proof_refused = |what: &str, e: anyhow::Error| {
                    BridgeError::proof_required(&format!("{what}: {e:#}"))
                };
                client
                    .connect(REMOTE_STEP_TIMEOUT)
                    .await
                    .map_err(|e| proof_refused("bunker refused the connection", e))?;
                let user_pubkey = client
                    .get_public_key(REMOTE_STEP_TIMEOUT)
                    .await
                    .map_err(|e| proof_refused("bunker did not name a user key", e))?;
                let challenge = LinkChallenge {
                    challenge: fauna_core::identity::random_hex(16),
                    relay_url: own_relay_url(state),
                    created_at: now_secs(),
                    expires_at: now_secs() + LINK_CHALLENGE_TTL_SECS,
                };
                let unsigned = template_json_for(&challenge.template(), &user_pubkey);
                let proof = client
                    .sign_event(&unsigned, REMOTE_STEP_TIMEOUT)
                    .await
                    .map_err(|e| proof_refused("bunker would not sign the challenge", e))?;
                nip42::verify_link_proof(
                    &proof,
                    &challenge.challenge,
                    &challenge.relay_url,
                    Some(&user_pubkey),
                )
                .map_err(|e| BridgeError::proof_required(&format!("bunker's proof: {e}")))?;

                // At rest without the one-time secret: `connect` consumed it,
                // and the row is not a place to keep a credential anyway.
                let conn = state.db.conn().await;
                db::link_account(
                    &conn,
                    actor_id,
                    &proof.pubkey,
                    "remote",
                    None,
                    Some(&bunker.without_secret()),
                    None,
                )
                .map_err(link_refusal)?;
                linked_reply(&proof.pubkey)
            }
            "nip07" => {
                let p: Nip07LinkParams = deserialize_value(&params).map_err(|e| {
                    BridgeError::invalid_params(&format!("pubkey required for nip07 mode: {e}"))
                })?;
                if p.pubkey.len() != 64 || hex::decode(&p.pubkey).is_err() {
                    return Err(BridgeError::invalid_params("pubkey must be 64-char hex"));
                }
                let Some(proof_json) = p.proof_json else {
                    return Err(BridgeError::proof_required(
                        "linking a browser-extension key needs the signed challenge from \
                         fauna.bridges.link_challenge — the request carried no proof",
                    ));
                };
                let proof: Event = serde_json::from_str(&proof_json).map_err(|e| {
                    BridgeError::invalid_params(&format!("proof_json is not a NIP-01 event: {e}"))
                })?;
                let Some(challenge) = take_challenge(state, actor_id, now_secs()) else {
                    return Err(BridgeError::proof_required(
                        "no live challenge for this account — request a new one and sign it \
                         within five minutes",
                    ));
                };
                nip42::verify_link_proof(
                    &proof,
                    &challenge.challenge,
                    &challenge.relay_url,
                    Some(&p.pubkey),
                )
                .map_err(|e| BridgeError::proof_required(&e.to_string()))?;

                let conn = state.db.conn().await;
                db::link_account(&conn, actor_id, &proof.pubkey, "nip07", None, None, None)
                    .map_err(link_refusal)?;
                linked_reply(&proof.pubkey)
            }
            other => Err(BridgeError::invalid_mode(other)),
        }
    }

    async fn link_challenge(
        &self,
        state: &AppState,
        actor_id: &str,
        mode: &str,
    ) -> Result<LinkChallengeReply, BridgeError> {
        match mode {
            "nip07" => {
                let challenge = mint_challenge(state, actor_id, now_secs());
                Ok(LinkChallengeReply {
                    challenge: challenge.challenge.clone(),
                    expires_at: challenge.expires_at as i64,
                    payload: template_payload(&challenge.template()),
                    extra: Default::default(),
                })
            }
            // The nest itself proves a remote key inside `link` (the NIP-46
            // handshake), and custodial modes hold the key here: nothing for
            // the app to sign.
            "remote" | "generate" | "import" => Err(BridgeError::invalid_params(&format!(
                "{mode} links need no client-signed challenge — call fauna.bridges.link directly"
            ))),
            other => Err(BridgeError::invalid_mode(other)),
        }
    }

    async fn unlink(&self, state: &AppState, actor_id: &str) -> Result<(), BridgeError> {
        let conn = state.db.conn().await;
        db::unlink_account(&conn, actor_id).map_err(|e| BridgeError::provider_error(&e.to_string()))
    }

    async fn update_settings(
        &self,
        state: &AppState,
        actor_id: &str,
        settings: Value,
    ) -> Result<(), BridgeError> {
        let wire: NostrSettingsWire = deserialize_value(&settings)
            .map_err(|e| BridgeError::invalid_params(&format!("settings: {e}")))?;
        // A list that is not a JSON string array is left to the dial-time
        // fallback (`relays::resolve_relay_urls`); its entries are checked.
        if let Some(list) = wire
            .relay_list
            .as_deref()
            .and_then(|j| serde_json::from_str::<Vec<String>>(j).ok())
        {
            refuse_undialable_relays(list.iter().map(String::as_str), state, "relay")?;
        }
        let nostr_settings = db::NostrSettings {
            relay_list: wire.relay_list,
            expose_content: wire.expose_content,
            auto_publish: wire.auto_publish,
            publish_replies: wire.publish_replies,
            publish_reactions: wire.publish_reactions,
            inbound_to_feed: wire.inbound_to_feed,
        };

        // The relay URL to self-advertise (NIP-65 kind-10002 / NIP-17
        // kind-10050) — prefer the actor's `nostr_push` peer's public relay
        // (R10 (account-data-plane.md § The ratified decisions): a private head advertises its public serving box, not its own
        // LAN domain), else this box's own domain. Resolved BEFORE the `conn`
        // guard below: `preferred_public_relay_url` takes the same CacheDb lock,
        // so computing it while holding `conn` would deadlock. Only computed on
        // a relay-list change (the sole trigger for the advertisement below).
        let nest_relay_url = if nostr_settings.relay_list.is_some() {
            crate::nostr::relays::preferred_public_relay_url(state, actor_id)
                .await
                .or_else(|| {
                    // Claim-refreshed identity domain, not the `node.domain`
                    // boot seed (domainless on a provisioned box).
                    state
                        .handle_domain_if_set()
                        .map(|d| format!("wss://{d}/nostr"))
                })
        } else {
            None
        };

        let conn = state.db.conn().await;
        db::update_settings(&conn, actor_id, &nostr_settings)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?;

        // Publish NIP-65 (kind 10002) relay-list metadata AND the NIP-17
        // kind-10050 DM-inbox list when the relay list changed and the account
        // holds a stored private key (generated / imported mode). The nest
        // advertises its own `/nostr` endpoint as the user's write + DM-inbox
        // relay (slice B, inbox role — `nostr.md` § The relay event store): a
        // foreign client reads the kind-10050 off the user's external relays,
        // learns `wss://<domain>/nostr`, and deposits gift-wrap DMs there. The
        // NIP-65 half is ported from the retired `PUT /api/v1/nostr/settings`
        // handler so the WS-RPC path preserves the publish-on-change behavior
        // (the sync worker has no periodic republish).
        if let Some(ref relay_list_json) = nostr_settings.relay_list
            && let Ok(Some(acct)) = db::get_account(&conn, actor_id)
            && let Some(ref encrypted_privkey) = acct.encrypted_privkey
        {
            let nest_key_bytes = state.nest_identity.signing_key.to_bytes();
            if let Ok(secret_bytes) =
                key_crypto::decrypt_nostr_privkey(&nest_key_bytes, encrypted_privkey)
                && let Ok(kp) = Keypair::from_secret_bytes(secret_bytes)
            {
                // The user's configured external relays — the publish targets
                // for both advertisement events (this is how the wider Nostr
                // network learns where to reach the user).
                let external_relays: Vec<String> =
                    serde_json::from_str(relay_list_json).unwrap_or_default();

                // NIP-65 (kind 10002): the user's external relays plus this nest.
                let mut nip65_relays = external_relays.clone();
                if let Some(ref u) = nest_relay_url
                    && !nip65_relays.contains(u)
                {
                    nip65_relays.push(u.clone());
                }
                if let Ok(nip65_json) = serde_json::to_string(&nip65_relays)
                    && let Some(event) =
                        crate::nostr::sync_worker::build_nip65_event(&kp, &nip65_json)
                {
                    let outbound = crate::nostr::sync_worker::OutboundEvent {
                        event,
                        relay_urls: external_relays.clone(),
                    };
                    let _ = state.nostr.sync_tx.send(outbound).await;
                    tracing::info!(
                        "nostr: published NIP-65 relay list for {}",
                        &actor_id[..8.min(actor_id.len())]
                    );
                }

                // NIP-17 (kind 10050): advertise this nest as the DM-inbox relay.
                if let Some(ref u) = nest_relay_url
                    && let Some(dm_event) = crate::nostr::sync_worker::build_nip10050_event(
                        &kp,
                        std::slice::from_ref(u),
                    )
                {
                    let outbound = crate::nostr::sync_worker::OutboundEvent {
                        event: dm_event,
                        relay_urls: external_relays.clone(),
                    };
                    let _ = state.nostr.sync_tx.send(outbound).await;
                    tracing::info!(
                        "nostr: published NIP-17 DM-inbox list for {}",
                        &actor_id[..8.min(actor_id.len())]
                    );
                }
            }
        }

        // Drop the guard here — `materialize_account` below acquires its own
        // (segment reads are async and go through `state.db`'s own locking;
        // holding this guard across that call would deadlock).
        drop(conn);

        // React to an `expose_content` toggle immediately (the sync worker's
        // periodic sweep is the ongoing safety net). Turning it on materializes
        // the back-catalogue of exposed posts into the relay store — translated
        // + signed once, deduped — so REQ serves signed rows instead of
        // re-signing per read. Turning it off deletes the derived rows (they are
        // recreatable — re-derived on re-expose — so this is no data loss).
        match nostr_settings.expose_content {
            Some(true) => {
                let acct = {
                    let conn = state.db.conn().await;
                    db::get_account(&conn, actor_id)
                };
                if let Ok(Some(acct)) = acct
                    && let Some(ref encrypted_privkey) = acct.encrypted_privkey
                {
                    let nest_key_bytes = state.nest_identity.signing_key.to_bytes();
                    if let Ok(secret_bytes) =
                        key_crypto::decrypt_nostr_privkey(&nest_key_bytes, encrypted_privkey)
                        && let Ok(kp) = Keypair::from_secret_bytes(secret_bytes)
                    {
                        match store::materialize_account(
                            &state.db,
                            &state.post_segments,
                            actor_id,
                            &kp,
                        )
                        .await
                        {
                            Ok(n) if n > 0 => tracing::info!(
                                "nostr: materialized {n} exposed posts for {}",
                                &actor_id[..8.min(actor_id.len())]
                            ),
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("nostr: materialize on expose-on failed: {e}")
                            }
                        }
                    }
                }
            }
            Some(false) => {
                let conn = state.db.conn().await;
                if let Ok(Some(acct)) = db::get_account(&conn, actor_id) {
                    match store::delete_derived_for_pubkey(&conn, &acct.nostr_pubkey) {
                        Ok(n) if n > 0 => tracing::info!(
                            "nostr: removed {n} derived events on expose-off for {}",
                            &actor_id[..8.min(actor_id.len())]
                        ),
                        Ok(_) => {}
                        Err(e) => tracing::warn!("nostr: delete derived on expose-off failed: {e}"),
                    }
                }
            }
            None => {}
        }

        Ok(())
    }

    async fn list_follows(
        &self,
        state: &AppState,
        actor_id: &str,
    ) -> Result<Vec<BridgeFollow>, BridgeError> {
        let conn = state.db.conn().await;
        let follows = db::list_follows(&conn, actor_id)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))?;
        drop(conn);
        Ok(follows
            .into_iter()
            .map(|f| {
                // Storage keeps relay_hints as a JSON string for the
                // bridge-internal sync workers; reproject into CBOR for
                // the wire by parsing → fauna_cbor round-trip. The legacy
                // shape was `{"relay_hints": <parsed>}`; preserved here.
                let relay_hints_cbor: Option<Value> = f.relay_hints.as_deref().and_then(|rh| {
                    serde_json::from_str::<serde_json::Value>(rh)
                        .ok()
                        .and_then(|j| {
                            let bytes = fauna_cbor::encode_canonical(&j).ok()?;
                            fauna_cbor::decode_strict(&bytes).ok()
                        })
                });
                let extra = relay_hints_cbor.map(|rh| {
                    Value::Map(std::collections::BTreeMap::from([(
                        "relay_hints".to_string(),
                        rh,
                    )]))
                });
                let npub_id = hex::decode(&f.nostr_pubkey)
                    .ok()
                    .and_then(|b| {
                        if b.len() == 32 {
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&b);
                            Some(arr)
                        } else {
                            None
                        }
                    })
                    .map(|b| nip19::encode_npub(&b))
                    .unwrap_or_else(|| f.nostr_pubkey.clone());
                BridgeFollow {
                    id: npub_id,
                    petname: f.petname,
                    created_at: Some(f.created_at),
                    extra,
                    unknown_keys: Default::default(),
                }
            })
            .collect())
    }

    async fn add_follow(
        &self,
        state: &AppState,
        actor_id: &str,
        id: &str,
        petname: Option<&str>,
        extra: Option<Value>,
    ) -> Result<(), BridgeError> {
        // Accept npub or hex
        let pubkey_hex = if id.starts_with("npub1") {
            nip19::decode_npub(id)
                .map(hex::encode)
                .map_err(|e| BridgeError::invalid_params(&format!("invalid npub: {e}")))?
        } else {
            id.to_string()
        };
        // Storage retains the legacy JSON encoding for relay_hints so the
        // bridge sync workers don't need a CBOR-aware path; convert at the
        // boundary by serializing the CBOR shape via fauna_cbor → reading
        // back as serde_json for the to_string hop.
        let relay_hints_json = extra.and_then(|e| {
            let wire: FollowExtraWire = deserialize_value(&e).ok()?;
            let rh = wire.relay_hints?;
            let bytes = fauna_cbor::encode_canonical(&rh).ok()?;
            let j: serde_json::Value = fauna_cbor::decode_strict(&bytes).ok()?;
            serde_json::to_string(&j).ok()
        });
        if let Some(hints) = relay_hints_json
            .as_deref()
            .and_then(|j| serde_json::from_str::<Vec<String>>(j).ok())
        {
            refuse_undialable_relays(hints.iter().map(String::as_str), state, "relay hint")?;
        }
        let conn = state.db.conn().await;
        db::add_follow(
            &conn,
            actor_id,
            &pubkey_hex,
            petname,
            relay_hints_json.as_deref(),
        )
        .map_err(|e| BridgeError::provider_error(&e.to_string()))
    }

    async fn remove_follow(
        &self,
        state: &AppState,
        actor_id: &str,
        follow_id: &str,
    ) -> Result<(), BridgeError> {
        let pubkey_hex = if follow_id.starts_with("npub1") {
            nip19::decode_npub(follow_id)
                .map(hex::encode)
                .map_err(|e| BridgeError::invalid_params(&format!("invalid npub: {e}")))?
        } else {
            follow_id.to_string()
        };
        let conn = state.db.conn().await;
        db::remove_follow(&conn, actor_id, &pubkey_hex)
            .map_err(|e| BridgeError::provider_error(&e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    /// The deployment-seed rotation's hand-off window for the deposited nsec,
    /// driven causally: the ceremony has committed while a serving generation
    /// not yet torn down still holds the retired seed (`box-recovery.md`
    /// § Deployment-seed rotation → *The bounded hand-off window*). A link that
    /// generation answers deposits a key, and must seal it under the seed the
    /// database holds: a deposit sealed under the retired copy never opens
    /// again, and the satellite walk refuses it on every later rotation.
    mod rotation_window {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        use fauna_bridge_nostr::nip19;
        use fauna_bridge_nostr::signing::Keypair;
        use fauna_protocol::Value;
        use zeroize::Zeroizing;

        use crate::bridge_management::BridgeProvider;
        use crate::db::CacheDb;
        use crate::test_support::{
            every_row_opens_under, seat_deployment_seed, serving_generation,
        };

        use super::super::NostrProvider;

        async fn the_link_seals_under_the_successor_seed(mode: &str, params: Value) {
            let (a, b, c) = (
                Zeroizing::new([0xa1u8; 32]),
                Zeroizing::new([0xb2u8; 32]),
                Zeroizing::new([0xc3u8; 32]),
            );
            let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
            crate::nostr::init_db(&db).await.expect("nostr tables");
            seat_deployment_seed(&db, &a).await;
            let outgoing = serving_generation(db.clone(), &a);

            db.rotate_deployment_seed(&a, &b)
                .await
                .expect("the ceremony runs")
                .expect("and commits");

            NostrProvider
                .link(&outgoing, &hex::encode([0x44u8; 32]), mode, params)
                .await
                .expect("the outgoing generation answers the link");
            assert!(
                every_row_opens_under(
                    &db,
                    "nostr_accounts",
                    "encrypted_privkey",
                    crate::nest_kek::NOSTR_NSEC_CONTEXT,
                    &b,
                )
                .await,
                "a {mode} link answered by the outgoing generation sealed the deposit under the \
                 retired deployment seed"
            );

            db.rotate_deployment_seed(&b, &c)
                .await
                .expect("the next rotation commits — the deposit does not wedge the walk")
                .expect("and is no rule refusal");
        }

        #[tokio::test]
        async fn a_generate_link_seals_under_the_successor_seed() {
            the_link_seals_under_the_successor_seed("generate", Value::Map(BTreeMap::new())).await;
        }

        #[tokio::test]
        async fn an_import_link_seals_under_the_successor_seed() {
            let nsec = nip19::encode_nsec(&Keypair::generate().secret_bytes());
            let params = Value::Map(BTreeMap::from([("nsec".to_string(), Value::String(nsec))]));
            the_link_seals_under_the_successor_seed("import", params).await;
        }
    }

    /// One pubkey, one actor: a link carrying a pubkey another actor on this
    /// box already holds is refused in every mode, and the holder's row —
    /// its deposited nsec included — survives. `nip07` and `remote` carry an
    /// unproven pubkey, so without this any user could delete another's
    /// account by naming its key.
    mod pubkey_held_elsewhere {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        use fauna_bridge_nostr::nip19;
        use fauna_bridge_nostr::signing::Keypair;
        use fauna_protocol::Value;

        use crate::bridge_management::BridgeProvider;
        use crate::db::CacheDb;
        use crate::nostr::db;
        use crate::routes::AppState;

        use super::super::NostrProvider;

        const VICTIM: &str = "7777777777777777777777777777777777777777777777777777777777777777";
        const ATTACKER: &str = "4242424242424242424242424242424242424242424242424242424242424242";

        /// `expected` is the refusal code: `identity_in_use` wherever the link
        /// reaches the writer; an arm that refuses before the writer (a
        /// `remote` link whose bunker cannot be dialled) passes its own code —
        /// the holder must survive either way.
        async fn refused_with_the_holder_intact(
            mode: &str,
            expected: &str,
            params: impl FnOnce(&Keypair, &AppState) -> Value,
        ) {
            let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
            crate::nostr::init_db(&db).await.expect("nostr tables");
            // The import arm seals its deposit before it reaches the writer.
            crate::test_support::seat_deployment_seed(&db, &[0xa1u8; 32]).await;
            // Loopback is admitted so the `remote` arm's dead bunker gets as
            // far as the dial: under the default policy the store-time check
            // refuses its URL first, which is `relay_store_guard`'s case.
            let state = AppState {
                nostr: crate::state::NostrState {
                    relay_dial_policy:
                        fauna_bridge_nostr::relay_client::RelayDialPolicy::PublicOrLoopback,
                    ..Default::default()
                },
                ..AppState::for_test(db)
            };
            let kp = Keypair::generate();
            {
                let conn = state.db.conn().await;
                db::link_account(
                    &conn,
                    VICTIM,
                    &kp.public_key_hex(),
                    "generated",
                    Some(&[0xAAu8; 48]),
                    None,
                    None,
                )
                .unwrap();
            }

            let err = NostrProvider
                .link(&state, ATTACKER, mode, params(&kp, &state))
                .await
                .expect_err("a pubkey another actor holds must be refused");
            assert_eq!(err.code, expected, "{mode}: {}", err.error);

            let conn = state.db.conn().await;
            let victim = db::get_account(&conn, VICTIM)
                .unwrap()
                .expect("the holder's row survives");
            assert_eq!(victim.encrypted_privkey.as_deref(), Some(&[0xAAu8; 48][..]));
            assert!(db::get_account(&conn, ATTACKER).unwrap().is_none());
        }

        fn one(key: &str, value: String) -> Value {
            Value::Map(BTreeMap::from([(key.to_string(), Value::String(value))]))
        }

        /// Even a VALID proof — the attacker somehow holds the key — cannot take
        /// a pubkey another account holds: the proof gets it to the writer, and
        /// the writer refuses.
        #[tokio::test]
        async fn a_nip07_link() {
            refused_with_the_holder_intact("nip07", "identity_in_use", |kp, state| {
                let challenge =
                    super::super::mint_challenge(state, ATTACKER, super::super::now_secs());
                let proof = serde_json::to_string(&challenge.template().sign(kp)).unwrap();
                Value::Map(BTreeMap::from([
                    ("pubkey".to_string(), Value::String(kp.public_key_hex())),
                    ("proof_json".to_string(), Value::String(proof)),
                ]))
            })
            .await;
        }

        /// A `remote` link reaches the writer only through a live bunker
        /// handshake — its `identity_in_use` case is tier_3
        /// (`tests/nostr_link_proof.rs`). Here the bunker is undialable, so the
        /// link stops before the writer and the holder is untouched.
        #[tokio::test]
        async fn a_remote_link() {
            refused_with_the_holder_intact("remote", "provider_error", |kp, _| {
                one(
                    "bunker_url",
                    format!(
                        "bunker://{}?relay=ws://127.0.0.1:1/nostr",
                        kp.public_key_hex()
                    ),
                )
            })
            .await;
        }

        #[tokio::test]
        async fn an_import_link() {
            refused_with_the_holder_intact("import", "identity_in_use", |kp, _| {
                one("nsec", nip19::encode_nsec(&kp.secret_bytes()))
            })
            .await;
        }
    }

    /// Proof of possession for `nip07` (`nostr.md` § Errors & edge cases →
    /// *Proof of possession*): the row is written only for a pubkey whose
    /// holder signed THIS nest's live challenge; every other shape is the
    /// typed `proof_required`, and a challenge is single-use and short-lived.
    /// The `remote` arm's proof (the NIP-46 handshake) is tier_3
    /// (`tests/nostr_link_proof.rs`) — it needs a relay and a bunker.
    mod proof_of_possession {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        use fauna_bridge_nostr::nip42::AuthEventTemplate;
        use fauna_bridge_nostr::signing::Keypair;
        use fauna_protocol::Value;

        use crate::bridge_management::{BridgeProvider, deserialize_value};
        use crate::db::CacheDb;
        use crate::nostr::db;
        use crate::routes::AppState;

        use super::super::{LINK_CHALLENGE_TTL_SECS, LinkChallenge, NostrProvider};

        const ACTOR: &str = "1111111111111111111111111111111111111111111111111111111111111111";

        async fn state() -> AppState {
            let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
            crate::nostr::init_db(&db).await.expect("nostr tables");
            AppState::for_test(db)
        }

        /// Ask for the challenge as the app does and read the template back
        /// out of the reply's payload — the object the extension signs.
        async fn challenge_for(state: &AppState, actor: &str) -> AuthEventTemplate {
            let reply = NostrProvider
                .link_challenge(state, actor, "nip07")
                .await
                .expect("nip07 issues a challenge");
            assert_eq!(reply.challenge.len(), 32, "16 random bytes as hex");
            let template: AuthEventTemplate =
                deserialize_value(&reply.payload).expect("payload is the unsigned event");
            assert_eq!(template.kind, 22242);
            assert!(template.tags.iter().any(|t| {
                t.name() == Some("challenge") && t.value() == Some(reply.challenge.as_str())
            }));
            template
        }

        fn nip07_params(pubkey: &str, proof_json: Option<String>) -> Value {
            let mut m = BTreeMap::from([("pubkey".to_string(), Value::String(pubkey.into()))]);
            if let Some(json) = proof_json {
                m.insert("proof_json".to_string(), Value::String(json));
            }
            Value::Map(m)
        }

        fn signed(template: AuthEventTemplate, kp: &Keypair) -> String {
            serde_json::to_string(&template.sign(kp)).unwrap()
        }

        async fn no_row(state: &AppState, actor: &str) {
            let conn = state.db.conn().await;
            assert!(db::get_account(&conn, actor).unwrap().is_none());
        }

        #[tokio::test]
        async fn a_bare_pubkey_is_refused_proof_required_never_linked() {
            let state = state().await;
            let kp = Keypair::generate();
            let err = NostrProvider
                .link(
                    &state,
                    ACTOR,
                    "nip07",
                    nip07_params(&kp.public_key_hex(), None),
                )
                .await
                .expect_err("a bare pubkey is refused, not linked");
            assert_eq!(err.code, "proof_required", "{}", err.error);
            no_row(&state, ACTOR).await;
        }

        #[tokio::test]
        async fn a_proof_signed_by_the_claimed_key_over_the_live_challenge_links() {
            let state = state().await;
            let kp = Keypair::generate();
            let template = challenge_for(&state, ACTOR).await;
            let reply = NostrProvider
                .link(
                    &state,
                    ACTOR,
                    "nip07",
                    nip07_params(&kp.public_key_hex(), Some(signed(template, &kp))),
                )
                .await
                .expect("a valid proof links");
            assert!(reply.linked);
            let conn = state.db.conn().await;
            let row = db::get_account(&conn, ACTOR).unwrap().expect("row written");
            assert_eq!(row.nostr_pubkey, kp.public_key_hex());
            assert_eq!(row.signing_mode, "nip07");
            assert!(
                row.encrypted_privkey.is_none(),
                "an external signer deposits nothing"
            );
        }

        #[tokio::test]
        async fn a_proof_by_another_key_than_the_one_claimed_is_refused() {
            let state = state().await;
            let holder = Keypair::generate();
            let victim = Keypair::generate();
            let template = challenge_for(&state, ACTOR).await;
            let err = NostrProvider
                .link(
                    &state,
                    ACTOR,
                    "nip07",
                    // Claims the victim's pubkey, proves the holder's.
                    nip07_params(&victim.public_key_hex(), Some(signed(template, &holder))),
                )
                .await
                .expect_err("the claimed key and the signing key must agree");
            assert_eq!(err.code, "proof_required", "{}", err.error);
            no_row(&state, ACTOR).await;
        }

        #[tokio::test]
        async fn a_proof_over_a_superseded_challenge_is_refused() {
            let state = state().await;
            let kp = Keypair::generate();
            let first = challenge_for(&state, ACTOR).await;
            let _second = challenge_for(&state, ACTOR).await;
            let err = NostrProvider
                .link(
                    &state,
                    ACTOR,
                    "nip07",
                    nip07_params(&kp.public_key_hex(), Some(signed(first, &kp))),
                )
                .await
                .expect_err("only the newest challenge is live");
            assert_eq!(err.code, "proof_required", "{}", err.error);
            no_row(&state, ACTOR).await;
        }

        #[tokio::test]
        async fn a_challenge_is_single_use() {
            let state = state().await;
            let kp = Keypair::generate();
            let template = challenge_for(&state, ACTOR).await;
            let proof = signed(template, &kp);
            NostrProvider
                .link(
                    &state,
                    ACTOR,
                    "nip07",
                    nip07_params(&kp.public_key_hex(), Some(proof.clone())),
                )
                .await
                .expect("first use links");
            NostrProvider.unlink(&state, ACTOR).await.unwrap();
            let err = NostrProvider
                .link(
                    &state,
                    ACTOR,
                    "nip07",
                    nip07_params(&kp.public_key_hex(), Some(proof)),
                )
                .await
                .expect_err("a replayed proof finds no live challenge");
            assert_eq!(err.code, "proof_required", "{}", err.error);
            no_row(&state, ACTOR).await;
        }

        #[tokio::test]
        async fn an_expired_challenge_is_refused() {
            let state = state().await;
            let kp = Keypair::generate();
            let template = challenge_for(&state, ACTOR).await;
            // Age the outstanding challenge past its TTL in place.
            {
                let mut pending = state.nostr.link_challenges.lock().unwrap();
                let c: &mut LinkChallenge = pending.get_mut(ACTOR).expect("outstanding");
                c.expires_at -= LINK_CHALLENGE_TTL_SECS + 1;
            }
            let err = NostrProvider
                .link(
                    &state,
                    ACTOR,
                    "nip07",
                    nip07_params(&kp.public_key_hex(), Some(signed(template, &kp))),
                )
                .await
                .expect_err("an expired challenge is dead");
            assert_eq!(err.code, "proof_required", "{}", err.error);
            no_row(&state, ACTOR).await;
        }

        #[tokio::test]
        async fn a_tampered_proof_is_refused() {
            let state = state().await;
            let kp = Keypair::generate();
            let template = challenge_for(&state, ACTOR).await;
            let mut event = template.sign(&kp);
            event.content = "tampered".into();
            let err = NostrProvider
                .link(
                    &state,
                    ACTOR,
                    "nip07",
                    nip07_params(
                        &kp.public_key_hex(),
                        Some(serde_json::to_string(&event).unwrap()),
                    ),
                )
                .await
                .expect_err("id/signature no longer verify");
            assert_eq!(err.code, "proof_required", "{}", err.error);
            no_row(&state, ACTOR).await;
        }

        #[tokio::test]
        async fn only_nip07_issues_a_challenge() {
            let state = state().await;
            for mode in ["generate", "import", "remote"] {
                let err = NostrProvider
                    .link_challenge(&state, ACTOR, mode)
                    .await
                    .expect_err(mode);
                assert_eq!(err.code, "invalid_params", "{mode}: {}", err.error);
            }
            let err = NostrProvider
                .link_challenge(&state, ACTOR, "bogus")
                .await
                .expect_err("unknown mode");
            assert_eq!(err.code, "invalid_mode");
        }
    }

    /// The store-time half of the relay dial guard (`nest/network-exposure.md`
    /// § Rulings F7): each user-supplied relay source refuses a private-literal
    /// relay with `invalid_params` and writes nothing — the relay list, a
    /// follow's hints, a `remote` link's bunker string (before any dial).
    mod relay_store_guard {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        use fauna_bridge_nostr::relay_client::RelayDialPolicy;
        use fauna_bridge_nostr::signing::Keypair;
        use fauna_protocol::Value;

        use crate::bridge_management::BridgeProvider;
        use crate::db::CacheDb;
        use crate::nostr::db;
        use crate::routes::AppState;

        use super::super::NostrProvider;

        const ACTOR: &str = "2222222222222222222222222222222222222222222222222222222222222222";
        const PRIVATE_RELAYS: [&str; 4] = [
            "ws://127.0.0.1:7777",
            "wss://10.0.0.7",
            "ws://[fd00::1]",
            "ws://169.254.169.254",
        ];

        async fn linked_state() -> AppState {
            let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
            crate::nostr::init_db(&db).await.expect("nostr tables");
            let state = AppState::for_test(db);
            let conn = state.db.conn().await;
            db::link_account(
                &conn,
                ACTOR,
                &Keypair::generate().public_key_hex(),
                "remote",
                None,
                None,
                Some(r#"["wss://relay.example.com"]"#),
            )
            .expect("link");
            drop(conn);
            state
        }

        fn map(entries: Vec<(&str, Value)>) -> Value {
            Value::Map(
                entries
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect::<BTreeMap<_, _>>(),
            )
        }

        #[tokio::test]
        async fn a_private_relay_in_the_list_is_refused_and_the_row_kept() {
            let state = linked_state().await;
            for bad in PRIVATE_RELAYS {
                let list = format!(r#"["wss://relay.example.com","{bad}"]"#);
                let err = NostrProvider
                    .update_settings(
                        &state,
                        ACTOR,
                        map(vec![("relay_list", Value::String(list))]),
                    )
                    .await
                    .expect_err(bad);
                assert_eq!(err.code, "invalid_params", "{bad}: {}", err.error);
            }
            let conn = state.db.conn().await;
            let acct = db::get_account(&conn, ACTOR).unwrap().expect("account");
            assert_eq!(
                acct.relay_list.as_deref(),
                Some(r#"["wss://relay.example.com"]"#)
            );
        }

        #[tokio::test]
        async fn a_public_relay_list_is_stored() {
            let state = linked_state().await;
            let list = r#"["wss://relay.example.com","wss://8.8.8.8"]"#;
            NostrProvider
                .update_settings(
                    &state,
                    ACTOR,
                    map(vec![("relay_list", Value::String(list.to_string()))]),
                )
                .await
                .expect("a public list is accepted");
            let conn = state.db.conn().await;
            let acct = db::get_account(&conn, ACTOR).unwrap().expect("account");
            assert_eq!(acct.relay_list.as_deref(), Some(list));
        }

        #[tokio::test]
        async fn a_private_follow_hint_is_refused_and_no_follow_written() {
            let state = linked_state().await;
            let target = Keypair::generate().public_key_hex();
            for bad in PRIVATE_RELAYS {
                let extra = map(vec![(
                    "relay_hints",
                    Value::List(vec![Value::String(bad.to_string())]),
                )]);
                let err = NostrProvider
                    .add_follow(&state, ACTOR, &target, None, Some(extra))
                    .await
                    .expect_err(bad);
                assert_eq!(err.code, "invalid_params", "{bad}: {}", err.error);
            }
            let conn = state.db.conn().await;
            assert!(db::list_follows(&conn, ACTOR).unwrap().is_empty());
        }

        #[tokio::test]
        async fn a_private_bunker_relay_is_refused_before_any_dial() {
            // Even the loopback-permitting test policy refuses a private range:
            // the store-time check runs under the nest's own dial policy.
            for policy in [
                RelayDialPolicy::PublicOnly,
                RelayDialPolicy::PublicOrLoopback,
            ] {
                let base = linked_state().await;
                let state = AppState {
                    nostr: crate::state::NostrState {
                        relay_dial_policy: policy,
                        ..Default::default()
                    },
                    ..base
                };
                let url = format!(
                    "bunker://{}?relay=wss://10.0.0.7&secret=s",
                    Keypair::generate().public_key_hex()
                );
                let err = NostrProvider
                    .link(
                        &state,
                        "3333333333333333333333333333333333333333333333333333333333333333",
                        "remote",
                        map(vec![("bunker_url", Value::String(url))]),
                    )
                    .await
                    .expect_err("a private bunker relay is refused");
                assert_eq!(err.code, "invalid_params", "{policy:?}: {}", err.error);
            }
        }
    }
}
