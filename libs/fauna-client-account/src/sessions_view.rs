//! The Sessions page's one fold (`docs/goal/ui/sessions.md` § State & data
//! shape) — pure and wasm-clean, so all seven apps paint the same rows in the
//! same order under the same names (§ Architectural rules 1: "one fold, seven
//! renders — no app decides which row is its own, how rows sort, or what a row
//! is called").
//!
//! Inputs, all handed in: the `fauna.sessions.list` reply, this process's own
//! live token ids (`fauna_protocol::auth::OwnSessionSource`), the device roster
//! reduced to `(principal, rendered name)` pairs, this machine's enrolled
//! principal, and a caller-supplied `now`. The roster arrives **already
//! rendered** because a device label is sealed and opens only under the owner's
//! keys — `fauna_devices_machine` owns that render, and this fold must not
//! reach for keys. No local cache: the page is a read-through.

use fauna_core::localized::LocalizedText;
use fauna_protocol::sessions::SessionInfo;

/// i18n keys this fold (and the page) resolve — `i18n/strings/en.yaml`
/// § `sessions`. Named here so the fold and its tests cannot drift from the
/// yaml by a typo nobody reads.
pub mod keys {
    pub const KIND_APP: &str = "sessions.kind_app";
    pub const KIND_DEVICE: &str = "sessions.kind_device";
    pub const KIND_UNKNOWN_DEVICE: &str = "sessions.kind_unknown_device";
    pub const MARK_THIS_APP: &str = "sessions.mark_this_app";
    pub const MARK_THIS_DEVICE: &str = "sessions.mark_this_device";
    pub const DETAIL: &str = "sessions.detail";
    pub const ADDRESS_NOT_RECORDED: &str = "sessions.address_not_recorded";
    pub const LOAD_FAILED: &str = "sessions.load_failed";
    pub const ACT_FAILED: &str = "sessions.act_failed";
    pub const NO_OWN_SESSION: &str = "sessions.no_own_session";
    pub const LOCKOUT_WRONG_WORD: &str = "sessions.lockout_wrong_word";
}

/// One enrolled device, as much of it as a session row needs: the principal a
/// session's `minted_by_device` joins on — **never** the roster's `device_id`
/// (`ui/sessions.md` § Don't do these) — and the name the roster already
/// rendered for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterDevice {
    pub principal: String,
    pub name: String,
}

/// Everything the fold reads.
#[derive(Debug, Clone, Copy)]
pub struct SessionsFoldInput<'a> {
    /// `fauna.sessions.list`'s rows.
    pub sessions: &'a [SessionInfo],
    /// This process's own live ids — every one folds into the single
    /// "This app" row (`behavior/devices.md` § The client's own session).
    pub own_token_ids: &'a [String],
    pub roster: &'a [RosterDevice],
    /// The principal this machine's device grant enrolled, when known — a
    /// session it minted from another process (the co-located sync agent) is
    /// marked "This device".
    pub this_device_principal: Option<&'a str>,
    /// Unix seconds. A row already expired by `now` is dropped: the list reply
    /// may be older than the paint.
    pub now: u64,
}

/// The whole page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionsSnapshot {
    pub rows: Vec<SessionRowView>,
    /// The page-level error (`error-message`), `None` when the last read and
    /// act both succeeded.
    pub error: Option<LocalizedText>,
}

/// One `session-card`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRowView {
    /// Row identity and the revoke argument. On the own row, the newest own id.
    pub token_id: String,
    /// "App sign-in" | "Device: {name}" | "A device key not in your device list".
    pub kind: LocalizedText,
    /// "This app" | "This device" — `session-this-mark-badge`.
    pub mark: Option<LocalizedText>,
    pub created_at: u64,
    pub last_used_at: u64,
    pub expires_at: u64,
    /// `None` → the detail line says the address was not recorded.
    pub ip_address: Option<String>,
    /// False exactly on this app's own row — leaving this app is Sign out.
    pub can_revoke: bool,
}

/// Where a row sorts: own app first, this device's other sessions next, then
/// everything else — each tier by last activity, newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    ThisApp,
    ThisDevice,
    Other,
}

/// Fold the inputs into the page's rows. `error` passes through untouched —
/// the fold never invents one.
pub fn fold(input: &SessionsFoldInput<'_>, error: Option<LocalizedText>) -> SessionsSnapshot {
    let live = input.sessions.iter().filter(|s| s.expires_at > input.now);
    let (own, others): (Vec<&SessionInfo>, Vec<&SessionInfo>) =
        live.partition(|s| input.own_token_ids.contains(&s.token_id));

    let mut tiered: Vec<(Tier, SessionRowView)> = Vec::with_capacity(others.len() + 1);
    // Every own id — the current one and the superseded-but-unexpired ones —
    // folds into ONE row: the newest mint names it, its activity is the latest
    // any of them saw, and it lives as long as the longest-lived.
    if let Some(newest) = own
        .iter()
        .copied()
        .max_by(|a, b| (a.created_at, &a.token_id).cmp(&(b.created_at, &b.token_id)))
    {
        let last_used_at = own.iter().map(|s| s.last_used_at).max().unwrap_or(0);
        let expires_at = own.iter().map(|s| s.expires_at).max().unwrap_or(0);
        tiered.push((
            Tier::ThisApp,
            SessionRowView {
                token_id: newest.token_id.clone(),
                kind: kind_of(newest, input.roster),
                mark: Some(LocalizedText::key(keys::MARK_THIS_APP)),
                created_at: newest.created_at,
                last_used_at,
                expires_at,
                ip_address: newest.ip_address.clone(),
                can_revoke: false,
            },
        ));
    }
    for s in others {
        let this_device = matches!(
            (s.minted_by_device.as_deref(), input.this_device_principal),
            (Some(minted), Some(mine)) if minted.eq_ignore_ascii_case(mine)
        );
        tiered.push((
            if this_device {
                Tier::ThisDevice
            } else {
                Tier::Other
            },
            SessionRowView {
                token_id: s.token_id.clone(),
                kind: kind_of(s, input.roster),
                mark: this_device.then(|| LocalizedText::key(keys::MARK_THIS_DEVICE)),
                created_at: s.created_at,
                last_used_at: s.last_used_at,
                expires_at: s.expires_at,
                ip_address: s.ip_address.clone(),
                can_revoke: true,
            },
        ));
    }
    tiered.sort_by(|(ta, a), (tb, b)| {
        ta.cmp(tb)
            .then(b.last_used_at.cmp(&a.last_used_at))
            .then(a.token_id.cmp(&b.token_id))
    });
    SessionsSnapshot {
        rows: tiered.into_iter().map(|(_, row)| row).collect(),
        error,
    }
}

/// What kind of sign-in a session is: no minting device → an app sign-in; a
/// minting principal the roster knows → that device by name; one it does not
/// (a custodian's key, a removed device's straggler) → the neutral third kind,
/// never an error (`ui/sessions.md` § Errors & edge cases).
fn kind_of(s: &SessionInfo, roster: &[RosterDevice]) -> LocalizedText {
    match s.minted_by_device.as_deref() {
        None => LocalizedText::key(keys::KIND_APP),
        Some(principal) => match roster
            .iter()
            .find(|d| d.principal.eq_ignore_ascii_case(principal))
        {
            Some(device) => LocalizedText::key_arg(keys::KIND_DEVICE, "name", device.name.clone()),
            None => LocalizedText::key(keys::KIND_UNKNOWN_DEVICE),
        },
    }
}

/// The `session-detail` line — times through the caller's formatter (native
/// apps pass the shared `fauna_core::format::format_unix_local`; web formats
/// its own), and the address, or plainly that none was recorded. The address
/// is never hidden: when the nest starts recording it the page gains it with
/// no app change (`ui/sessions.md` § Errors & edge cases).
pub fn session_detail<F, S>(
    row: &SessionRowView,
    format_time: impl Fn(u64) -> String,
    lookup: F,
) -> LocalizedText
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let address = match &row.ip_address {
        Some(ip) => ip.clone(),
        None => LocalizedText::key(keys::ADDRESS_NOT_RECORDED).resolve(&lookup),
    };
    LocalizedText::key_args(
        keys::DETAIL,
        [
            ("created", format_time(row.created_at)),
            ("last_used", format_time(row.last_used_at)),
            ("expires", format_time(row.expires_at)),
            ("address", address),
        ],
    )
}

/// A failed read → the page error, carrying the transport's own words.
pub fn load_failed(error: impl std::fmt::Display) -> LocalizedText {
    LocalizedText::key_arg(keys::LOAD_FAILED, "error", error.to_string())
}

/// A failed act → the page error, carrying the transport's own words.
pub fn act_failed(error: impl std::fmt::Display) -> LocalizedText {
    LocalizedText::key_arg(keys::ACT_FAILED, "error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;

    fn session(id: &str, created: u64, last_used: u64, minted_by: Option<&str>) -> SessionInfo {
        SessionInfo {
            token_id: id.into(),
            created_at: created,
            expires_at: created + 3_600,
            ip_address: None,
            last_used_at: last_used,
            minted_by_device: minted_by.map(str::to_string),
            extra: Default::default(),
        }
    }

    fn roster() -> Vec<RosterDevice> {
        vec![RosterDevice {
            principal: "aa".repeat(32),
            name: "Laptop".into(),
        }]
    }

    fn ids(rows: &[SessionRowView]) -> Vec<&str> {
        rows.iter().map(|r| r.token_id.as_str()).collect()
    }

    #[test]
    fn own_row_is_first_unrevocable_and_folds_every_own_id() {
        let sessions = vec![
            session("stranger", NOW - 100, NOW - 1, None),
            session("own-old", NOW - 3_000, NOW - 2_000, None),
            session("own-new", NOW - 50, NOW - 40, None),
        ];
        let own = vec!["own-old".to_string(), "own-new".to_string()];
        let snap = fold(
            &SessionsFoldInput {
                sessions: &sessions,
                own_token_ids: &own,
                roster: &[],
                this_device_principal: None,
                now: NOW,
            },
            None,
        );
        assert_eq!(ids(&snap.rows), ["own-new", "stranger"]);
        let own_row = &snap.rows[0];
        assert!(!own_row.can_revoke);
        assert_eq!(own_row.mark, Some(LocalizedText::key(keys::MARK_THIS_APP)));
        assert_eq!(own_row.last_used_at, NOW - 40);
        assert_eq!(own_row.expires_at, NOW - 50 + 3_600);
        assert!(snap.rows[1].can_revoke);
        assert_eq!(snap.rows[1].mark, None);
    }

    #[test]
    fn kind_joins_on_principal_and_names_the_device() {
        let p = "aa".repeat(32);
        let sessions = vec![
            session("direct", NOW - 10, NOW - 10, None),
            session("device", NOW - 20, NOW - 20, Some(&p)),
            session("custodian", NOW - 30, NOW - 30, Some(&"bb".repeat(32))),
        ];
        let snap = fold(
            &SessionsFoldInput {
                sessions: &sessions,
                own_token_ids: &[],
                roster: &roster(),
                this_device_principal: None,
                now: NOW,
            },
            None,
        );
        let kinds: Vec<_> = snap.rows.iter().map(|r| r.kind.clone()).collect();
        assert_eq!(
            kinds,
            [
                LocalizedText::key(keys::KIND_APP),
                LocalizedText::key_arg(keys::KIND_DEVICE, "name", "Laptop"),
                LocalizedText::key(keys::KIND_UNKNOWN_DEVICE),
            ]
        );
    }

    #[test]
    fn roster_device_id_never_joins() {
        // A minting key equal to some roster row's device_id but no principal
        // stays the neutral kind — the join key is `principal`, only.
        let roster = vec![RosterDevice {
            principal: "cc".repeat(32),
            name: "Phone".into(),
        }];
        let sessions = vec![session("x", NOW - 1, NOW - 1, Some("device-id-hex"))];
        let snap = fold(
            &SessionsFoldInput {
                sessions: &sessions,
                own_token_ids: &[],
                roster: &roster,
                this_device_principal: None,
                now: NOW,
            },
            None,
        );
        assert_eq!(
            snap.rows[0].kind,
            LocalizedText::key(keys::KIND_UNKNOWN_DEVICE)
        );
    }

    #[test]
    fn sort_is_own_then_this_device_then_rest_by_last_activity() {
        let mine = "aa".repeat(32);
        let sessions = vec![
            session("rest-old", NOW - 500, NOW - 400, None),
            session("rest-new", NOW - 500, NOW - 5, None),
            session("device-old", NOW - 500, NOW - 300, Some(&mine)),
            session("device-new", NOW - 500, NOW - 200, Some(&mine)),
            session("own", NOW - 500, NOW - 900, None),
        ];
        let own = vec!["own".to_string()];
        let snap = fold(
            &SessionsFoldInput {
                sessions: &sessions,
                own_token_ids: &own,
                roster: &roster(),
                this_device_principal: Some(&mine),
                now: NOW,
            },
            None,
        );
        assert_eq!(
            ids(&snap.rows),
            ["own", "device-new", "device-old", "rest-new", "rest-old"]
        );
        assert_eq!(
            snap.rows[1].mark,
            Some(LocalizedText::key(keys::MARK_THIS_DEVICE))
        );
        assert!(snap.rows[1].can_revoke);
    }

    #[test]
    fn expired_rows_drop_and_error_passes_through() {
        let sessions = vec![session("gone", NOW - 7_200, NOW - 7_000, None)];
        let err = load_failed("boom");
        let snap = fold(
            &SessionsFoldInput {
                sessions: &sessions,
                own_token_ids: &[],
                roster: &[],
                this_device_principal: None,
                now: NOW,
            },
            Some(err.clone()),
        );
        assert!(snap.rows.is_empty());
        assert_eq!(snap.error, Some(err));
    }

    #[test]
    fn detail_says_the_address_was_not_recorded() {
        let sessions = vec![session("x", NOW - 10, NOW - 5, None)];
        let snap = fold(
            &SessionsFoldInput {
                sessions: &sessions,
                own_token_ids: &[],
                roster: &[],
                this_device_principal: None,
                now: NOW,
            },
            None,
        );
        let lookup =
            |k: &str| (k == keys::ADDRESS_NOT_RECORDED).then(|| "not recorded".to_string());
        let detail = session_detail(&snap.rows[0], |t| t.to_string(), lookup);
        assert_eq!(detail.key, keys::DETAIL);
        assert_eq!(detail.args["address"], "not recorded");
        assert_eq!(detail.args["created"], (NOW - 10).to_string());

        let mut with_ip = snap.rows[0].clone();
        with_ip.ip_address = Some("192.0.2.7".into());
        let detail = session_detail(&with_ip, |t| t.to_string(), |_| None::<String>);
        assert_eq!(detail.args["address"], "192.0.2.7");
    }
}
