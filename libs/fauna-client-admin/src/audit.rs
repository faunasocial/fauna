//! The admin audit log on `admin-logs` — what the section renders, derived once
//! here rather than seven times (`admin.md` § The admin audit log on
//! `admin-logs`).
//!
//! Two jobs, both pure: compose each entry's row text (which admin, by handle;
//! the nest itself for a system entry; the action; what it acted on), and
//! **verify the chain over the entries fetched** — recompute every row's hash
//! from the columns it was served, under the format the row records, check
//! every `prev_hash` link, and compare against `fauna.admin.audit.integrity`'s
//! head. The recomputation is [`fauna_core::chain_version::audit_entry_hash`],
//! the very function the nest writes with, so the writer and this verifier
//! cannot drift apart field-for-field.
//!
//! The refuse rule holds here as on the nest: a row recording a hash format
//! this build does not know is counted as uncheckable, never tried against
//! another format until one fits (`fauna_core::chain_version` module docs).

use fauna_core::chain_version::{self, AuditPreimage, ChainVersion};
use fauna_core::localized::LocalizedText;
use fauna_protocol::RpcRequester;
use fauna_protocol::admin::{AdminAuditEntry, AdminAuditIntegrityReply};

use crate::{AdminClient, actor_picker_options, users_list_all};

/// Entries per `fauna.admin.audit.list` page — one screenful, and well under
/// the nest's clamp of 1000.
pub const AUDIT_PAGE_SIZE: i64 = 50;

/// One read of the section: a page of entries, the chain summary they are
/// checked against, and the handle join that names their admins.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditLogPage {
    /// `None` for the newest page (a load or a refresh); the cursor it was
    /// read from for an older one.
    pub before_id: Option<i64>,
    pub entries: Vec<AdminAuditEntry>,
    pub integrity: AdminAuditIntegrityReply,
    pub handles: Vec<(Vec<u8>, String)>,
}

/// Read one page of the section. The integrity summary is read **first**: an
/// entry written between the two reads then lands in the page but past the
/// head, where the check simply does not reach it — read the other way round,
/// a head newer than every entry shown would make the shown record look short.
///
/// A free function over [`AdminClient`] for the reason
/// [`users_list_all`] is one: the client keeps one method per kind.
pub async fn audit_log_page<R: RpcRequester>(
    client: &AdminClient<R>,
    before_id: Option<i64>,
) -> Result<AuditLogPage, R::Error> {
    let integrity = client.audit_integrity().await?;
    let entries = client
        .audit_list(Some(AUDIT_PAGE_SIZE), before_id)
        .await?
        .entries;
    let handles = actor_picker_options(&users_list_all(client).await?);
    Ok(AuditLogPage {
        before_id,
        entries,
        integrity,
        handles,
    })
}

/// What an app holds between reads: every entry fetched so far, and the
/// latest summary and handle join. The entries never leave shared Rust as
/// anything but rows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditLog {
    entries: Vec<AdminAuditEntry>,
    integrity: AdminAuditIntegrityReply,
    handles: Vec<(Vec<u8>, String)>,
}

impl AuditLog {
    /// Fold a read in: the newest page replaces what was held (a refresh), an
    /// older page adds to it.
    pub fn absorb(&mut self, page: AuditLogPage) {
        if page.before_id.is_none() {
            self.entries.clear();
        }
        self.entries.extend(page.entries);
        self.integrity = page.integrity;
        self.handles = page.handles;
    }

    pub fn view(&self) -> AuditLogView {
        audit_log_view(&self.entries, &self.integrity, &self.handles)
    }
}

/// One `admin-audit-entry` row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuditLogRow {
    /// The entry's id — the `before_id` cursor and the id an integrity break
    /// names.
    pub id: i64,
    /// When, epoch milliseconds. Each app paints it with its own absolute
    /// local-timestamp door (`value-formatting.md` § Absolute local timestamp
    /// display) ahead of [`Self::text`]; the time zone is the device's, which
    /// a wasm build cannot read in Rust.
    pub ts_ms: i64,
    /// Who did what to what.
    pub text: LocalizedText,
}

/// The chain check's verdict over the entries fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AuditIntegrity {
    /// Nothing recorded yet.
    Empty,
    /// Every entry shown recomputes and links.
    Verified { checked: u32 },
    /// Every entry this build can check recomputes and links; `uncheckable`
    /// rows record a hash format it does not know.
    Uncheckable { checked: u32, uncheckable: u32 },
    /// The oldest entry (in chain order) whose hash, link or head does not
    /// check out. A break outranks everything else.
    Broken { entry_id: i64 },
}

/// What the `admin-audit-section` renders.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuditLogView {
    /// Newest first, one per entry fetched (duplicates across pages folded).
    pub rows: Vec<AuditLogRow>,
    pub integrity: AuditIntegrity,
    /// The `admin-audit-integrity-status` line.
    pub integrity_text: LocalizedText,
    /// The `before_id` for `admin-audit-more-button`, or `None` once the whole
    /// record is shown — the button is then disabled, not clickable-and-guarded.
    pub more_before_id: Option<i64>,
}

/// Fold the fetched audit pages and the integrity summary into the section.
///
/// `entries` is every page fetched so far, in any order (each page arrives
/// newest first). `handles` is the caller's `(actor_id, handle-or-hex)` join —
/// [`crate::actor_picker_options`] over [`crate::users_list_all`], the identity
/// rule an admin picker uses (`admin.md` § 2 → *What identifies a user in an
/// admin picker*); an actor it does not name (a deleted account) renders as
/// its full hex.
pub fn audit_log_view(
    entries: &[AdminAuditEntry],
    integrity: &AdminAuditIntegrityReply,
    handles: &[(Vec<u8>, String)],
) -> AuditLogView {
    let chain = in_chain_order(entries);
    let verdict = audit_chain_check(&chain, integrity);
    let rows = chain
        .iter()
        .rev()
        .map(|e| AuditLogRow {
            id: e.id,
            ts_ms: e.ts,
            text: entry_text(e, handles),
        })
        .collect();
    let more_before_id = match chain.first() {
        Some(oldest) if (chain.len() as i64) < integrity.chain_length => Some(oldest.id),
        _ => None,
    };
    AuditLogView {
        rows,
        integrity: verdict,
        integrity_text: integrity_text(verdict),
        more_before_id,
    }
}

/// Oldest first, one per id.
fn in_chain_order(entries: &[AdminAuditEntry]) -> Vec<&AdminAuditEntry> {
    let mut chain: Vec<&AdminAuditEntry> = entries.iter().collect();
    chain.sort_by_key(|e| e.id);
    chain.dedup_by_key(|e| e.id);
    chain
}

/// Verify `chain` (oldest first, as [`in_chain_order`] leaves it): every row
/// recomputes under its recorded format, every adjacent pair links, the oldest
/// links to the genesis when the whole record is here, and the row the nest
/// names as its head carries the head hash.
fn audit_chain_check(
    chain: &[&AdminAuditEntry],
    integrity: &AdminAuditIntegrityReply,
) -> AuditIntegrity {
    if chain.is_empty() {
        return AuditIntegrity::Empty;
    }
    let whole_record = chain.len() as i64 >= integrity.chain_length;
    let (mut checked, mut uncheckable) = (0u32, 0u32);
    for (i, e) in chain.iter().enumerate() {
        let expected_prev = match i {
            0 if whole_record => Some(chain_version::audit_genesis_prev_hash()),
            0 => None,
            _ => Some(chain[i - 1].entry_hash.clone()),
        };
        if expected_prev.is_some_and(|prev| prev != e.prev_hash) {
            return AuditIntegrity::Broken { entry_id: e.id };
        }
        if e.id == integrity.head_id && e.entry_hash != integrity.head_hash {
            return AuditIntegrity::Broken { entry_id: e.id };
        }
        let Some(version) = ChainVersion::from_recorded(e.entry_hash_version) else {
            uncheckable += 1;
            continue;
        };
        let recomputed = chain_version::audit_entry_hash(
            version,
            &AuditPreimage {
                id: e.id,
                ts: e.ts,
                actor_id: e.actor_id.as_deref().map(Vec::as_slice),
                action: &e.action,
                target: e.target.as_deref(),
                detail: e.detail.as_deref(),
                prev_hash: &e.prev_hash,
            },
        );
        if recomputed != e.entry_hash {
            return AuditIntegrity::Broken { entry_id: e.id };
        }
        checked += 1;
    }
    if uncheckable > 0 {
        AuditIntegrity::Uncheckable {
            checked,
            uncheckable,
        }
    } else {
        AuditIntegrity::Verified { checked }
    }
}

fn entry_text(e: &AdminAuditEntry, handles: &[(Vec<u8>, String)]) -> LocalizedText {
    let action = ("action", e.action.clone());
    let target = e
        .target
        .as_deref()
        .filter(|t| !t.is_empty())
        .map(|t| ("target", t.to_string()));
    let who = e.actor_id.as_deref().map(|actor| {
        let actor: &[u8] = actor.as_slice();
        let label = handles
            .iter()
            .find(|(id, _)| id.as_slice() == actor)
            .map(|(_, h)| h.clone())
            .unwrap_or_else(|| fauna_core::format::hex_full(actor));
        ("who", label)
    });
    let key = match (&who, &target) {
        (Some(_), Some(_)) => "admin.logs_page.audit_entry",
        (Some(_), None) => "admin.logs_page.audit_entry_no_target",
        (None, Some(_)) => "admin.logs_page.audit_entry_system",
        (None, None) => "admin.logs_page.audit_entry_system_no_target",
    };
    LocalizedText::key_args(key, [Some(action), who, target].into_iter().flatten())
}

fn integrity_text(verdict: AuditIntegrity) -> LocalizedText {
    match verdict {
        AuditIntegrity::Empty => LocalizedText::key("admin.logs_page.audit_empty"),
        AuditIntegrity::Verified { checked } => LocalizedText::key_arg(
            "admin.logs_page.audit_verified",
            "count",
            checked.to_string(),
        ),
        AuditIntegrity::Uncheckable { uncheckable, .. } => LocalizedText::key_arg(
            "admin.logs_page.audit_uncheckable",
            "count",
            uncheckable.to_string(),
        ),
        AuditIntegrity::Broken { entry_id } => {
            LocalizedText::key_arg("admin.logs_page.audit_broken", "id", entry_id.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::ByteBuf;

    const ADMIN: [u8; 32] = [7u8; 32];

    type Row<'a> = (Option<[u8; 32]>, &'a str, Option<&'a str>);

    /// A chain written exactly as the nest writes it: `(actor, action, target)`
    /// per row, ids from 1, each linked to the last through the shared
    /// recomputation.
    fn chain(rows: &[Row<'_>]) -> Vec<AdminAuditEntry> {
        let mut prev = chain_version::audit_genesis_prev_hash();
        rows.iter()
            .enumerate()
            .map(|(i, (actor, action, target))| {
                let id = i as i64 + 1;
                let ts = 1_700_000_000_000 + id;
                let entry_hash = chain_version::audit_entry_hash(
                    ChainVersion::CURRENT,
                    &AuditPreimage {
                        id,
                        ts,
                        actor_id: actor.as_ref().map(|a| a.as_slice()),
                        action,
                        target: *target,
                        detail: None,
                        prev_hash: &prev,
                    },
                );
                let e = AdminAuditEntry {
                    id,
                    ts,
                    actor_id: actor.map(|a| ByteBuf::from(a.to_vec())),
                    action: action.to_string(),
                    target: target.map(str::to_string),
                    detail: None,
                    prev_hash: prev.clone(),
                    entry_hash: entry_hash.clone(),
                    entry_hash_version: ChainVersion::CURRENT.as_i64(),
                    ..Default::default()
                };
                prev = entry_hash;
                e
            })
            .collect()
    }

    fn three() -> Vec<AdminAuditEntry> {
        chain(&[
            (None, "admin.claim", None),
            (Some(ADMIN), "user.update", Some("bob")),
            (Some(ADMIN), "tier.create", Some("gold")),
        ])
    }

    fn head(entries: &[AdminAuditEntry]) -> AdminAuditIntegrityReply {
        let newest = entries.iter().max_by_key(|e| e.id).unwrap();
        AdminAuditIntegrityReply {
            head_id: newest.id,
            head_hash: newest.entry_hash.clone(),
            chain_length: entries.len() as i64,
            ..Default::default()
        }
    }

    fn handles() -> Vec<(Vec<u8>, String)> {
        vec![(ADMIN.to_vec(), "alice".to_string())]
    }

    #[test]
    fn a_clean_chain_verifies_and_renders_newest_first() {
        let entries = three();
        let view = audit_log_view(&entries, &head(&entries), &handles());
        assert_eq!(view.integrity, AuditIntegrity::Verified { checked: 3 });
        assert_eq!(view.integrity_text.key, "admin.logs_page.audit_verified");
        assert_eq!(view.integrity_text.args["count"], "3");
        assert_eq!(
            view.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        let newest = &view.rows[0].text;
        assert_eq!(newest.key, "admin.logs_page.audit_entry");
        assert_eq!(newest.args["who"], "alice");
        assert_eq!(newest.args["action"], "tier.create");
        assert_eq!(newest.args["target"], "gold");
        let system = &view.rows[2].text;
        assert_eq!(system.key, "admin.logs_page.audit_entry_system_no_target");
        assert!(!system.args.contains_key("who"));
        assert_eq!(view.more_before_id, None, "the whole record is shown");
    }

    /// A column changed after the fact recomputes to a different hash: the
    /// verdict names that entry, whatever the rows after it say.
    #[test]
    fn a_tampered_row_is_named() {
        let mut entries = three();
        entries[1].target = Some("mallory".to_string());
        let view = audit_log_view(&entries, &head(&entries), &handles());
        assert_eq!(view.integrity, AuditIntegrity::Broken { entry_id: 2 });
        assert_eq!(view.integrity_text.key, "admin.logs_page.audit_broken");
        assert_eq!(view.integrity_text.args["id"], "2");
    }

    /// A row whose stored hash was rewritten to match its altered columns still
    /// breaks the next row's link.
    #[test]
    fn a_rehashed_row_breaks_the_next_link() {
        let mut entries = three();
        entries[1].entry_hash = "00".repeat(32);
        let mut summary = head(&entries);
        summary.chain_length = 3;
        assert_eq!(
            audit_log_view(&entries, &summary, &handles()).integrity,
            AuditIntegrity::Broken { entry_id: 2 },
            "row 2's own hash no longer recomputes"
        );
    }

    /// The nest's head names a hash the fetched newest row does not carry: the
    /// record shown is not the record the nest certifies.
    #[test]
    fn a_head_mismatch_is_a_break() {
        let entries = three();
        let mut summary = head(&entries);
        summary.head_hash = "ff".repeat(32);
        assert_eq!(
            audit_log_view(&entries, &summary, &handles()).integrity,
            AuditIntegrity::Broken { entry_id: 3 }
        );
    }

    /// The refuse rule: a format this build does not know is counted, never
    /// tried against the format it does know — even when that would match.
    #[test]
    fn an_unknown_hash_version_is_uncheckable_not_guessed() {
        let mut entries = three();
        entries[0].entry_hash_version = 3;
        let view = audit_log_view(&entries, &head(&entries), &handles());
        assert_eq!(
            view.integrity,
            AuditIntegrity::Uncheckable {
                checked: 2,
                uncheckable: 1
            }
        );
        assert_eq!(view.integrity_text.key, "admin.logs_page.audit_uncheckable");
        assert_eq!(view.integrity_text.args["count"], "1");
    }

    /// One page of a longer record: the oldest row shown is not linked to the
    /// genesis (its predecessor is not here), and the more cursor is its id.
    /// A second page folds in, duplicates and all.
    #[test]
    fn a_partial_record_pages_back_and_folds_pages() {
        let entries = three();
        let summary = head(&entries);
        let first_page: Vec<_> = entries[1..].to_vec();
        let view = audit_log_view(&first_page, &summary, &handles());
        assert_eq!(view.integrity, AuditIntegrity::Verified { checked: 2 });
        assert_eq!(view.more_before_id, Some(2));

        let mut both = first_page.clone();
        both.extend(entries[..2].iter().cloned());
        let view = audit_log_view(&both, &summary, &handles());
        assert_eq!(view.rows.len(), 3);
        assert_eq!(view.integrity, AuditIntegrity::Verified { checked: 3 });
        assert_eq!(view.more_before_id, None);
    }

    /// The whole record is here, so its oldest row must link to the genesis.
    #[test]
    fn a_whole_record_must_start_at_the_genesis() {
        let mut entries = three();
        entries[0].prev_hash = "ab".repeat(32);
        assert_eq!(
            audit_log_view(&entries, &head(&entries), &handles()).integrity,
            AuditIntegrity::Broken { entry_id: 1 }
        );
    }

    /// An older page adds to what is held; the newest page (a refresh)
    /// replaces it.
    #[test]
    fn the_held_log_pages_back_and_refreshes() {
        let entries = three();
        let summary = head(&entries);
        let page = |before_id, entries: &[AdminAuditEntry]| AuditLogPage {
            before_id,
            entries: entries.to_vec(),
            integrity: summary.clone(),
            handles: handles(),
        };
        let mut log = AuditLog::default();
        log.absorb(page(None, &entries[2..]));
        assert_eq!(log.view().more_before_id, Some(3));
        log.absorb(page(Some(3), &entries[..2]));
        let view = log.view();
        assert_eq!(view.rows.len(), 3);
        assert_eq!(view.integrity, AuditIntegrity::Verified { checked: 3 });
        log.absorb(page(None, &entries[1..]));
        assert_eq!(log.view().rows.len(), 2, "a refresh drops the older pages");
    }

    #[test]
    fn an_unnamed_admin_renders_as_full_hex_and_empty_reads_empty() {
        let entries = three();
        let view = audit_log_view(&entries, &head(&entries), &[]);
        assert_eq!(view.rows[0].text.args["who"], hex::encode(ADMIN));

        let empty = audit_log_view(&[], &AdminAuditIntegrityReply::default(), &[]);
        assert_eq!(empty.integrity, AuditIntegrity::Empty);
        assert_eq!(empty.integrity_text.key, "admin.logs_page.audit_empty");
        assert!(empty.rows.is_empty());
        assert_eq!(empty.more_before_id, None);
    }
}
