//! The page's text, composed once (`docs/goal/ui/connected-apps.md` § Where
//! logic lives): a roster row's lines and a request card's lines, as
//! [`ComposedText`] — an i18n key plus arguments, some of which are themselves
//! translatable — so each app resolves them through its own localization
//! pipeline and paints them, and no app writes the wording, the order or the
//! joining punctuation again.
//!
//! Times are formatted by a `Fn(i64) -> String` the caller supplies
//! ([`roster_row_text_with`], [`oracle_lines_with`]): native Rust and the
//! UniFFI exports pass the shared `format_unix_local_ms` (the `local-clock`
//! feature's [`roster_row_text`] / [`oracle_lines`] defaults), while web passes
//! its JS `localTimestamp` — wasm has no OS timezone database. What crosses to
//! an app is therefore plain keys and strings, with no closure to carry over a
//! UniFFI boundary.

use std::collections::HashMap;

use fauna_atproto_settings_machine::{ConsentCardRow, ConsentSetRow};
use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

use crate::snapshots::{ConnectedAppRow, OracleSummaryRow, VERBATIM_KEY, class};

/// What a row's facts line puts between its facts.
pub const FACTS_SEPARATOR: &str = " · ";

/// What a composed text's lines are joined with when one element carries them
/// all (a tui/linux item's declared text).
pub const LINE_SEPARATOR: &str = "\n";

/// A [`LocalizedText`] whose template arguments may themselves be
/// translatable: `nested` maps an argument name to the text that fills it
/// (a resolved app name, a described scope, the oracle's operation words),
/// and wins over `text.args` of the same name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ComposedText {
    pub text: LocalizedText,
    pub nested: HashMap<String, LocalizedText>,
}

impl ComposedText {
    fn key(key: &str) -> Self {
        Self {
            text: LocalizedText::key(key),
            nested: HashMap::new(),
        }
    }

    fn args<const N: usize>(key: &str, pairs: [(&str, &str); N]) -> Self {
        Self {
            text: LocalizedText::key_args(key, pairs),
            nested: HashMap::new(),
        }
    }

    fn with_nested<const N: usize>(
        key: &str,
        pairs: [(&str, &str); N],
        nested: impl IntoIterator<Item = (&'static str, LocalizedText)>,
    ) -> Self {
        Self {
            text: LocalizedText::key_args(key, pairs),
            nested: nested
                .into_iter()
                .map(|(n, t)| (n.to_string(), t))
                .collect(),
        }
    }

    fn bullet(text: LocalizedText) -> Self {
        Self::with_nested(BULLET, [], [("text", text)])
    }

    fn bullet_verbatim(text: &str) -> Self {
        Self::args(BULLET, [("text", text)])
    }

    /// Resolve through `lookup` (the app's key → template function): each
    /// nested text first, then this template with them substituted.
    pub fn resolve<F, S>(&self, lookup: F) -> String
    where
        F: Fn(&str) -> Option<S>,
        S: AsRef<str>,
    {
        let mut text = self.text.clone();
        for (name, nested) in &self.nested {
            text.args
                .insert(name.clone(), nested.resolve(|k| lookup(k)));
        }
        text.resolve(lookup)
    }
}

/// A roster row's text: `lines` top to bottom (the head, any burned state,
/// the publisher, the scopes, the permission sets, the oracle summary), then
/// the `facts` the app joins with [`FACTS_SEPARATOR`] as the last line —
/// [`RosterRowText::resolve_lines`] does both for a Rust app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RosterRowText {
    pub lines: Vec<ComposedText>,
    pub facts: Vec<ComposedText>,
}

impl RosterRowText {
    /// Every line resolved, the facts joined into the last one.
    pub fn resolve_lines<F, S>(&self, lookup: F) -> Vec<String>
    where
        F: Fn(&str) -> Option<S>,
        S: AsRef<str>,
    {
        let mut out: Vec<String> = self.lines.iter().map(|l| l.resolve(&lookup)).collect();
        out.push(
            self.facts
                .iter()
                .map(|f| f.resolve(&lookup))
                .collect::<Vec<_>>()
                .join(FACTS_SEPARATOR),
        );
        out
    }
}

const VERBATIM: &str = VERBATIM_KEY;
const BULLET: &str = "connected_apps.bullet";
const INDENTED: &str = "connected_apps.indented";

/// The class badge's key — the one half of the grouping key an app used to
/// carry. An unknown class has no badge rather than a guess.
pub fn class_label_key(class_value: &str) -> Option<&'static str> {
    Some(match class_value {
        class::REMOTE => "connected_apps.class_remote",
        class::DEVICE => "connected_apps.class_device",
        class::WASM => "connected_apps.class_wasm",
        class::CONTAINER => "connected_apps.class_container",
        class::APP_PASSWORD => "connected_apps.class_app_password",
        class::SIGNER => "connected_apps.class_signer",
        class::OAUTH => "connected_apps.class_oauth",
        _ => return None,
    })
}

/// One permission set's lines, shared by the consent card and the roster row:
/// who decided the bundle (the NSID, verbatim) and its title, what its
/// publisher says it is for, then its members in words.
pub fn set_lines(set: &ConsentSetRow) -> Vec<ComposedText> {
    let mut lines = vec![match set.title.as_deref() {
        Some(title) => ComposedText::args(
            "atproto_settings.consent_set_heading",
            [("title", title), ("nsid", &set.nsid)],
        ),
        None => ComposedText::args(
            "atproto_settings.consent_set_heading_unnamed",
            [("nsid", &set.nsid)],
        ),
    }];
    lines.extend(
        set.details
            .as_deref()
            .map(|d| ComposedText::args(INDENTED, [("text", d)])),
    );
    lines.extend(
        set.member_descriptions
            .iter()
            .map(|m| ComposedText::bullet_verbatim(m)),
    );
    lines
}

/// The oracle summary on a served principal's row (`connected-apps.md`
/// § Layout & flow, region 3): the operations in the current window and every
/// request answered, then the newest operation — its class in the consent
/// card's words, its detail verbatim.
///
/// [`oracle_lines_with`] with the shared local-clock formatter.
#[cfg(feature = "local-clock")]
pub fn oracle_lines(oracle: &OracleSummaryRow) -> Vec<ComposedText> {
    oracle_lines_with(oracle, &fauna_core::format::format_unix_local_ms)
}

/// [`oracle_lines`] with the caller's time formatter (unix millis → text).
pub fn oracle_lines_with(
    oracle: &OracleSummaryRow,
    format_time: &dyn Fn(i64) -> String,
) -> Vec<ComposedText> {
    let ops = match oracle.ops_in_window {
        1 => LocalizedText::key("connected_apps.oracle_ops_one"),
        n => LocalizedText::key_arg("connected_apps.oracle_ops", "count", n.to_string()),
    };
    let requests = match oracle.requests_served {
        1 => LocalizedText::key("connected_apps.oracle_requests_one"),
        n => LocalizedText::key_arg("connected_apps.oracle_requests", "count", n.to_string()),
    };
    let mut lines = vec![ComposedText::with_nested(
        "connected_apps.oracle_summary",
        [],
        [("ops", ops), ("requests", requests)],
    )];
    lines.extend(oracle.last_op.as_ref().map(|op| {
        let time = format_time(op.at_millis);
        // The detail is a verbatim carrier (`machine::verbatim`): blank when it
        // has no key or its one `text` argument is whitespace.
        let bare = op.detail.key.is_empty()
            || (op.detail.key == VERBATIM
                && op
                    .detail
                    .args
                    .get("text")
                    .is_none_or(|t| t.trim().is_empty()));
        let key = if bare {
            "connected_apps.oracle_latest_bare"
        } else {
            "connected_apps.oracle_latest"
        };
        let mut nested = vec![("operation", op.words.clone())];
        if !bare {
            nested.push(("detail", op.detail.clone()));
        }
        ComposedText::with_nested(key, [("time", time.as_str())], nested)
    }));
    lines
}

/// One roster row's text (see [`RosterRowText`]): [`roster_row_text_with`]
/// with the shared local-clock formatter.
#[cfg(feature = "local-clock")]
pub fn roster_row_text(row: &ConnectedAppRow) -> RosterRowText {
    roster_row_text_with(row, &fauna_core::format::format_unix_local_ms)
}

/// [`roster_row_text`] with the caller's time formatter (unix millis → text).
pub fn roster_row_text_with(
    row: &ConnectedAppRow,
    format_time: &dyn Fn(i64) -> String,
) -> RosterRowText {
    let mut head =
        ComposedText::with_nested("connected_apps.row_head", [], [("name", row.name.clone())]);
    match class_label_key(&row.class) {
        Some(badge) => {
            head.nested
                .insert("badge".into(), LocalizedText::key(badge));
        }
        None => head = ComposedText::with_nested(VERBATIM, [], [("text", row.name.clone())]),
    }
    let mut lines = vec![head];
    // The burned state sits directly under the name and above everything a
    // user might copy into a mail app: the login below it authenticates
    // nothing any more (`mail-credentials.md` § Rotation and recovery →
    // *Succession*). A signer client the burn killed says the same in its
    // own words (`nostr.md` § Key succession and rotation).
    if row.mail.as_ref().is_some_and(|m| m.revoked) {
        lines.push(ComposedText::key("settings.mail.credential_revoked"));
    } else if row.signer_burned {
        lines.push(ComposedText::key("connected_apps.signer_revoked"));
    }
    if let (Some(client_id), Some(domain)) = (&row.client_id, &row.publisher) {
        lines.push(ComposedText::with_nested(
            "connected_apps.publisher_line",
            [("client_id", client_id.as_str())],
            [(
                "publisher",
                LocalizedText::key_arg("connected_apps.publisher", "domain", domain),
            )],
        ));
    }
    lines.extend(
        row.scope_descriptions
            .iter()
            .cloned()
            .map(ComposedText::bullet),
    );
    // The permission sets those permissions came from, after the list and in
    // the card's own lines, so the row reads as the approval did
    // (`atproto-oauth-provider.md` § Implementation status today).
    lines.extend(row.sets.iter().flat_map(set_lines));
    if let Some(oracle) = &row.oracle {
        lines.extend(oracle_lines_with(oracle, format_time));
    }

    let mut facts = Vec::new();
    // A paused row says why it cannot act and how to undo it; "not
    // connected" is only for a row nothing but itself holds back.
    if row.paused {
        facts.push(ComposedText::key("connected_apps.paused"));
    } else if !row.connected {
        facts.push(ComposedText::key("connected_apps.not_connected"));
    }
    facts.push(ComposedText::args(
        "connected_apps.created",
        [("time", &format_time(row.created_at_millis))],
    ));
    facts.push(match row.last_used_at_millis {
        Some(ms) => ComposedText::args("connected_apps.last_used", [("time", &format_time(ms))]),
        None => ComposedText::key("connected_apps.never_used"),
    });
    facts.push(match row.lasts_until_millis {
        Some(ms) => ComposedText::args("connected_apps.lasts_until", [("time", &format_time(ms))]),
        None => ComposedText::key("connected_apps.open_ended"),
    });
    RosterRowText { lines, facts }
}

/// A request card's lines, top to bottom: the heading, who is asking, the
/// scopes heading and one bulleted line per scope, the permission sets, then
/// — last — what the approve ends besides granting (a key replacement's line,
/// `ConsentCardRow.ends`; absent on an ordinary card).
pub fn request_card_lines(consent: &ConsentCardRow) -> Vec<ComposedText> {
    let who = match &consent.client_name {
        Some(name) => ComposedText::args(
            "atproto_settings.consent_client",
            [("name", name.as_str()), ("client_id", &consent.client_id)],
        ),
        None => ComposedText::args(
            "atproto_settings.consent_client_unnamed",
            [("client_id", &consent.client_id)],
        ),
    };
    let mut lines = vec![
        ComposedText::key("atproto_settings.consent_heading"),
        who,
        ComposedText::key("atproto_settings.consent_scopes_heading"),
    ];
    lines.extend(
        consent
            .scope_descriptions
            .iter()
            .map(|s| ComposedText::bullet_verbatim(s)),
    );
    lines.extend(consent.sets.iter().flat_map(set_lines));
    lines.extend(consent.ends.as_ref().map(|e| ComposedText {
        text: e.clone(),
        nested: HashMap::new(),
    }));
    lines
}

#[cfg(all(test, feature = "local-clock"))]
mod tests;
