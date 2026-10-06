//! The Settings rail Root — the Status landing (split out of `settings/mod.rs`).

use fauna_client_status::StatusText;
use fauna_i18n::strings::{
    admin, atproto_settings, common, devices, labeler_catalog, logs, mail_aliases, mail_export,
    mail_import, mail_lists, mail_settings, mail_spam, muted_words, nests, personalization,
    settings as t, settings::encryption_page, status as status_t, subscriptions, task_delegation,
    tui_settings, web_settings,
};
use fauna_protocol::account::QuotaGetReply;
use fauna_ui_ids as ids;

// Unlike the mail rows above, whose labels come straight off `fauna_i18n`, the
// archive-import row reads the SUB-PAGE's own `TITLE` — the one const the page
// also paints as its heading, so the rail row and the page can never disagree
// about what this wizard is called.
use super::{Action, QuotaView, SettingsField, SettingsState, archive_import};
use crate::element::{Element, Field, Gesture};
use crate::wizard::localized;

/// A Settings rail row's id, `settings-nav-row[<key>]` — keyed by the ui.yaml
/// page key the row opens, or by the two-element nav slug for an entry realized
/// from the `settings` page itself (`privacy`, `general`, `encryption`; ui.yaml
/// `navigation.sub_page_nav_rows`).
fn rail_id(key: &str) -> String {
    format!("{}[{key}]", fauna_ui_ids::SETTINGS_NAV_ROW)
}

/// The rail Root — the **Status landing** (`settings.md` § Live-data placement):
/// the live account cell (`account-actor-id` + copy) and, once `fauna.quota.get`
/// resolves, `quota-section` + the three quota cells. Below them, the rail rows
/// that drill into the other sub-pages: `account-settings-link` is a real ui.yaml
/// landmark/nav (into the Account sub-page); every other row is
/// `settings-nav-row[<page key>]` ([`rail_id`]), so a test bound to gestures
/// reaches any sub-page by clicks.
/// `account-actor-id-copy-btn`: "Copy", then "Copied!" once it fired, carrying
/// the copied actor id as its `copied` attr so a test asserts the CONTENTS (OSC
/// 52 is fire-and-forget; the profile/web-settings copy-button contract).
fn actor_id_copy_button(state: &SettingsState) -> Element {
    let label = if state.actor_id_copied {
        common::COPIED
    } else {
        common::COPY
    };
    let button = Element::gesture_button(
        ids::ACCOUNT_ACTOR_ID_COPY_BTN,
        label,
        true,
        Gesture::Settings(Action::CopyActorId),
    );
    if state.actor_id_copied {
        button.attr("copied", state.account_actor_id.clone())
    } else {
        button
    }
}

/// The root page without a region section or a loaded Status snapshot — the
/// unit tests' view of it.
#[cfg(test)]
pub(super) fn root_elements(state: &SettingsState) -> Vec<Element> {
    root_page(state, Vec::new(), &StatusText::default())
}

/// The Status sections 5–8 (`ui/status.md` § Layout & flow) — Node, Sync and
/// Encryption, in that order, from the shared snapshot's text projection.
/// **A section whose leg is `None` is not painted at all**: no header, no
/// placeholder, no zero (the un-hydrated-paint rule the quota and feature-limits
/// sections follow, and what lets a driver's wait land on the real value). The
/// element text is the bare value the witness reads on every app; the human
/// label rides `labelled` (paint-only), and the section title is chrome. The
/// Build section is painted separately, after About, so the app's version and
/// its commit sit together.
fn status_sections(status: &StatusText) -> Vec<Element> {
    let mut els = Vec::new();
    if let (Some(domain), Some(version)) = (&status.node_domain, &status.node_version) {
        els.push(Element::chrome(status_t::node::TITLE));
        els.push(Element::label(ids::STATUS_NODE_DOMAIN, domain.clone()).labelled(common::DOMAIN));
        // The same "Version" string linux's node row uses, so the two apps
        // cannot label one fact two ways.
        els.push(
            Element::label(ids::STATUS_NODE_VERSION, version.clone())
                .labelled(admin::dashboard::VERSION),
        );
    }
    if let (Some(pending), Some(last)) = (&status.sync_pending, &status.sync_last) {
        els.push(Element::chrome(common::SYNC));
        els.push(
            Element::label(ids::STATUS_SYNC_PENDING, pending.clone()).labelled(common::PENDING),
        );
        els.push(
            Element::label(ids::STATUS_SYNC_LAST, last.clone()).labelled(status_t::sync::LAST_SYNC),
        );
    }
    if let (Some(key_packages), Some(channels)) = (&status.mls_key_packages, &status.mls_channels) {
        els.push(Element::chrome(encryption_page::TITLE));
        els.push(
            Element::label(ids::STATUS_MLS_KEY_PACKAGES, key_packages.clone())
                .labelled(status_t::encryption::KEY_PACKAGES),
        );
        els.push(
            Element::label(ids::STATUS_MLS_CHANNELS, channels.clone())
                .labelled(status_t::encryption::DM_CHANNELS),
        );
    }
    els
}

/// The Build section (`ui/status.md` § Build): the abbreviated commit of the
/// running build, absent on an unstamped build — never a `dev` placeholder.
fn build_section(status: &StatusText) -> Vec<Element> {
    match &status.build_sha {
        Some(sha) => vec![
            Element::chrome(status_t::build::TITLE),
            Element::label(ids::STATUS_BUILD_SHA, sha.clone()).labelled(status_t::build::COMMIT),
        ],
        None => Vec::new(),
    }
}

/// The Settings root. `region` is the region transparency section
/// (`crate::region::settings_elements` — device-scoped, so it rides in from the
/// app rather than from `SettingsState`), painted beside feature limits: the
/// two "what bounds me, and who says so" surfaces sit together. `status` is the
/// shared Status snapshot's text projection (`super::status_text` — three of
/// its legs are live app state, so it rides in the same way).
pub(super) fn root_page(
    state: &SettingsState,
    region: Vec<Element>,
    status: &StatusText,
) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        // The live account cell (from the session — no fetch).
        Element::label(ids::ACCOUNT_ACTOR_ID, state.account_actor_id.clone()),
        actor_id_copy_button(state),
    ];
    // quota-section registers ONLY after the fetch resolves — so the driver's
    // `wait_for("quota-section")` waits for real data (linux's `{#if quota}`).
    if let Some(quota) = &state.quota {
        els.push(Element::label(ids::QUOTA_SECTION, t::account_page::USAGE));
        els.push(Element::label(ids::QUOTA_INBOX, quota.inbox.clone()));
        els.push(Element::label(ids::QUOTA_STORAGE, quota.storage.clone()));
        els.push(Element::label(ids::QUOTA_DEVICES, quota.devices.clone()));
        // `quota-display` component's visual-bar pair, layered on top of the
        // `quota-storage` text field above (web's reference shape). The bar's
        // painted text is its percentage — tui has no widget-level fraction,
        // so the shared action reads this id as text (the mail-export-progress-bar
        // idiom, `mail_export.rs`); `settings-storage-text` reuses `quota.storage`
        // verbatim rather than re-deriving the same "used / max" string.
        els.push(
            Element::label(
                ids::SETTINGS_STORAGE_BAR,
                format!("{:.0}%", quota.storage_fraction * 100.0),
            )
            .attr("fraction", format!("{:.4}", quota.storage_fraction)),
        );
        els.push(Element::label(
            ids::SETTINGS_STORAGE_TEXT,
            quota.storage.clone(),
        ));
    }
    els.extend(feature_limits_elements(state));
    els.extend(region);
    // Status sections 5–7 (Node, Sync, Encryption) from the shared snapshot.
    els.extend(status_sections(status));
    // About: the running version and the user-triggered newer-version check
    // (`about.rs`), then the Build section beside it — the landing's last
    // live blocks, before the rail rows.
    els.extend(super::about::elements(state));
    els.extend(build_section(status));
    // The rail rows into the sub-pages.
    els.push(
        Element::gesture_button(
            ids::ACCOUNT_SETTINGS_LINK,
            t::account_page::TITLE,
            true,
            Gesture::Settings(Action::OpenAccount),
        )
        .nav(),
    );
    // Members To Review, directly after Account — the canonical rail order
    // (`settings.md` § Navigation model), placed there because the Account
    // sub-page's Recovery Kit section holds the ephemeral half of the very same
    // review, and the two belong side by side.
    //
    // ⚠ **Unconditional, deliberately.** The page is empty most of the time and
    // that is the design, not clutter: it is the ratified permanent HOME of a
    // deferred backlog (`succession-aftermath.md` § Propagation, ruling 1), and
    // a home you can only reach while it has something in it is not one. Gating
    // the row on `member_reviews` being non-empty would also make the empty
    // state — `member-review-empty`, approved with the page — unreachable by
    // any route a user has.
    els.push(
        Element::gesture_button(
            rail_id("member_review"),
            t::member_review_page::TITLE,
            true,
            Gesture::Settings(Action::OpenMemberReview),
        )
        .nav(),
    );
    // Privacy is realized from the `settings` page itself, so its row is keyed
    // by the nav slug `privacy` (ui.yaml `navigation.sub_page_nav_rows`).
    els.push(
        Element::gesture_button(
            rail_id("privacy"),
            t::privacy_page::TITLE,
            true,
            Gesture::Settings(Action::OpenPrivacy),
        )
        .nav(),
    );
    // Muted words sits directly after Privacy — the canonical rail order
    // (`settings.md` § Navigation model: its sibling personal content-filtering
    // surface to the spam preferences on Privacy).
    els.push(
        Element::gesture_button(
            rail_id("muted-words"),
            muted_words::TITLE,
            true,
            Gesture::Settings(Action::OpenMutedWords),
        )
        .nav(),
    );
    // Personalization + Community labelers sit directly after Muted words — the
    // canonical rail order (`settings.md` § Navigation model: "**Personalization**
    // … and **Community labelers** … sit right after Muted words as its sibling
    // personalization surfaces"), the placement linux, web and windows already
    // agree on. These rows are the real-human
    // affordance without which the pages would be built but unreachable (a page
    // nobody can open is not a configuration surface).
    els.push(
        Element::gesture_button(
            rail_id("personalization"),
            personalization::TITLE,
            true,
            Gesture::Settings(Action::OpenPersonalization),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("labeler-catalog"),
            labeler_catalog::TITLE,
            true,
            Gesture::Settings(Action::OpenLabelerCatalog),
        )
        .nav(),
    );
    // General, then Encryption — the canonical rail order (`settings.md`
    // § Navigation model: "… Community labelers · General · Encryption ·
    // Devices …"), tui's standing parity gap closed. Both are
    // keyed by their nav slugs, like Privacy.
    els.push(
        Element::gesture_button(
            rail_id("general"),
            t::GENERAL,
            true,
            Gesture::Settings(Action::OpenGeneral),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("encryption"),
            encryption_page::TITLE,
            true,
            Gesture::Settings(Action::OpenEncryption),
        )
        .nav(),
    );
    // Subscriptions, then Web, then Mail & Calendar — the canonical rail order
    // (`settings.md` § Navigation model: `… Subscriptions · Web · Mail &
    // Calendar · Mail aliases …`), now painted complete with the consumer
    // `subscription-settings` page landed. The
    // row is not optional: a page reachable only from the e2e nav patch is
    // not a configuration surface, and the apps are the ONLY configuration
    // surface (§ Product invariants).
    els.push(
        Element::gesture_button(
            rail_id("subscription-settings"),
            subscriptions::TITLE,
            true,
            Gesture::Settings(Action::OpenSubscriptions),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("web-settings"),
            web_settings::TITLE,
            true,
            Gesture::Settings(Action::OpenWeb),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("mail-settings"),
            mail_settings::TITLE,
            true,
            Gesture::Settings(Action::OpenMail),
        )
        .nav(),
    );
    // Mail aliases + Mail spam sit directly after Mail & Calendar — the
    // canonical rail order (`settings.md` § Navigation model: the rail is
    // "flat, one entry per page", `… Mail & Calendar · Mail aliases · Mail spam
    // · Mail export …`; they are rail SIBLINGS of Mail & Calendar, not children
    // of it). **The Aliases row is a
    // bundled fix, not new scope:** the 2026-07-29 aliases slice built the page
    // but no way for a human to reach it, so it was drivable only from the e2e
    // nav patch — a page nobody can open is not a configuration surface (the
    // product invariant that the apps are the ONLY configuration surface).
    els.push(
        Element::gesture_button(
            rail_id("mail-aliases"),
            mail_aliases::TITLE,
            true,
            Gesture::Settings(Action::OpenMailAliases),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("mail-spam"),
            mail_spam::TITLE,
            true,
            Gesture::Settings(Action::OpenMailSpam),
        )
        .nav(),
    );
    // Export mailbox, then Import mailbox, then Lists + List members, continue
    // the same canonical run (`settings.md` § Navigation model: `… Mail spam ·
    // Mail export · Mail import · Mail lists · Mail list members · Nests …` —
    // the Import slot ADDED to that canonical order in the same commit as this
    // row, closing a gap: the ui.yaml page was ratified 2026-08-27 but the rail
    // slot was never listed). **Both rows are the same class of bundled fix the
    // Aliases row above was:** a page reachable only from the e2e nav patch is
    // not a configuration surface, and the apps are the ONLY configuration
    // surface (§ Product invariants).
    els.push(
        Element::gesture_button(
            rail_id("mail-export"),
            mail_export::TITLE,
            true,
            Gesture::Settings(Action::OpenMailExport),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("mail-import"),
            mail_import::TITLE,
            true,
            Gesture::Settings(Action::OpenMailImport),
        )
        .nav(),
    );
    // Import from other services follows Import mailbox in the canonical rail
    // (`settings.md` § Navigation model, slot added 2026-09-08) — the second
    // import wizard beside the first.
    els.push(
        Element::gesture_button(
            rail_id("archive-import"),
            archive_import::TITLE,
            true,
            Gesture::Settings(Action::OpenArchiveImport),
        )
        .nav(),
    );
    // **List members has its own rail slot in the canonical order**, which is
    // why the page must work with nothing selected: entering it this way picks
    // the user's first list (`route_subpage`).
    els.push(
        Element::gesture_button(
            rail_id("mail-lists"),
            mail_lists::TITLE,
            true,
            Gesture::Settings(Action::OpenMailLists),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("mail-list-members"),
            mail_lists::MEMBERS_TITLE,
            true,
            Gesture::Settings(Action::OpenMailListMembers),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("tui-settings"),
            tui_settings::TITLE,
            true,
            Gesture::Settings(Action::OpenTuiSettings),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("settings-logs"),
            logs::TITLE,
            true,
            Gesture::Settings(Action::OpenLogs),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("devices"),
            devices::TITLE,
            true,
            Gesture::Settings(Action::OpenDevices),
        )
        .nav(),
    );
    // Directly after Devices (`settings.md` § Navigation model): "machines
    // enrolled" beside "sign-ins live now".
    els.push(
        Element::gesture_button(
            rail_id("sessions"),
            fauna_i18n::strings::sessions::TITLE,
            true,
            Gesture::Settings(Action::OpenSessions),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("folders"),
            devices::FOLDERS,
            true,
            Gesture::Settings(Action::OpenFolders),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("atproto"),
            atproto_settings::TITLE,
            true,
            Gesture::Settings(Action::OpenAtproto),
        )
        .nav(),
    );
    els.push(
        Element::gesture_button(
            rail_id("nests"),
            nests::TITLE,
            true,
            Gesture::Settings(Action::OpenNests),
        )
        .nav(),
    );
    // Task delegation sits after Nests — the canonical rail order
    // (`settings.md` § Navigation model: the cross-participant capstone).
    els.push(
        Element::gesture_button(
            rail_id("task-delegation"),
            task_delegation::TITLE,
            true,
            Gesture::Settings(Action::OpenTaskDelegation),
        )
        .nav(),
    );
    // Connected apps sits directly after Task delegation — the third
    // participants-cluster surface (`settings.md` § Navigation model).
    els.push(
        Element::gesture_button(
            rail_id("connected-apps"),
            fauna_i18n::strings::connected_apps::TITLE,
            true,
            Gesture::Settings(Action::OpenConnectedApps),
        )
        .nav(),
    );
    els
}

/// The `feature-limits-section` — the controversial-class feature plane's
/// transparency read (`dynamic-features.md` § Transparency & auditability),
/// tui being the lead app that renders it first.
///
/// **This function paints; it decides nothing.** Every judgement — which cells
/// survived the tier meet, `remaining = limit − observed`, which tier set each
/// bound, whether a member is available/restricted/hidden, and the words for
/// all of it — is `fauna_client_features`' output, so the answer to *"why can't
/// I do this"* cannot differ across the 7 apps (priority #1) and is written
/// once (priority #2). Re-deriving any of it here is the documented way to get
/// the plane wrong: a client that recomposed the tier meet would report `zaps`
/// as available under a `payments` deny the nest refuses — a silent gate
/// wearing the opposite costume.
///
/// Nothing registers until the read resolves (`state.features == None`), the
/// `quota-section` rule: a limits section painted before its data would tell
/// the user nothing restricts them, which is exactly the settled-claim-without-
/// a-basis defect the un-hydrated-paint finding names.
fn feature_limits_elements(state: &SettingsState) -> Vec<Element> {
    use fauna_i18n::strings::features as f;

    let Some(rows) = &state.features else {
        return Vec::new();
    };
    let mut els = vec![Element::label(
        ids::FEATURE_LIMITS_SECTION,
        f::SECTION_TITLE,
    )];

    // A `hidden` row means this nest build does not carry the feature at all
    // (its capability token is absent), so rendering it would advertise a plane
    // the artifact does not have. Filtered here rather than in the shared crate
    // because the crate's job is to *decide*, and a surface that wanted to show
    // the excised members greyed out would use the same rows.
    let visible: Vec<_> = rows.iter().filter(|r| r.affordance != "hidden").collect();

    // The empty state is "this nest gates nothing", NOT "nothing restricts
    // you": an unrestricted member is still a row, because "unrestricted" is an
    // answer and its absence could not be told apart from a nest that never
    // heard of the feature.
    if visible.is_empty() {
        els.push(Element::label(ids::FEATURE_LIMITS_EMPTY, f::EMPTY));
        return els;
    }

    for (i, row) in visible.iter().enumerate() {
        els.push(Element::label(ids::FEATURE_LIMITS_ROW, " ").within(ids::FEATURE_LIMITS_ROW, i));
        els.push(
            Element::label(ids::FEATURE_LIMITS_NAME, localized(&row.name))
                .within(ids::FEATURE_LIMITS_ROW, i),
        );
        els.push(
            Element::label(ids::FEATURE_LIMITS_STATUS, localized(&row.status))
                .within(ids::FEATURE_LIMITS_ROW, i),
        );
        // Only when something actually blocks — boundary 4's "no silent gates"
        // half. `resolve_nested`, not `resolve`: the sentence's `{window}` is
        // itself an i18n key, and plain resolution paints the raw key inside
        // the sentence the user is meant to understand.
        if let Some(reason) = &row.restriction {
            els.push(
                Element::label(
                    ids::FEATURE_LIMITS_RESTRICTION,
                    reason.resolve_nested(fauna_i18n::strings::lookup),
                )
                .within(ids::FEATURE_LIMITS_ROW, i),
            );
        }
        for (m, cell) in row.cells.iter().enumerate() {
            els.push(
                Element::label(ids::FEATURE_LIMITS_QUOTA, " ")
                    .within(ids::FEATURE_LIMITS_QUOTA, m)
                    .within(ids::FEATURE_LIMITS_ROW, i),
            );
            els.push(
                Element::label(
                    ids::FEATURE_LIMITS_QUOTA_LABEL,
                    cell.label.resolve_nested(fauna_i18n::strings::lookup),
                )
                .within(ids::FEATURE_LIMITS_QUOTA, m)
                .within(ids::FEATURE_LIMITS_ROW, i),
            );
            els.push(
                Element::label(
                    ids::FEATURE_LIMITS_QUOTA_VALUE,
                    // The shared two-level composition (a byte or sat magnitude
                    // is itself localized), so tui and linux cannot render the
                    // same headroom differently.
                    fauna_client_features::cell_value_text(cell, fauna_i18n::strings::lookup),
                )
                .within(ids::FEATURE_LIMITS_QUOTA, m)
                .within(ids::FEATURE_LIMITS_ROW, i),
            );
            els.push(
                // Per CELL, not per row: the meet takes the MIN per (dimension,
                // window), so two bounds on one feature can come from different
                // tiers, and a single row-level attribution would be a guess.
                Element::label(ids::FEATURE_LIMITS_QUOTA_TIER, localized(&cell.tier_label))
                    .within(ids::FEATURE_LIMITS_QUOTA, m)
                    .within(ids::FEATURE_LIMITS_ROW, i),
            );
        }
        own_limit_elements(state, &row.feature, i, &mut els);
    }
    els
}

/// The self-limits control on one row (`dynamic-features.md` § Authoring
/// surfaces; `settings.md` § Layout & flow item 2b): the bearer's own AUTHORED
/// document in words, the edit button, and — while open for this member — the
/// shared `feature-policy-editor`, in place, right under its row.
///
/// Absent while the authored read has not resolved (or it failed to
/// answer): a summary painted without its data would say "No limit set"
/// with no basis, the un-hydrated-paint defect again.
fn own_limit_elements(state: &SettingsState, feature: &str, i: usize, els: &mut Vec<Element>) {
    use fauna_i18n::strings::features as f;

    let own = &state.own_limits;
    let Some(row) = own
        .surface
        .as_ref()
        .and_then(|surface| surface.rows().into_iter().find(|r| r.feature == feature))
    else {
        return;
    };
    els.push(Element::chrome(f::OWN_LABEL));
    els.push(
        Element::label(ids::FEATURE_LIMITS_OWN_SUMMARY, localized(&row.summary))
            .within(ids::FEATURE_LIMITS_ROW, i),
    );
    els.push(
        Element::gesture_button(
            ids::FEATURE_LIMITS_OWN_EDIT_BUTTON,
            f::OWN_EDIT,
            true,
            Gesture::Settings(Action::OpenOwnFeatureLimitEditor(feature.to_string())),
        )
        .within(ids::FEATURE_LIMITS_ROW, i),
    );
    if let Some(editor) = own.editor.as_ref().filter(|e| e.feature_key() == feature) {
        els.extend(crate::feature_editor::editor_elements(
            editor,
            own.status.as_deref(),
            crate::feature_editor::EditorGestures {
                on: Gesture::Settings(Action::OwnFeatureLimitOn(true)),
                off: Gesture::Settings(Action::OwnFeatureLimitOn(false)),
                save: Gesture::Settings(Action::SaveOwnFeatureLimit),
                remove: Gesture::Settings(Action::RemoveOwnFeatureLimit),
                cancel: Gesture::Settings(Action::CancelOwnFeatureLimitEditor),
                cell: |cell| Field::Settings(SettingsField::OwnFeatureLimitCell { cell }),
            },
        ));
    }
}

impl QuotaView {
    /// Format a [`QuotaGetReply`] into the display cells — `"used / max"` bytes
    /// via the shared `fauna_core::format::byte_size` (so every app localizes
    /// the same 1024-unit decision), and a bare `used / max` device count.
    pub(super) fn from_reply(reply: &QuotaGetReply) -> Self {
        let bytes = |used: i64, max: i64| format!("{} / {}", byte_size(used), byte_size(max));
        QuotaView {
            inbox: bytes(reply.inbox.used_bytes, reply.inbox.max_bytes),
            storage: bytes(reply.storage.used_bytes, reply.storage.max_bytes),
            devices: format!("{} / {}", reply.devices.used, reply.devices.max),
            storage_fraction: fauna_core::format::quota_fraction(
                reply.storage.used_bytes,
                reply.storage.max_bytes,
            ),
        }
    }
}

use crate::format::byte_size;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Role;
    use fauna_client_features::test_fixtures::item;
    use fauna_client_features::{GatedFeature, RuleTier, feature_rows};
    use fauna_core::feature_gate::{
        Availability, FeaturePolicy, UsageCounters, WindowCounts, effective_policy, entry,
    };
    use fauna_protocol::discovery::capability;
    use fauna_protocol::features::FeatureStatusItem;

    /// A state whose feature read has resolved to `rows`.
    fn with_features(rows: Vec<fauna_client_features::FeatureRow>) -> SettingsState {
        SettingsState {
            features: Some(rows),
            ..SettingsState::default()
        }
    }

    fn ids(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.id.as_str()).collect()
    }

    fn text_of<'a>(els: &'a [Element], id: &str) -> &'a str {
        els.iter()
            .find(|e| e.id == id)
            .map(|e| e.text.as_str())
            .unwrap_or_default()
    }

    const STATUS_IDS: [&str; 7] = [
        "status-node-domain",
        "status-node-version",
        "status-sync-pending",
        "status-sync-last",
        "status-mls-key-packages",
        "status-mls-channels",
        "status-build-sha",
    ];

    /// The un-hydrated window for sections 5–8: nothing registers before its
    /// leg loads — no placeholder, no zero (`ui/status.md` § State & data
    /// shape), so a driver's wait lands on the real value.
    #[test]
    fn status_sections_are_absent_until_their_legs_load() {
        let els = root_elements(&SettingsState::default());
        for id in STATUS_IDS {
            assert!(
                !ids(&els).contains(&id),
                "{id} must not register before its leg loads"
            );
        }
        // Half a leg is still no leg: a node domain without its version paints
        // nothing, the pair is one section.
        let half = StatusText {
            node_domain: Some("nest.example".into()),
            ..StatusText::default()
        };
        let els = root_page(&SettingsState::default(), Vec::new(), &half);
        assert!(!ids(&els).contains(&"status-node-domain"));
    }

    /// Every loaded leg paints its BARE value under the canonical id — the
    /// witness on every app reads the value, never a "Domain: …" line — with
    /// the human label riding `labelled`, in the section order the goal doc
    /// lists (Node, Sync, Encryption, then Build beside About).
    #[test]
    fn loaded_status_legs_paint_bare_values_under_the_canonical_ids() {
        let status = StatusText {
            node_domain: Some("nest.example".into()),
            node_version: Some("0.1.2".into()),
            sync_pending: Some("0 files, 0 B".into()),
            sync_last: Some(common::NEVER.into()),
            mls_key_packages: Some("20".into()),
            mls_channels: Some("2".into()),
            build_sha: Some("4c1b9f52624c".into()),
        };
        let els = root_page(&SettingsState::default(), Vec::new(), &status);
        assert_eq!(text_of(&els, "status-node-domain"), "nest.example");
        assert_eq!(text_of(&els, "status-node-version"), "0.1.2");
        assert_eq!(text_of(&els, "status-sync-pending"), "0 files, 0 B");
        assert_eq!(text_of(&els, "status-sync-last"), common::NEVER);
        assert_eq!(text_of(&els, "status-mls-key-packages"), "20");
        assert_eq!(text_of(&els, "status-mls-channels"), "2");
        assert_eq!(text_of(&els, "status-build-sha"), "4c1b9f52624c");
        for id in STATUS_IDS {
            let el = els.iter().find(|e| e.id == id).unwrap();
            assert!(matches!(el.role, Role::Label), "{id} is a read-only value");
            assert!(el.label.is_some(), "{id} carries its human label");
        }
        let position = |id: &str| ids(&els).iter().position(|e| *e == id).unwrap();
        assert!(position("status-node-domain") < position("status-sync-pending"));
        assert!(position("status-sync-pending") < position("status-mls-key-packages"));
        assert!(
            position("settings-app-version") < position("status-build-sha"),
            "the build commit sits right after the app's own version"
        );
        assert!(position("status-build-sha") < position("account-settings-link"));
    }

    /// The un-hydrated window. A limits section painted before its read lands
    /// would tell the user nothing restricts them — a settled claim with no
    /// basis, which is this queue's most-repeated defect (six instances).
    #[test]
    fn nothing_registers_until_the_feature_read_resolves() {
        let els = root_elements(&SettingsState::default());
        for id in [
            "feature-limits-section",
            "feature-limits-empty",
            "feature-limits-row",
        ] {
            assert!(
                !ids(&els).contains(&id),
                "{id} must not register before the read resolves"
            );
        }
    }

    /// The whole point of the surface (boundary 4): the limit, the remainder
    /// **and** the tier that set it, for a member the user is merely bounded on.
    #[test]
    fn a_bounded_member_shows_its_headroom_and_who_set_it() {
        let rows = feature_rows(
            &[item(GatedFeature::P2pShare, &[])],
            &[capability::P2P_SHARE.to_string()],
        );
        let els = root_elements(&with_features(rows));

        assert!(ids(&els).contains(&"feature-limits-section"));
        assert_eq!(text_of(&els, "feature-limits-name"), "File sharing");
        assert_eq!(text_of(&els, "feature-limits-status"), "Available");
        assert!(
            !ids(&els).contains(&"feature-limits-restriction"),
            "nothing blocks, so there is no why-line to paint"
        );

        // The operations/day cell, painted in full.
        let label = els
            .iter()
            .filter(|e| e.id == "feature-limits-quota-label")
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>();
        assert!(
            label.contains(&"Uses per day"),
            "expected a resolved cell label, got {label:?}"
        );
        let limit = entry(GatedFeature::P2pShare)
            .tier1
            .operations
            .per_day
            .unwrap();
        let values = els
            .iter()
            .filter(|e| e.id == "feature-limits-quota-value")
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>();
        assert!(
            values.contains(&format!("{limit} left of {limit}").as_str()),
            "expected the headroom sentence, got {values:?}"
        );
        assert_eq!(
            text_of(&els, "feature-limits-quota-tier"),
            "Fauna's built-in limits",
            "boundary 4's attribution half"
        );
    }

    /// No raw i18n key may reach the screen. Both the cell label and the
    /// restriction sentence substitute keys, and plain `resolve` leaves them
    /// visible — this is the assertion that catches a `resolve`/`resolve_nested`
    /// slip anywhere in the section.
    #[test]
    fn no_painted_text_leaks_a_raw_i18n_key() {
        let feature = GatedFeature::P2pShare;
        let ops_day = entry(feature).tier1.operations.per_day.unwrap();
        let spent = FeatureStatusItem {
            feature,
            policy: effective_policy(feature, &[], &[]),
            usage: UsageCounters {
                operations: WindowCounts {
                    day: ops_day,
                    week: ops_day,
                    month: ops_day,
                },
                ..Default::default()
            },
            extra: Default::default(),
        };
        let els = root_elements(&with_features(feature_rows(
            &[spent],
            &[capability::P2P_SHARE.to_string()],
        )));

        assert_eq!(
            text_of(&els, "feature-limits-restriction"),
            "You've used up Fauna's built-in limit for this day."
        );
        for el in &els {
            assert!(
                !el.text.contains("features."),
                "{} painted a raw key: {:?}",
                el.id,
                el.text
            );
        }
    }

    /// A member the nest build does not carry must not be rendered at all —
    /// `registry()` is not cfg-gated, so a payments-excised nest still returns a
    /// `payments` row, and painting it would advertise a plane the artifact
    /// does not have.
    #[test]
    fn a_hidden_member_is_not_rendered_and_a_carried_one_is() {
        let payments = [item(GatedFeature::Payments, &[])];

        let excised = root_elements(&with_features(feature_rows(&payments, &[])));
        assert!(
            ids(&excised).contains(&"feature-limits-empty"),
            "the only member is hidden, so the section has nothing to show"
        );
        assert!(!ids(&excised).contains(&"feature-limits-row"));

        let carried = root_elements(&with_features(feature_rows(
            &payments,
            &[capability::SUBSCRIPTIONS.to_string()],
        )));
        assert!(ids(&carried).contains(&"feature-limits-row"));
        assert_eq!(text_of(&carried, "feature-limits-name"), "Payments");
    }

    /// Attribution is per CELL, not per row: the meet takes the MIN per
    /// (dimension, window), so an admin bound on one window sits beside a
    /// structural bound on another, and a single row-level tier would be a lie
    /// about one of them.
    #[test]
    fn two_cells_of_one_member_can_name_different_tiers() {
        let admin_day = FeaturePolicy {
            availability: Availability::Limit,
            operations: fauna_core::feature_gate::WindowedBounds {
                per_day: Some(3),
                per_week: None,
                per_month: None,
            },
            ..Default::default()
        };
        let rows = feature_rows(
            &[item(
                GatedFeature::P2pShare,
                &[(RuleTier::Admin, admin_day)],
            )],
            &[capability::P2P_SHARE.to_string()],
        );
        let els = root_elements(&with_features(rows));

        let tiers: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "feature-limits-quota-tier")
            .map(|e| e.text.as_str())
            .collect();
        assert!(
            tiers.contains(&"Your nest admin") && tiers.contains(&"Fauna's built-in limits"),
            "expected both tiers across the cells, got {tiers:?}"
        );
    }

    /// Cells are scoped to their row *and* indexed within it, so a test can
    /// assert one (dimension, window) cell instead of counting globally — the
    /// scoped-query rule (`testing.md` point 1). Two members means a flat
    /// query would silently read the wrong feature's quota.
    #[test]
    fn a_quota_cell_is_addressable_within_its_own_row() {
        let rows = feature_rows(
            &[
                item(GatedFeature::Payments, &[]),
                item(GatedFeature::P2pShare, &[]),
            ],
            &[
                capability::SUBSCRIPTIONS.to_string(),
                capability::P2P_SHARE.to_string(),
            ],
        );
        let els = root_elements(&with_features(rows));

        let second_row_cells: Vec<&Element> = els
            .iter()
            .filter(|e| {
                e.id == "feature-limits-quota-value"
                    && e.path.first().map(|(id, i)| (id.as_str(), *i))
                        == Some(("feature-limits-row", 1))
            })
            .collect();
        assert!(
            !second_row_cells.is_empty(),
            "the second member's cells must be scoped to row 1"
        );
        for (offset, cell) in second_row_cells.iter().enumerate() {
            assert_eq!(
                cell.path.get(1).map(|(id, i)| (id.as_str(), *i)),
                Some(("feature-limits-quota", offset)),
                "each cell carries its own index inside the row"
            );
        }
    }

    /// **The rail's destinations and its actions must be distinguishable.**
    /// `account-settings-link` (goes to the Account page) and
    /// `account-actor-id-copy-btn` (copies the actor id, right now) sit within
    /// two rows of each other and used to paint identically as `[ text ]` — the
    /// audit's nav-vs-action finding, reported by a live user as not knowing
    /// which rows were "navigational?".
    #[test]
    fn a_rail_destination_is_nav_and_a_root_page_action_is_not() {
        let els = root_elements(&SettingsState::default());
        let by_id = |id: &str| els.iter().find(|e| e.id == id).unwrap().nav;
        assert!(by_id("account-settings-link"), "a rail row goes somewhere");
        assert!(
            !by_id("account-actor-id-copy-btn"),
            "Copy acts now — it must keep the brackets"
        );
    }

    /// The copy confirms on the button and reports what it copied — the id,
    /// not a placeholder — only once it has fired.
    #[test]
    fn the_actor_id_copy_button_confirms_and_reports_what_it_copied() {
        let mut state = SettingsState {
            account_actor_id: "ab".repeat(32),
            ..SettingsState::default()
        };
        let copy_btn = |state: &SettingsState| {
            root_elements(state)
                .into_iter()
                .find(|e| e.id == "account-actor-id-copy-btn")
                .unwrap()
        };
        let before = copy_btn(&state);
        assert_eq!(before.text, common::COPY);
        assert!(before.attrs.iter().all(|(k, _)| k != "copied"));

        state.actor_id_copied = true;
        let after = copy_btn(&state);
        assert_eq!(after.text, common::COPIED);
        assert!(
            after
                .attrs
                .contains(&("copied".to_string(), "ab".repeat(32))),
            "{:?}",
            after.attrs
        );
    }

    /// Every rail row is `settings-nav-row[<key>]` (ui.yaml
    /// `navigation.sub_page_nav_rows`) — no untagged button is left on the root,
    /// since an untagged row is one a gesture-only test cannot click — each key
    /// is distinct, and the whole rail reads as destinations: one forgotten
    /// `.nav()` would silently re-create the ambiguity for that row alone, which
    /// is exactly the shape no one would notice.
    #[test]
    fn every_rail_row_is_a_keyed_destination() {
        let els = root_elements(&SettingsState::default());
        let untagged: Vec<&str> = els
            .iter()
            .filter(|e| e.id.is_empty() && matches!(e.role, Role::Button(_)))
            .map(|e| e.text.as_str())
            .collect();
        assert!(untagged.is_empty(), "untagged rail rows: {untagged:?}");
        let rail: Vec<&Element> = els
            .iter()
            .filter(|e| e.id.starts_with("settings-nav-row[") && e.id.ends_with(']'))
            .collect();
        assert!(
            rail.len() >= 20,
            "expected the flat rail, found {} rows",
            rail.len()
        );
        let keys: std::collections::BTreeSet<&str> = rail.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(keys.len(), rail.len(), "duplicate rail-row keys");
        assert!(keys.contains("settings-nav-row[mail-settings]"));
        for row in rail {
            assert!(
                row.nav,
                "rail row {:?} must paint as a destination",
                row.text
            );
        }
    }

    // ── The self-limits control (dynamic-features.md § Authoring surfaces) ──

    fn with_own_limits() -> SettingsState {
        let caps = vec![
            capability::SUBSCRIPTIONS.to_string(),
            capability::P2P_SHARE.to_string(),
        ];
        let items: Vec<FeatureStatusItem> =
            GatedFeature::ALL.iter().map(|f| item(*f, &[])).collect();
        let mut state = with_features(feature_rows(&items, &caps));
        state.own_limits.surface = Some(fauna_client_features::AuthoredSurface {
            items: GatedFeature::ALL
                .iter()
                .map(|f| fauna_client_features::AuthoredPolicyItem {
                    feature: *f,
                    policy: None,
                    unreadable: false,
                    ceiling: item(*f, &[]).policy,
                    extra: Default::default(),
                })
                .collect(),
            ..Default::default()
        });
        state
    }

    /// Each row gains the bearer's own authored document in words and the edit
    /// button, scoped within the row; the editor is closed until pressed.
    #[test]
    fn each_row_carries_its_own_summary_and_edit_button() {
        let els = feature_limits_elements(&with_own_limits());
        let own: Vec<_> = els
            .iter()
            .filter(|e| e.id == "feature-limits-own-summary")
            .collect();
        assert_eq!(own.len(), 3, "one per carried member");
        for (i, e) in own.iter().enumerate() {
            assert_eq!(e.text, fauna_i18n::strings::features::AUTHORED_NONE);
            assert!(
                e.path
                    .iter()
                    .any(|s| s.0 == "feature-limits-row" && s.1 == i)
            );
        }
        assert!(ids(&els).contains(&"feature-limits-own-edit-button"));
        assert!(!ids(&els).contains(&"feature-policy-editor"));
    }

    /// No authored read yet (or it failed): the rows stay read-only rather
    /// than claiming "No limit set" with no basis.
    #[test]
    fn no_authored_read_means_no_own_summary() {
        let items: Vec<FeatureStatusItem> =
            GatedFeature::ALL.iter().map(|f| item(*f, &[])).collect();
        let els = feature_limits_elements(&with_features(feature_rows(
            &items,
            &[capability::P2P_SHARE.to_string()],
        )));
        assert!(!ids(&els).contains(&"feature-limits-own-summary"));
        assert!(!ids(&els).contains(&"feature-limits-own-edit-button"));
    }

    /// The edit button opens the SHARED editor in place at the self tier; its
    /// writes declare the self kind the offline gate reads.
    #[test]
    fn the_own_edit_button_opens_the_shared_editor_in_place() {
        let mut state = with_own_limits();
        super::super::apply(
            &mut state,
            Action::OpenOwnFeatureLimitEditor("p2p-share".into()),
        );
        let els = feature_limits_elements(&state);
        let title = text_of(&els, "feature-policy-editor-title");
        assert_eq!(
            title,
            fauna_i18n::strings::features::editor_title_self(
                fauna_i18n::strings::features::NAME_P2P_SHARE
            )
        );
        assert!(ids(&els).contains(&"feature-policy-editor-cell-input"));
        assert!(
            !ids(&els).contains(&"feature-policy-editor-remove-button"),
            "no document yet, so no Remove"
        );
        assert_eq!(
            Action::SaveOwnFeatureLimit.wire_kind(),
            Some("fauna.features.self_limits.update")
        );
        super::super::apply(&mut state, Action::CancelOwnFeatureLimitEditor);
        assert!(!ids(&feature_limits_elements(&state)).contains(&"feature-policy-editor"));
    }
}
