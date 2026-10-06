//! Shared orchestration for the admin `admin-aliases` page — the **external
//! forwarder** half (`mail-aliases.md` § Kind 7): an address on a local domain
//! with no local mailbox that forwards inbound to an external destination,
//! attributed to the managing admin actor.
//!
//! Authority for behavior: `docs/goal/behavior/mail-aliases.md` § Kind 7 +
//! § Wire shapes (`create_forwarder` / `list_forwarders` / `delete_forwarder`,
//! all Admin-class) and `docs/goal/behavior/mail-forwarding.md` § Admin external
//! forwarders (the forward *dispatch*). Authority for UX/IDs:
//! `docs/goal/behavior/admin.md` § 4 + `tests/e2e-unified/ui.yaml`
//! `admin-aliases` (the `admin-aliases-forwarder-*` IDs).
//!
//! Per priority #2/#4, the snapshot projection + action sequencing (the
//! `<pattern>@<local_domain>` address render, hex-encoding the alias id for the
//! delete action, re-reading the list after a mutation, supplying the hosted
//! local-domain list for the add-form picker) live here — not in any per-app
//! shell. The UI renders [`ForwardersSnapshot`] and dispatches
//! [`ForwarderAction`]; the per-app glue implements one WS-RPC seam
//! ([`ForwarderNest`]) over `MailAdminClient` (`libs/fauna-client-bridges`:
//! `list_forwarders`, `create_forwarder`, `delete_forwarder`, `list_local_domains`).
//! Mirrors `bridge_approval.rs` / `local_domains.rs`.
//!
//! Catch-all designation (the page's other admin-tier alias concern, `admin.md`
//! § 4) is a **deferred** slice: it needs a `fauna.bridges.set_catch_all_actor`
//! setter RPC that does not exist yet (catch-all is settable only at
//! `add_local_domain` time today). It folds in here when that lands.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_protocol::MaybeSendSync;
use fauna_protocol::bridge_routing::AliasRow;
use serde::{Deserialize, Serialize};

use crate::error::{DispatchError, NestError};

/// One external forwarder as the `admin-aliases-forwarder-list` renders it.
/// Projected from the wire [`AliasRow`] (`kind == "forwarder"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ForwarderView {
    /// Lowercase hex of the forwarder's 16-byte alias id. Carried verbatim into
    /// the delete action (the machine hex-decodes it for the wire) — the UI
    /// never needs the raw bytes.
    pub alias_id_hex: String,
    /// The local domain the forwarder lives on (`admin-aliases-forwarder-row`
    /// scope key).
    pub local_domain: String,
    /// The local-part pattern (`info`).
    pub pattern: String,
    /// The full source address (`<pattern>@<local_domain>`) —
    /// `admin-aliases-forwarder-row-address`.
    pub address: String,
    /// The external destination (`admin-aliases-forwarder-row-target`). Always
    /// `Some` on a real forwarder row; defensively `String::new()` if the wire
    /// ever omitted it (a non-forwarder row shouldn't reach this list).
    pub forward_target: String,
}

impl From<AliasRow> for ForwarderView {
    fn from(row: AliasRow) -> Self {
        let address = format!("{}@{}", row.pattern, row.local_domain);
        Self {
            alias_id_hex: hex::encode(&row.alias_id),
            local_domain: row.local_domain,
            pattern: row.pattern,
            address,
            forward_target: row.forward_target.unwrap_or_default(),
        }
    }
}

/// Coarse machine status for spinner / disabled-control rendering. Mirrors
/// `BridgeApprovalStatus` / `LocalDomainStatus`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ForwarderStatus {
    Idle,
    Loading,
    Working,
}

/// Read-only snapshot the per-app UI renders for `admin-aliases` (forwarder
/// half).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ForwardersSnapshot {
    /// The deployment's external forwarders, each rendering one
    /// `admin-aliases-forwarder-list` row.
    pub forwarders: Vec<ForwarderView>,
    /// Active hosted local domains (`mail_domains` non-soft-deleted) — the
    /// options for the add-form domain picker
    /// (`admin-aliases-forwarder-add-domain-select`). A forwarder's
    /// `local_domain` must be one of these.
    pub local_domains: Vec<String>,
    pub status: ForwarderStatus,
    /// Last action's error, surfaced via `admin-aliases-action-error`
    /// (`conflicts_with_existing_alias` / `reserved_local_part` /
    /// `validate_forward_target` failures, etc.).
    pub error: Option<String>,
}

impl ForwardersSnapshot {
    fn empty() -> Self {
        Self {
            forwarders: Vec::new(),
            local_domains: Vec::new(),
            status: ForwarderStatus::Idle,
            error: None,
        }
    }
}

/// Actions the per-app UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ForwarderAction {
    /// Re-read the forwarder list + the hosted-domain list (page load / after a
    /// mutation).
    Refresh,
    /// Create an external forwarder. `local_domain` must be an active hosted
    /// domain; the nest validates `forward_target` (RFC-5321 + must-not-be-a-
    /// hosted-domain), the reserved-local-part rule, and the exact↔forwarder
    /// collision.
    Create {
        local_domain: String,
        pattern: String,
        forward_target: String,
    },
    /// Delete a forwarder by its hex alias id (from a rendered [`ForwarderView`]).
    Delete { alias_id_hex: String },
}

/// WS-RPC seam to nest. Per-app glue implements this over `MailAdminClient`
/// (`libs/fauna-client-bridges`) — each method a 1:1 forward.
// Dual `async_trait` arm + `MaybeSendSync` supertrait so the one seam serves
// native + wasm (see `fauna_protocol::MaybeSendSync`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ForwarderNest: MaybeSendSync {
    /// `fauna.bridges.list_forwarders` — every `kind='forwarder'` row.
    async fn list_forwarders(&self) -> Result<Vec<AliasRow>, NestError>;
    /// Active hosted local-domain names (`mail_domains` non-soft-deleted) for
    /// the add-form picker. Glue maps `fauna.bridges.list_local_domains`'s active
    /// rows to their `domain_name`.
    async fn list_local_domains(&self) -> Result<Vec<String>, NestError>;
    /// `fauna.bridges.create_forwarder`.
    async fn create_forwarder(
        &self,
        local_domain: String,
        pattern: String,
        forward_target: String,
    ) -> Result<(), NestError>;
    /// `fauna.bridges.delete_forwarder` — `alias_id` is the 16-byte wire form
    /// (the machine hex-decodes the row's string id).
    async fn delete_forwarder(&self, alias_id: Vec<u8>) -> Result<(), NestError>;
}

/// Decode a row's hex alias id to the 16-byte wire form. A corrupted snapshot
/// shouldn't crash the dispatch — a malformed / wrong-length string surfaces as
/// a user-visible error instead of panicking or sending garbage to nest.
fn decode_alias_id(alias_id_hex: &str) -> Result<Vec<u8>, DispatchError> {
    crate::error::decode_hex_id16("forwarder alias id", alias_id_hex)
}

/// One instance per admin client. Holds the rendered snapshot; drives the seam.
/// Mirrors `BridgeApprovalMachine` / `LocalDomainMachine`.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct ForwarderMachine {
    nest: Arc<dyn ForwarderNest>,
    inner: Mutex<ForwardersSnapshot>,
}

// `new` takes `Arc<dyn ForwarderNest>` (not an FFI type), so it stays in a plain
// (non-exported) impl alongside the private helpers. The FFI surface — `snapshot`
// (sync) + `hydrate`/`dispatch` (async) — lives in the exported impl blocks
// below (mirrors `bridge_approval.rs`).
impl ForwarderMachine {
    pub fn new(nest: Arc<dyn ForwarderNest>) -> Self {
        Self {
            nest,
            inner: Mutex::new(ForwardersSnapshot::empty()),
        }
    }

    fn set_status(&self, status: ForwarderStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(ForwarderStatus::Loading);
        // Both reads before taking the lock (no .await while holding it).
        let forwarders = self.nest.list_forwarders().await?;
        let local_domains = self.nest.list_local_domains().await?;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.forwarders = forwarders.into_iter().map(ForwarderView::from).collect();
        snap.local_domains = local_domains;
        snap.status = ForwarderStatus::Idle;
        Ok(())
    }

    async fn create(
        &self,
        local_domain: String,
        pattern: String,
        forward_target: String,
    ) -> Result<(), DispatchError> {
        self.set_status(ForwarderStatus::Working);
        self.nest
            .create_forwarder(local_domain, pattern, forward_target)
            .await?;
        // Re-read so the new forwarder appears in the list.
        self.refresh().await
    }

    async fn delete(&self, alias_id_hex: String) -> Result<(), DispatchError> {
        self.set_status(ForwarderStatus::Working);
        let alias_id = decode_alias_id(&alias_id_hex)?;
        self.nest.delete_forwarder(alias_id).await?;
        // Re-read so the deleted forwarder drops out of the list.
        self.refresh().await
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl ForwarderMachine {
    pub fn snapshot(&self) -> ForwardersSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl ForwarderMachine {
    /// Initial page load.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    pub async fn dispatch(&self, action: ForwarderAction) -> Result<(), DispatchError> {
        // Clear any prior error before the new action runs.
        self.inner.lock().expect("snapshot mutex").error = None;
        crate::dispatch_capturing_error!(
            self,
            ForwarderStatus,
            match action {
                ForwarderAction::Refresh => self.refresh().await,
                ForwarderAction::Create {
                    local_domain,
                    pattern,
                    forward_target,
                } => self.create(local_domain, pattern, forward_target).await,
                ForwarderAction::Delete { alias_id_hex } => self.delete(alias_id_hex).await,
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::ByteBuf;
    use std::sync::Mutex as StdMutex;

    /// In-memory nest modelling the forwarder table: `list_forwarders` returns
    /// the rows; `create` inserts (refusing a `(local_domain, pattern)` collision
    /// and a hosted-domain target); `delete` removes by id.
    #[derive(Default)]
    struct FakeNest {
        forwarders: StdMutex<Vec<AliasRow>>,
        domains: StdMutex<Vec<String>>,
        next_id: StdMutex<u8>,
    }

    fn forwarder_row(id: u8, domain: &str, pattern: &str, target: &str) -> AliasRow {
        AliasRow {
            alias_id: ByteBuf::from(vec![id; 16]),
            actor_id: ByteBuf::from(vec![0u8; 32]),
            local_domain: domain.into(),
            kind: "forwarder".into(),
            pattern: pattern.into(),
            forward_target: Some(target.into()),
            created_at: 1_700_000_000_000,
            ..Default::default()
        }
    }

    #[async_trait]
    impl ForwarderNest for FakeNest {
        async fn list_forwarders(&self) -> Result<Vec<AliasRow>, NestError> {
            Ok(self.forwarders.lock().unwrap().clone())
        }

        async fn list_local_domains(&self) -> Result<Vec<String>, NestError> {
            Ok(self.domains.lock().unwrap().clone())
        }

        async fn create_forwarder(
            &self,
            local_domain: String,
            pattern: String,
            forward_target: String,
        ) -> Result<(), NestError> {
            if forward_target.ends_with("@example.com")
                && self
                    .domains
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|d| d == "example.com")
            {
                // Target is a hosted domain — that's an alias, not a forward.
                return Err(NestError::Rejected("validate_forward_target".into()));
            }
            let mut fws = self.forwarders.lock().unwrap();
            if fws
                .iter()
                .any(|f| f.local_domain == local_domain && f.pattern == pattern)
            {
                return Err(NestError::Rejected("conflicts_with_existing_alias".into()));
            }
            let mut id = self.next_id.lock().unwrap();
            *id += 1;
            fws.push(forwarder_row(*id, &local_domain, &pattern, &forward_target));
            Ok(())
        }

        async fn delete_forwarder(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            let mut fws = self.forwarders.lock().unwrap();
            let before = fws.len();
            fws.retain(|f| f.alias_id.as_ref() != alias_id.as_slice());
            if fws.len() == before {
                return Err(NestError::Rejected("fauna.bridges.not_found".into()));
            }
            Ok(())
        }
    }

    fn machine_with(forwarders: Vec<AliasRow>, domains: Vec<&str>) -> ForwarderMachine {
        ForwarderMachine::new(Arc::new(FakeNest {
            forwarders: StdMutex::new(forwarders),
            domains: StdMutex::new(domains.into_iter().map(String::from).collect()),
            next_id: StdMutex::new(0x10),
        }))
    }

    #[tokio::test]
    async fn refresh_projects_rows_and_domains() {
        let m = machine_with(
            vec![forwarder_row(
                0xAB,
                "example.com",
                "info",
                "oldaccount@example.com",
            )],
            vec!["example.com", "other.test"],
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.forwarders.len(), 1);
        let f = &snap.forwarders[0];
        assert_eq!(f.address, "info@example.com");
        assert_eq!(f.forward_target, "oldaccount@example.com");
        assert_eq!(f.alias_id_hex, hex::encode([0xABu8; 16]));
        assert_eq!(snap.local_domains, vec!["example.com", "other.test"]);
        assert_eq!(snap.status, ForwarderStatus::Idle);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn create_adds_and_relists() {
        let m = machine_with(vec![], vec!["example.com"]);
        m.hydrate().await.unwrap();
        m.dispatch(ForwarderAction::Create {
            local_domain: "example.com".into(),
            pattern: "sales".into(),
            forward_target: "team@offsite.example".into(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.forwarders.len(), 1);
        assert_eq!(snap.forwarders[0].address, "sales@example.com");
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn create_collision_surfaces_error_and_keeps_list() {
        let m = machine_with(
            vec![forwarder_row(0x01, "example.com", "info", "a@b.example")],
            vec!["example.com"],
        );
        m.hydrate().await.unwrap();
        let err = m
            .dispatch(ForwarderAction::Create {
                local_domain: "example.com".into(),
                pattern: "info".into(),
                forward_target: "c@d.example".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)));
        let snap = m.snapshot();
        assert!(
            snap.error
                .as_deref()
                .unwrap()
                .contains("conflicts_with_existing_alias"),
            "error: {:?}",
            snap.error
        );
        assert_eq!(snap.forwarders.len(), 1, "list untouched");
        assert_eq!(snap.status, ForwarderStatus::Idle);
    }

    #[tokio::test]
    async fn delete_removes_from_list() {
        let m = machine_with(
            vec![forwarder_row(0x22, "example.com", "info", "a@b.example")],
            vec!["example.com"],
        );
        m.hydrate().await.unwrap();
        let id_hex = m.snapshot().forwarders[0].alias_id_hex.clone();
        m.dispatch(ForwarderAction::Delete {
            alias_id_hex: id_hex,
        })
        .await
        .unwrap();
        assert!(m.snapshot().forwarders.is_empty());
    }

    #[tokio::test]
    async fn delete_malformed_hex_surfaces_wrap_without_calling_nest() {
        let m = machine_with(vec![], vec![]);
        let err = m
            .dispatch(ForwarderAction::Delete {
                alias_id_hex: "zz-not-hex".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Wrap(_)), "got {err:?}");
        assert!(m.snapshot().error.is_some());
    }

    #[tokio::test]
    async fn delete_wrong_length_hex_surfaces_invalid_state() {
        let m = machine_with(vec![], vec![]);
        // Valid hex but 32 bytes — not a 16-byte alias id.
        let err = m
            .dispatch(ForwarderAction::Delete {
                alias_id_hex: hex::encode([0x66u8; 32]),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
    }
}
