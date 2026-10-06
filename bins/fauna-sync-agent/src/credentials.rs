//! Capability persistence — `sync-agent.md` § Credential model (ratified
//! 2026-07-19).
//!
//! The agent persists its provisioned [`SyncCapability`] (BackupKey +
//! actor_id + bearer + the renewal device signing key) in the per-user
//! platform secure store via `fauna-credential-store`, namespace
//! **`fauna-sync-agent`**: macOS login Keychain, linux Secret Service (with
//! the `0600`-file backend for e2e / the sanctioned headless case), windows
//! Credential Manager. This is what lets the agent run **app-dead**: at boot
//! it reloads the capability with no login-time push, and the renewal loop
//! (`crate::renewal`) keeps the bearer fresh from the persisted renewal key.
//!
//! Storage shape: one record, account key [`CAPABILITY_KEY`], value =
//! hex(canonical dag-cbor of `SyncCapability`). The dag-cbor shape is the same
//! additive-tolerant serde the IPC wire uses (`#[serde(default)]` on every
//! post-v1 field), so an older agent reading a newer record — or the reverse —
//! degrades instead of erroring (`version-compatibility.md` I4 applied
//! at-rest). The `/v1` suffix in the key names the *record framing* (hex +
//! dag-cbor), not the capability schema, which evolves additively inside it.
//!
//! Un-provisioning ([`delete_capability`]) removes the record; in-memory
//! copies stay `Zeroizing` at every hop.

use zeroize::{Zeroize, Zeroizing};

use fauna_ipc::sync::{SignedOutMarker, SyncCapability, capability_is_signed_out};

/// The credential-store namespace (`application` attribute / Keychain service).
/// Also the namespace `tui.md` § Credential storage documents for this agent.
///
/// Re-exported from [`fauna_ipc::sync`], which owns it: the app writes this
/// namespace too (the sign-out marker), so it is app↔agent contract, not an
/// agent-private detail.
pub use fauna_ipc::sync::CRED_NAMESPACE;

/// Account key of the persisted capability record.
pub use fauna_ipc::sync::CAPABILITY_KEY;

/// The production per-user store, honoring the shared env contract
/// (`FAUNA_KEYRING_APP` namespace override, `FAUNA_E2E_CREDENTIAL_DIR` file
/// backend) so e2e runs never touch a developer's real keyring. Built **once**
/// by `run_agent` and injected into `SyncServiceState.credentials` — unit
/// tests construct state with `None` and never reach the OS store (the
/// launch-isolation rule of testing.md § point 10, applied to the agent's own
/// tests).
pub fn production_store() -> fauna_credential_store::CredentialStore {
    fauna_credential_store::CredentialStore::new(CRED_NAMESPACE)
}

/// Persist the capability (called on provision and on every bearer refresh —
/// the persisted record must carry the *current* bearer, or a reboot resumes
/// from an expired one and waits a full renewal round-trip).
pub fn persist_capability(store: &fauna_credential_store::CredentialStore, cap: &SyncCapability) {
    use fauna_client_accounts::SecretStore;
    let bytes = match fauna_cbor::encode_canonical(cap) {
        Ok(b) => Zeroizing::new(b),
        Err(e) => {
            tracing::warn!("capability persist: encode failed: {e}");
            return;
        }
    };
    let hex_value = Zeroizing::new(hex::encode(&*bytes));
    store.set(CAPABILITY_KEY, &hex_value);
    tracing::info!("capability persisted to credential store");
}

/// Load the persisted capability, or `None` when absent / unreadable (a
/// corrupt record logs and reads as absent — the app re-provisions on its next
/// convergence tick, so fail-open-to-empty is self-healing).
pub fn load_capability(store: &fauna_credential_store::CredentialStore) -> Option<SyncCapability> {
    use fauna_client_accounts::SecretStore;
    let mut hex_value = store.get(CAPABILITY_KEY)?;
    let decoded = hex::decode(&hex_value);
    hex_value.zeroize();
    let bytes = match decoded {
        Ok(b) => Zeroizing::new(b),
        Err(e) => {
            tracing::warn!("capability load: stored record is not hex: {e}");
            return None;
        }
    };
    match fauna_cbor::decode_strict::<SyncCapability>(&bytes) {
        Ok(cap) => Some(cap),
        Err(e) => {
            tracing::warn!("capability load: decode failed: {e:?}");
            None
        }
    }
}

/// Delete the persisted record (the un-provision half: sign-out, account
/// switch, revocation). Idempotent.
pub fn delete_capability(store: &fauna_credential_store::CredentialStore) {
    use fauna_client_accounts::SecretStore;
    store.delete(CAPABILITY_KEY);
    tracing::info!("capability deleted from credential store");
}

/// Account key of the persisted [`RefusalRecord`], beside the capability's.
/// Agent-private: no app reads or writes it. `/v1` names the record framing
/// (hex + dag-cbor), as [`CAPABILITY_KEY`]'s does.
pub const RENEWAL_REFUSED_KEY: &str = "renewal-refused/v1";

/// What the nest refused when it answered this machine's renewal
/// `fauna.auth.not_registered` — and, while it stands, the reason the agent
/// reports `needs_reenrollment` (`sync-agent-credentials.md` § Credential
/// model → *A refused renewal is terminal*: the report follows this record,
/// never the bearer slot, so an app's pushed bearer does not clear it).
///
/// It names the refused evidence and holds no secret. The renewal loop asks
/// the nest again at once only when the machine's current evidence
/// [`is_news_to`](Self::is_news_to) it. Not a field on `SyncCapability`: that
/// is the IPC type an app fills, and no app may set this.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RefusalRecord {
    /// The nest that refused.
    pub nest_url: String,
    /// The store principal's **public** key the refused mint signed with, hex.
    pub principal_key: String,
    /// Whether the enrollment ceremony's registration latch read *registered*
    /// at the refusal (`renewal::principal_is_registered`).
    #[serde(default)]
    pub grant_registered: bool,
}

impl RefusalRecord {
    /// Whether this evidence is something `refused` was not refused on: another
    /// nest, another principal key, or the latch now reading *registered* where
    /// the record says it did not. A latch that went the other way is no news.
    pub fn is_news_to(&self, refused: &RefusalRecord) -> bool {
        self.nest_url != refused.nest_url
            || self.principal_key != refused.principal_key
            || (self.grant_registered && !refused.grant_registered)
    }
}

/// Persist the refusal record (same framing as the capability).
pub fn persist_refusal(store: &fauna_credential_store::CredentialStore, record: &RefusalRecord) {
    use fauna_client_accounts::SecretStore;
    match fauna_cbor::encode_canonical(record) {
        Ok(bytes) => store.set(RENEWAL_REFUSED_KEY, &hex::encode(bytes)),
        Err(e) => tracing::warn!("refusal record persist: encode failed: {e}"),
    }
}

/// Load the refusal record. Absent or unreadable reads as *not refused*.
pub fn load_refusal(store: &fauna_credential_store::CredentialStore) -> Option<RefusalRecord> {
    use fauna_client_accounts::SecretStore;
    let record = store.get(RENEWAL_REFUSED_KEY)?;
    let decoded = hex::decode(&record)
        .ok()
        .and_then(|bytes| fauna_cbor::decode_strict::<RefusalRecord>(&bytes).ok());
    if decoded.is_none() {
        tracing::warn!("refusal record: unreadable; treating as not refused");
    }
    decoded
}

/// Delete the refusal record — on a successful renewal, and wherever the
/// capability record is deleted. Idempotent.
pub fn delete_refusal(store: &fauna_credential_store::CredentialStore) {
    use fauna_client_accounts::SecretStore;
    store.delete(RENEWAL_REFUSED_KEY);
}

/// Load the app-written sign-out marker, or `None` when absent / unreadable.
///
/// A corrupt marker reads as absent — the same fail-open-to-empty shape
/// [`load_capability`] uses, and the safe direction here too: an unreadable
/// marker must not silently revoke a live capability, and the app re-writes one
/// at the next sign-out.
pub fn load_signed_out_marker(
    store: &fauna_credential_store::CredentialStore,
) -> Option<SignedOutMarker> {
    use fauna_client_accounts::SecretStore;
    let record = store.get(fauna_ipc::sync::SIGNED_OUT_KEY)?;
    match SignedOutMarker::decode_record(&record) {
        Some(m) => Some(m),
        None => {
            tracing::warn!("sign-out marker: unreadable record; treating as absent");
            None
        }
    }
}

/// The boot-path restore, capability **and** its authorization in one decision
/// (`sync-agent.md` § Credential model → *The signed-out reconcile*).
///
/// Returns the capability the agent may serve, or `None` — and when a persisted
/// capability is refused, **deletes it**, so a machine that reboots twice does
/// not re-litigate the same revoked capability (and so the record does not sit
/// at rest naming an account that signed out).
///
/// This is the single decision point `service::run_agent` and the agent's own
/// resume tests share. Re-deriving it at either call site is what let the boot
/// path and its test drift apart before.
pub fn restore_authorized_capability(
    store: &fauna_credential_store::CredentialStore,
) -> Option<SyncCapability> {
    let cap = load_capability(store)?;
    let Some(marker) = load_signed_out_marker(store) else {
        return Some(cap);
    };
    if capability_is_signed_out(&cap, &marker) {
        tracing::warn!(
            signed_out_at_ms = marker.signed_out_at_ms,
            provisioned_at_ms = ?cap.provisioned_at_ms,
            "refusing to resume a capability whose account signed out on this machine; \
             the un-provision message was never delivered — dropping the persisted record"
        );
        delete_capability(store);
        delete_refusal(store);
        return None;
    }
    Some(cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_accounts::SecretStore;
    use fauna_ipc::sync::BearerToken;

    fn test_store(dir: &std::path::Path) -> fauna_credential_store::CredentialStore {
        fauna_credential_store::CredentialStore::with_file_backend(
            "fauna-sync-agent-test",
            dir.to_path_buf(),
        )
    }

    fn cap() -> SyncCapability {
        SyncCapability::new(
            vec![1u8; 32],
            vec![2u8; 32],
            "https://nest.example".into(),
            "dev-test".into(),
            BearerToken::new("tok-1".into(), 1234),
        )
    }

    /// The round-trip the boot path takes, against an explicit file-backend
    /// store (no env, no D-Bus, no real keyring).
    #[test]
    fn capability_round_trips_through_a_store() {
        let dir = std::env::temp_dir().join(format!("sync-agent-cred-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = test_store(&dir);

        persist_capability(&store, &cap());
        let loaded = load_capability(&store).expect("record present");
        assert_eq!(loaded.nest_url, "https://nest.example");
        assert_eq!(loaded.device_id, "dev-test");
        assert_eq!(loaded.bearer.token, "tok-1");
        assert_eq!(loaded.backup_key_array(), Some([1u8; 32]));

        delete_capability(&store);
        assert!(
            load_capability(&store).is_none(),
            "deleted record is gone (un-provision)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_marker(store: &fauna_credential_store::CredentialStore, actor: [u8; 32], at_ms: u64) {
        let record = SignedOutMarker::new(actor.to_vec(), at_ms)
            .encode_record()
            .expect("marker encodes");
        store.set(fauna_ipc::sync::SIGNED_OUT_KEY, &record);
    }

    /// **The lost-message case, boot arm**.
    ///
    /// The app signed out and its single best-effort `UnprovisionCapability`
    /// never arrived — modelled by never calling the handler at all, which is
    /// exactly what a wedged agent or a timed-out connect leaves behind. The
    /// persisted capability therefore survives the sign-out, and before this
    /// reconcile every later boot resumed it: a signed-out account's engines
    /// served indefinitely and revived on every reboot.
    ///
    /// Asserts the boot path refuses it **and drops the record**, so the second
    /// reboot does not re-litigate the same revoked capability.
    #[test]
    fn a_boot_refuses_a_capability_whose_account_signed_out() {
        let dir = std::env::temp_dir().join(format!("sync-agent-signedout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = test_store(&dir);
        let actor = [2u8; 32]; // `cap()`'s actor_id.

        // Provisioned, then signed out one second later; the message is lost.
        persist_capability(&store, &cap().with_provisioned_at_ms(1_000));
        write_marker(&store, actor, 2_000);

        assert!(
            restore_authorized_capability(&store).is_none(),
            "a capability whose account signed out must not be resumed"
        );
        assert!(
            load_capability(&store).is_none(),
            "the refused capability is dropped, not left at rest naming a signed-out account"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn refusal() -> RefusalRecord {
        RefusalRecord {
            nest_url: "https://nest.example".into(),
            principal_key: hex::encode([0xc3u8; 32]),
            grant_registered: false,
        }
    }

    /// The refusal record round-trips its own key beside the capability's,
    /// reads as *not refused* when absent or unreadable, and goes when deleted.
    #[test]
    fn the_refusal_record_round_trips_and_an_unreadable_one_is_not_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = test_store(dir.path());
        assert!(load_refusal(&store).is_none(), "absent → not refused");

        persist_capability(&store, &cap());
        persist_refusal(&store, &refusal());
        assert_eq!(load_refusal(&store), Some(refusal()));
        assert!(
            load_capability(&store).is_some(),
            "the record has a key of its own; the capability is untouched"
        );

        store.set(RENEWAL_REFUSED_KEY, "not-hex-at-all!");
        assert!(load_refusal(&store).is_none(), "corrupt → not refused");

        persist_refusal(&store, &refusal());
        delete_refusal(&store);
        assert!(load_refusal(&store).is_none());
    }

    /// The boot path's signed-out refusal drops the refusal record with the
    /// capability it described.
    #[test]
    fn a_boot_that_refuses_a_signed_out_capability_drops_its_refusal_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = test_store(dir.path());
        persist_capability(&store, &cap().with_provisioned_at_ms(1_000));
        persist_refusal(&store, &refusal());
        write_marker(&store, [2u8; 32], 2_000);

        assert!(restore_authorized_capability(&store).is_none());
        assert!(load_refusal(&store).is_none());
    }

    /// What is news to a refusal: another nest, another key, or the latch now
    /// registered where it was not. The same evidence, or a latch that went
    /// back to unregistered, is not.
    #[test]
    fn evidence_is_news_only_when_it_was_not_refused_already() {
        let refused = refusal();
        assert!(!refused.clone().is_news_to(&refused));
        assert!(
            RefusalRecord {
                nest_url: "https://other.example".into(),
                ..refused.clone()
            }
            .is_news_to(&refused)
        );
        assert!(
            RefusalRecord {
                principal_key: hex::encode([0xd4u8; 32]),
                ..refused.clone()
            }
            .is_news_to(&refused)
        );
        let registered = RefusalRecord {
            grant_registered: true,
            ..refused.clone()
        };
        assert!(registered.is_news_to(&refused));
        assert!(!refused.is_news_to(&registered));
    }

    /// The other direction, and the one that makes the reconcile safe to run
    /// automatically: the user signed back in, so the re-provision outranks the
    /// marker its own sign-out left behind. Without this the reconcile would
    /// silently stop sync for a signed-in user — the same breach, inverted.
    #[test]
    fn a_boot_resumes_a_capability_provisioned_after_the_sign_out() {
        let dir = std::env::temp_dir().join(format!("sync-agent-signedin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = test_store(&dir);

        write_marker(&store, [2u8; 32], 2_000);
        persist_capability(&store, &cap().with_provisioned_at_ms(3_000));

        let restored = restore_authorized_capability(&store)
            .expect("a re-provision after the sign-out must be resumed");
        assert_eq!(restored.bearer.token, "tok-1");
        assert!(
            load_capability(&store).is_some(),
            "a live capability's record survives the stale marker"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The provision handler rebuilds the capability field by field**, so a
    /// field it does not name is dropped on every provision *and reconnect*.
    /// Dropping this one is uniquely nasty: an un-stamped capability reads as
    /// provisioned at time 0, so the reconcile would revoke a freshly-provisioned
    /// capability against the marker its own previous sign-out left behind —
    /// silently stopping a signed-in user's sync, the original breach inverted.
    ///
    /// Asserts the full round trip a real sign-in takes: the app's stamped
    /// capability survives `ProvisionCapability` into the persisted record, and
    /// the boot path then resumes it despite the older marker.
    #[tokio::test]
    async fn the_provision_stamp_survives_the_handlers_field_by_field_rebuild() {
        use fauna_ipc::sync::{Request, RequestMethod};

        let dir = std::env::temp_dir().join(format!("sync-agent-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = std::sync::Arc::new(test_store(&dir));
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = crate::state::SyncServiceState::new_with_credentials(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(Some(dir.clone())),
            Some(store.clone()),
        );

        // A stale marker from an earlier sign-out, then a fresh sign-in.
        write_marker(&store, [2u8; 32], 2_000);
        let resp = crate::pipe_server::handle_request(
            &Request {
                id: 1,
                method: RequestMethod::ProvisionCapability(cap().with_provisioned_at_ms(3_000)),
            },
            &state,
        )
        .await;
        assert!(matches!(
            resp.result,
            fauna_ipc::sync::ResponseResult::Ok(_)
        ));

        assert_eq!(
            load_capability(&store).map(|c| c.provisioned_at_ms),
            Some(3_000),
            "the provision handler must carry the stamp into the persisted record"
        );
        assert!(
            restore_authorized_capability(&store).is_some(),
            "a sign-in after the marker must survive the next boot"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A corrupt stored record reads as absent (fail-open-to-empty), never a
    /// panic — the app's convergence loop re-provisions.
    #[test]
    fn corrupt_record_reads_as_absent() {
        let dir = std::env::temp_dir().join(format!("sync-agent-cred-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = test_store(&dir);
        store.set(CAPABILITY_KEY, "not-hex-at-all!");
        assert!(load_capability(&store).is_none(), "corrupt record → absent");
        store.set(CAPABILITY_KEY, &hex::encode([0u8; 4]));
        assert!(
            load_capability(&store).is_none(),
            "hex-but-not-cbor record → absent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
