//! The admin dashboard's one card set (`admin.md` § 1. Dashboard → *The card
//! set*, § State & data shape → `admin_dashboard_snapshot()`).
//!
//! Every app used to assemble its own cards from the dashboard's reads, so the
//! seven apps showed seven different sets. The set is ruled once and built here:
//! [`admin_dashboard_cards`] is the pure fold from the five reads to the ordered
//! card list, and [`crate::AdminClient::dashboard_snapshot`] makes the five reads
//! and folds them, so an app renders the list and chooses nothing.
//!
//! Each read is optional: a read that fails drops only its own cards, and its
//! error rides [`AdminDashboardSnapshot::error`] for the app's `error-message`.

use fauna_core::localized::LocalizedText;
use fauna_protocol::RpcRequester;
use fauna_protocol::admin::{AdminStatsReply, AdminStatusReply};
use fauna_protocol::bridge_routing::{MailHealthReply, MailHealthRequest};
use fauna_protocol::discovery::{
    NestInfoReply, NestInfoRequest, SetupStatusReply, SetupStatusRequest,
};
use fauna_protocol::node_policy::RegistrationMode;

use crate::AdminClient;

/// The template a verbatim card value (a count, the domain, the version) or a
/// tier's own name rides through — `admin.dashboard.verbatim` is `{text}`.
const VERBATIM_KEY: &str = "admin.dashboard.verbatim";

/// One `admin-stat-card`: its label and its value, each for the app to resolve.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StatCard {
    pub label: LocalizedText,
    pub value: LocalizedText,
}

/// The dashboard's cards in the ruled order, plus the first failed read's
/// detail (`None` when every read answered). A failed read has already dropped
/// its own cards from `stats`; the error is what the app shows on its
/// `error-message`, wrapped in `admin.dashboard.load_error`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct AdminDashboardSnapshot {
    pub stats: Vec<StatCard>,
    pub error: Option<String>,
}

/// The five reads the dashboard folds — each `None` when its read failed.
#[derive(Debug, Clone, Default)]
pub struct AdminDashboardReads {
    /// `fauna.admin.stats` — members, suspended, per tier, storage, connections.
    pub stats: Option<AdminStatsReply>,
    /// `fauna.admin.status` — the running version.
    pub status: Option<AdminStatusReply>,
    /// `fauna.setup.status` — secure connections and the registration mode.
    pub setup: Option<SetupStatusReply>,
    /// `fauna.nest.info` — the nest's domain.
    pub nest_info: Option<NestInfoReply>,
    /// `fauna.bridges.mail_health` — the mail state.
    pub mail_health: Option<MailHealthReply>,
}

fn verbatim(text: impl Into<String>) -> LocalizedText {
    LocalizedText::key_arg(VERBATIM_KEY, "text", text)
}

fn card(label: &str, value: LocalizedText) -> StatCard {
    StatCard {
        label: LocalizedText::key(label),
        value,
    }
}

fn bytes(n: i64) -> LocalizedText {
    // The wire's byte counters are non-negative by construction; a display
    // value is the wrong place to surface a violation.
    fauna_core::format::byte_size(n.max(0) as u64)
}

/// The registration card's value: the shared picker's own label for the mode,
/// so the three modes read as three. A mode this build does not know renders
/// as its wire token, read-only — never guessed into one of the three.
fn registration_value(mode: &str) -> LocalizedText {
    match RegistrationMode::from_wire_str(mode) {
        Some(m) => crate::registration_mode_options()
            .into_iter()
            .find(|o| o.value == m.as_wire_str())
            .map(|o| o.label)
            .unwrap_or_else(|| verbatim(mode)),
        None => verbatim(mode),
    }
}

/// The ruled card set, in order: members, suspended members, one card per tier
/// (the tier's name its label), total storage with its inbox and files parts,
/// live connections, mail, secure connections, the domain, the version and
/// registration. A read that is `None` contributes no card; a card with no
/// wire value behind it (a nest that reports no registration mode) is omitted
/// rather than shown as a dash.
pub fn admin_dashboard_cards(reads: &AdminDashboardReads) -> Vec<StatCard> {
    let mut cards = Vec::new();
    if let Some(s) = &reads.stats {
        cards.push(card(
            "admin.view.registered_users",
            verbatim(s.total_users.to_string()),
        ));
        cards.push(card(
            "admin.dashboard.suspended",
            verbatim(s.suspended_users.to_string()),
        ));
        for (tier, count) in &s.users_by_tier {
            cards.push(StatCard {
                label: verbatim(tier.clone()),
                value: verbatim(count.to_string()),
            });
        }
        // The two counters are disjoint (undelivered inbox content vs. the
        // members' stored files), so neither alone is what the nest holds.
        cards.push(card(
            "admin.dashboard.total_storage",
            bytes(s.total_inbox_bytes.saturating_add(s.total_storage_bytes)),
        ));
        cards.push(card(
            "admin.dashboard.inbox_storage",
            bytes(s.total_inbox_bytes),
        ));
        cards.push(card(
            "admin.dashboard.file_storage",
            bytes(s.total_storage_bytes),
        ));
        cards.push(card(
            "admin.dashboard.connections",
            verbatim(s.ws_connections.to_string()),
        ));
    }
    if let Some(m) = &reads.mail_health {
        cards.push(card(
            "admin.dashboard.mail",
            fauna_core::format::mail_health_state_label(&m.state),
        ));
    }
    if let Some(setup) = &reads.setup {
        cards.push(card(
            "admin.dashboard.secure_connections",
            LocalizedText::key(if setup.tls_active {
                "admin.dashboard.secure_on"
            } else {
                "admin.dashboard.secure_off"
            }),
        ));
    }
    if let Some(info) = &reads.nest_info {
        cards.push(card(
            "admin.dashboard.nest_domain",
            verbatim(info.domain.clone()),
        ));
    }
    if let Some(status) = &reads.status {
        cards.push(card(
            "admin.dashboard.version",
            verbatim(status.version.clone()),
        ));
    }
    if let Some(mode) = reads
        .setup
        .as_ref()
        .and_then(|s| s.registration_mode.as_deref())
    {
        cards.push(card(
            "admin.dashboard.registration",
            registration_value(mode),
        ));
    }
    cards
}

impl<R: RpcRequester> AdminClient<R>
where
    R::Error: core::fmt::Display,
{
    /// `admin_dashboard_snapshot()` — the dashboard's five reads, each
    /// tolerated on its own, folded by [`admin_dashboard_cards`]. The first
    /// failed read's detail (prefixed with its kind) rides
    /// [`AdminDashboardSnapshot::error`]; the cards the other reads produced
    /// still render.
    pub async fn dashboard_snapshot(&self) -> AdminDashboardSnapshot {
        let mut error: Option<String> = None;
        let mut keep = |kind: &str, e: R::Error| {
            error.get_or_insert_with(|| format!("{kind}: {e}"));
        };
        let stats = self
            .stats()
            .await
            .map_err(|e| keep("fauna.admin.stats", e))
            .ok();
        let status = self
            .status()
            .await
            .map_err(|e| keep("fauna.admin.status", e))
            .ok();
        let setup = self
            .nest
            .request::<_, SetupStatusReply>("fauna.setup.status", SetupStatusRequest::default())
            .await
            .map_err(|e| keep("fauna.setup.status", e))
            .ok();
        let nest_info = self
            .nest
            .request::<_, NestInfoReply>("fauna.nest.info", NestInfoRequest::default())
            .await
            .map_err(|e| keep("fauna.nest.info", e))
            .ok();
        let mail_health = self
            .nest
            .request::<_, MailHealthReply>(
                "fauna.bridges.mail_health",
                MailHealthRequest::default(),
            )
            .await
            .map_err(|e| keep("fauna.bridges.mail_health", e))
            .ok();
        let reads = AdminDashboardReads {
            stats,
            status,
            setup,
            nest_info,
            mail_health,
        };
        AdminDashboardSnapshot {
            stats: admin_dashboard_cards(&reads),
            error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;

    fn stats() -> AdminStatsReply {
        AdminStatsReply {
            total_users: 3,
            users_by_tier: vec![("free".into(), 2), ("supporter".into(), 1)],
            suspended_users: 1,
            total_inbox_bytes: 1024,
            total_storage_bytes: 2048,
            ws_connections: 5,
            extra: Default::default(),
        }
    }

    fn setup(mode: Option<&str>) -> SetupStatusReply {
        SetupStatusReply {
            tls_active: true,
            registration_mode: mode.map(str::to_string),
            ..Default::default()
        }
    }

    fn all_reads() -> AdminDashboardReads {
        AdminDashboardReads {
            stats: Some(stats()),
            status: Some(AdminStatusReply {
                version: "1.2.3".into(),
                ..Default::default()
            }),
            setup: Some(setup(Some("invite_required"))),
            nest_info: Some(NestInfoReply {
                domain: "nest.example".into(),
                ..Default::default()
            }),
            mail_health: Some(MailHealthReply {
                state: "warming_up".into(),
                ..Default::default()
            }),
        }
    }

    // The crate carries no i18n table; the tests resolve against the few
    // templates they read (any other key renders as itself), which is enough to
    // pin each card's keys and arguments.
    fn lookup(key: &str) -> Option<&'static str> {
        match key {
            VERBATIM_KEY => Some("{text}"),
            "size.bytes" => Some("{value} B"),
            "size.kb" => Some("{value} KB"),
            _ => None,
        }
    }

    fn rendered(cards: &[StatCard]) -> Vec<(String, String)> {
        cards
            .iter()
            .map(|c| (c.label.resolve(lookup), c.value.resolve(lookup)))
            .collect()
    }

    /// The full set, in the ruled order, with each card's source value.
    #[test]
    fn the_full_set_is_in_the_ruled_order() {
        let cards = rendered(&admin_dashboard_cards(&all_reads()));
        let expected: Vec<(&str, &str)> = vec![
            ("admin.view.registered_users", "3"),
            ("admin.dashboard.suspended", "1"),
            ("free", "2"),
            ("supporter", "1"),
            ("admin.dashboard.total_storage", "3 KB"),
            ("admin.dashboard.inbox_storage", "1 KB"),
            ("admin.dashboard.file_storage", "2 KB"),
            ("admin.dashboard.connections", "5"),
            (
                "admin.dashboard.mail",
                "admin.mail_page.health_state_warming_up",
            ),
            (
                "admin.dashboard.secure_connections",
                "admin.dashboard.secure_on",
            ),
            ("admin.dashboard.nest_domain", "nest.example"),
            ("admin.dashboard.version", "1.2.3"),
            (
                "admin.dashboard.registration",
                "admin.users_page.registration_mode_invite_required",
            ),
        ];
        let expected: Vec<(String, String)> = expected
            .into_iter()
            .map(|(l, v)| (l.to_string(), v.to_string()))
            .collect();
        assert_eq!(cards, expected);
    }

    /// Total storage is inbox + files — the two counters are disjoint.
    #[test]
    fn total_storage_sums_the_two_disjoint_counters() {
        let cards = admin_dashboard_cards(&all_reads());
        let total = cards
            .iter()
            .find(|c| c.label.key == "admin.dashboard.total_storage")
            .unwrap();
        assert_eq!(total.value, fauna_core::format::byte_size(1024 + 2048));
    }

    /// Registration reads as three modes, never an open/closed pair; an
    /// unknown mode renders as its token, and an absent one drops the card.
    #[test]
    fn registration_is_three_valued() {
        let value = |mode: Option<&str>| {
            let reads = AdminDashboardReads {
                setup: Some(setup(mode)),
                ..Default::default()
            };
            admin_dashboard_cards(&reads)
                .into_iter()
                .find(|c| c.label.key == "admin.dashboard.registration")
                .map(|c| c.value.resolve(lookup))
        };
        assert_eq!(
            value(Some("open")).as_deref(),
            Some("admin.users_page.registration_mode_open")
        );
        assert_eq!(
            value(Some("invite_required")).as_deref(),
            Some("admin.users_page.registration_mode_invite_required")
        );
        assert_eq!(
            value(Some("closed")).as_deref(),
            Some("admin.users_page.registration_mode_closed")
        );
        assert_eq!(value(Some("by_lottery")).as_deref(), Some("by_lottery"));
        assert_eq!(value(None), None);
    }

    /// A failed read drops only its own cards.
    #[test]
    fn a_failed_read_drops_only_its_cards() {
        let labels = |reads: &AdminDashboardReads| -> Vec<String> {
            admin_dashboard_cards(reads)
                .into_iter()
                .map(|c| c.label.resolve(lookup))
                .collect()
        };
        let full = labels(&all_reads());

        let no_stats = labels(&AdminDashboardReads {
            stats: None,
            ..all_reads()
        });
        assert_eq!(no_stats, full[8..].to_vec(), "stats owns the first eight");

        let no_setup = labels(&AdminDashboardReads {
            setup: None,
            ..all_reads()
        });
        let mut want = full.clone();
        want.retain(|l| {
            l != "admin.dashboard.secure_connections" && l != "admin.dashboard.registration"
        });
        assert_eq!(no_setup, want);

        for (strip, gone) in [
            (0, "admin.dashboard.version"),
            (1, "admin.dashboard.nest_domain"),
            (2, "admin.dashboard.mail"),
        ] {
            let mut reads = all_reads();
            match strip {
                0 => reads.status = None,
                1 => reads.nest_info = None,
                _ => reads.mail_health = None,
            }
            let mut want = full.clone();
            want.retain(|l| l != gone);
            assert_eq!(labels(&reads), want, "dropping {gone}");
        }

        assert!(admin_dashboard_cards(&AdminDashboardReads::default()).is_empty());
    }

    /// The client makes the five reads, in order, and folds them.
    #[test]
    fn dashboard_snapshot_makes_the_five_reads() {
        fn enc<T: serde::Serialize>(reply: &Option<T>) -> Vec<u8> {
            fauna_protocol::encode_canonical(reply.as_ref().unwrap())
                .unwrap()
                .to_vec()
        }
        let all = all_reads();
        let rec = std::sync::Arc::new(fauna_client_testkit::ScriptedRequester::new([
            enc(&all.stats),
            enc(&all.status),
            enc(&all.setup),
            enc(&all.nest_info),
            enc(&all.mail_health),
        ]));
        let client = AdminClient::new(rec.clone());
        let snap = block_on(client.dashboard_snapshot());
        assert_eq!(snap.error, None);
        assert_eq!(snap.stats, admin_dashboard_cards(&all));
        assert_eq!(
            rec.kinds(),
            vec![
                "fauna.admin.stats",
                "fauna.admin.status",
                "fauna.setup.status",
                "fauna.nest.info",
                "fauna.bridges.mail_health",
            ]
        );
    }

    /// Every read failing yields no cards and the first read's error.
    #[test]
    fn dashboard_snapshot_reports_the_first_failure() {
        let client = AdminClient::new(std::sync::Arc::new(
            fauna_client_testkit::FailingRequester::new("down"),
        ));
        let snap = block_on(client.dashboard_snapshot());
        assert!(snap.stats.is_empty());
        assert_eq!(snap.error.as_deref(), Some("fauna.admin.stats: down"));
    }
}
