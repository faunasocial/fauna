//! Message bubble rendered from a shared-Rust `MessageSnapshot`.
//!
//! Mirrors `apps/fauna-windows/.../Controls/DmMessageBubble.xaml`. Layout:
//! sender + badges + timestamp on top; then the body — the shared
//! `RenderDocument` walk, which now also paints the `Attachment` blocks the
//! manager appends after the text (render-model.md § D2); aggregated reaction
//! pills; per-message reply + ⋯ actions controls; per-message content-label
//! badge.
//!
//! Self-vs-peer alignment: `MessageSnapshot.is_own` right-aligns a bubble;
//! everything else left-aligned. (Fixed 2026-07-17 — this used to compare
//! `sender_display` against the caller's handle, which can never match:
//! `sender_display` is empty until contact-name resolution lands
//! (conversations.md § Where logic lives), so every bubble rendered as peer.)
//!
//! Badge IDs: `encrypted-badge`, `signed-badge`, `verified-badge`,
//! `content-label-badge` — all indexed siblings inside the bubble container per
//! the spec section 4 + goal doc Element IDs. `c2pa-badge` is per attachment,
//! painted by the document walk beside the attachment it vouches for.
//!
//! Reactions + message delete (conversations.md § Reactions & message delete;
//! Windows is the reference leg): an always-visible `dm-message-actions-button`
//! (⋯, shown iff ≥1 action is available) opens the `dm-message-actions-menu`
//! flyout — a fixed quick-set of `dm-reaction-option` emojis and a
//! `dm-reaction-more-button` (GTK emoji chooser, the only sanctioned divergence),
//! plus (own messages only) `dm-message-delete-button` → `dm-message-delete-confirm-button`
//! and (received messages only) `dm-message-mark-as-spam-button` (the live `Insert`
//! consumer — trains the sealed tier-1 spam model and writes a sealed
//! training-history row via `FaunaClient::mark_message_spam`; mail-spam.md
//! § Encrypted-mode interaction).
//! Aggregated reactions render under the bubble as tap-to-toggle `dm-reaction-pill`s.
//! A deleted message renders the localized `dm-message-deleted` tombstone
//! placeholder (body/attachments/reactions/actions stripped). The affordances are
//! **capability-gated** off `ThreadCapabilities` (`supports_reactions`,
//! `supports_message_delete && is_own`) + `!is_own` (mark-as-spam) and route
//! through the shared `ConversationsManager` (`toggle_reaction` / `delete_message`)
//! or `FaunaClient::mark_message_spam` — never a rail branch (capability-gated, not per-rail).

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;

use fauna_conversations::message::{MessageId, MessageSnapshot};
// QUICKSET_EMOJIS: the shared quick-set (identical order on all apps;
// conversations.md § Reactions & message delete), so the Rust-native apps
// can never drift on the set.
use fauna_conversations::{QUICKSET_EMOJIS, ThreadCapabilities, ThreadId};

use fauna_core::obligation::RenderVerdict;

use crate::client::FaunaClient;
use crate::views::document;

/// Callback fired by a bubble's reply controls: `(replied-to message id,
/// reply_all)`. `false` = `dm-reply-button` (sender-only); `true` =
/// `dm-reply-all-button` (every participant but self). Wired to
/// `manager.start_reply`.
pub type OnReply = Rc<dyn Fn(MessageId, bool)>;

/// A message bubble's event closures read the newest snapshot for their own
/// message id from this map instead of a captured render-time clone — a
/// bubble that isn't rebuilt this tick keeps last tick's closures (identity-
/// keyed reuse, `detail.rs` § the messages-box reconcile), so a closure fixed
/// at an earlier `build` would otherwise see a stale message. Mirrors windows'
/// `_latestSnapshotById`.
pub type LatestSnapshotById = Rc<RefCell<HashMap<MessageId, MessageSnapshot>>>;

/// Build (or, given `existing`, rebind in place) a bubble for a single
/// `MessageSnapshot`. `existing` is `Some` when `detail.rs` is refreshing a
/// message whose content or selection state changed but whose message id it
/// has already built a widget for — the SAME outer box is cleared and
/// repainted rather than replaced, so its identity in `messages_box` (and any
/// external reference to it) survives. `caps` gates the per-bubble affordances
/// (reactions / delete / reply-all) — never branch on rail. `thread_id` is
/// needed to route `toggle_reaction` / `delete_message` through the shared
/// manager. `show_reply_all` gates the `dm-reply-all-button` (mail only —
/// `supports_recipient_selection`). `rt` drives the async manager dispatch
/// (reaction/delete/reveal) off the GTK main thread. `client` loads the
/// link-preview og:image blob (the shared walker has no blob loader — the same
/// async-byte-load-stays-client idiom as the feed card, render-model.md § D4).
/// `is_selected` is `SearchNav::Mail`'s second half (conversations.md § The
/// selected message) — whether this is the one message a mail search result
/// named. `latest_by_id` is threaded through to every closure this function
/// wires (see [`LatestSnapshotById`]).
#[allow(clippy::too_many_arguments)]
pub fn build(
    existing: Option<&gtk::Box>,
    msg: &MessageSnapshot,
    self_handle: &str,
    caps: &ThreadCapabilities,
    thread_id: &ThreadId,
    show_reply_all: bool,
    on_reply: OnReply,
    client: &Rc<FaunaClient>,
    rt: &tokio::runtime::Handle,
    is_selected: bool,
    latest_by_id: &LatestSnapshotById,
) -> gtk::Box {
    let is_self = msg.is_own;

    let bubble = match existing {
        Some(b) => {
            while let Some(child) = b.first_child() {
                b.remove(&child);
            }
            b.remove_css_class("message-selected");
            b.remove_css_class("message-self");
            b.remove_css_class("message-peer");
            b.clone()
        }
        None => gtk::Box::new(gtk::Orientation::Vertical, 4),
    };
    // The mark for `SearchNav::Mail`'s second half (`conversations.md` § The
    // selected message): a background tint — GUI apps have fill to vary, per
    // that section. Applied regardless of which arm below renders (deleted /
    // legal-takedown / blocked / muted / collapsed / normal), same as the
    // `selected` attribute on the timestamp every arm also carries.
    if is_selected {
        bubble.add_css_class("message-selected");
    }
    // Fill the messages-list width so the bubble has a DEFINITE width on every
    // measure. A `gtk::TextView` reports its *minimum* height at its *minimum*
    // width (its longest word → many wrapped lines → tall); with the old
    // `halign=Start/End` + `hexpand=false` + min `size_request(480)` the bubble's
    // width was never pinned, so during an in-place rebuild the box measured the
    // body's height at that narrow minimum width and reserved ~3× too much height
    // (whitespace below multi-line messages) until a resize/crossfade re-measured
    // at the real width. Filling pins the width (the scroll viewport width is
    // definite), so height-for-width is correct on the first pass. Self vs peer
    // stays distinguished by the message-self / message-peer background.
    bubble.set_hexpand(true);
    bubble.set_halign(gtk::Align::Fill);
    if is_self {
        bubble.add_css_class("message-self");
    } else {
        bubble.add_css_class("message-peer");
    }

    // Deleted tombstone (conversations.md § Reactions & message delete): a
    // cooperative delete-marker every compliant client honors by rendering a
    // localized placeholder — body / attachments / reactions / actions all
    // stripped. The placeholder is the dedicated `dm-message-deleted` element
    // (the cross-app tombstone id windows registered in ui.yaml), so the
    // bubble carries no `dm-message-text` once deleted.
    if msg.deleted {
        let placeholder = gtk::Label::new(Some(
            crate::i18n::strings::conversations::detail::MESSAGE_DELETED,
        ));
        placeholder.add_css_class("dim-label");
        placeholder.add_css_class("message-deleted");
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_xalign(0.0);
        crate::testid::set_test_id(&placeholder, ids::DM_MESSAGE_DELETED);
        bubble.append(&placeholder);
        bubble.append(&message_timestamp_label(msg.timestamp_ms, is_selected));
        bubble.set_overflow(gtk::Overflow::Hidden);
        return bubble;
    }

    // Legal-takedown tombstone (moderation.md § Categories & enforcement item 1):
    // the nest withheld the sealed envelope under a legal obligation, so the bubble
    // collapses (like `deleted`) to the shared localized tombstone rendered in place
    // of the withheld body — never a blank/failed-decrypt bubble. No dedicated test
    // ID (presentation, like the post quoted-post tombstone `document.rs` /
    // ContentLabelBadge; a new e2e id would need ui.yaml approval first, § UI
    // Consistency A). The DOM twin is web `+page.svelte`'s legal_takedown_ref arm.
    if let Some(reference) = &msg.legal_takedown_ref {
        let tombstone = gtk::Label::new(Some(
            &crate::i18n::strings::moderation::legal_takedown::tombstone(reference),
        ));
        tombstone.add_css_class("dim-label");
        tombstone.add_css_class("message-deleted");
        tombstone.set_halign(gtk::Align::Start);
        tombstone.set_wrap(true);
        tombstone.set_xalign(0.0);
        bubble.append(&tombstone);
        bubble.append(&message_timestamp_label(msg.timestamp_ms, is_selected));
        bubble.set_overflow(gtk::Overflow::Hidden);
        return bubble;
    }

    // Content-policy render enforcement (family-safety.md § Content policy): the
    // supervised viewer's guardian floor over this message's post-decrypt labels
    // (`ConversationsManager::observe_local_detection` classifies inbound bodies).
    // A `block` floor is absolute — no reveal — so it is checked FIRST, ahead of
    // the muted-keyword collapse, so a message that is both muted (revealable) and
    // blocked can never be revealed past the guardian's block. `collapse` is
    // handled after the muted arm (both are revealable collapses). Mirrors the feed
    // (`views/feed/post_list.rs`) — both surfaces share `content_policy::verdict_for`.
    let composed = crate::region::verdict_for(&msg.message_id.0, &msg.labels, || {
        crate::region::message_input(&msg.body)
    });
    let content_verdict = composed.verdict;
    // Guardian Notify (family-safety.md § Guardian Notify): count this message if
    // the guardian floor enforces on it — a no-op unless content_notify is on.
    // Deduped per message per local day; the app's flush tick reports the batch.
    crate::content_policy::note_enforcement(&msg.message_id.0, &msg.labels);
    // A REGION verdict paints the region's own placeholder (the region, its
    // authority, the authority's reason verbatim) — the same verb as the family
    // arm below, better attributed; a revealed region `collapse` falls through
    // to the ordinary arms (tui's `conversations/mod.rs` order).
    if let Some(withheld) = crate::region::region_verdict(&composed)
        && (withheld.verb == fauna_client_region::RegionVerb::Block
            || !crate::conversations::is_content_revealed(&msg.message_id))
    {
        let b = bubble.clone();
        let mid = msg.message_id.clone();
        let sh = self_handle.to_string();
        let cap = *caps;
        let tid = thread_id.clone();
        let orp = on_reply.clone();
        let cl = client.clone();
        let rth = rt.clone();
        let latest = Rc::clone(latest_by_id);
        bubble.append(&crate::region::placeholder_box(&withheld, move || {
            crate::conversations::reveal_content(mid.clone());
            let Some(m) = latest.borrow().get(&mid).cloned() else {
                return;
            };
            while let Some(ch) = b.first_child() {
                b.remove(&ch);
            }
            append_full_content(
                &b,
                &m,
                &sh,
                &cap,
                &tid,
                show_reply_all,
                orp.clone(),
                &cl,
                &rth,
                is_selected,
                &latest,
            );
            b.set_overflow(gtk::Overflow::Hidden);
        }));
        bubble.append(&message_timestamp_label(msg.timestamp_ms, is_selected));
        bubble.set_overflow(gtk::Overflow::Hidden);
        return bubble;
    }
    if content_verdict == RenderVerdict::Block {
        let placeholder =
            gtk::Label::new(Some(crate::i18n::strings::family::CONTENT_BLOCKED_NOTICE));
        placeholder.add_css_class("dim-label");
        placeholder.add_css_class("message-deleted");
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_wrap(true);
        placeholder.set_xalign(0.0);
        crate::testid::set_test_id(&placeholder, ids::CONTENT_POLICY_BLOCKED_NOTICE);
        bubble.append(&placeholder);
        bubble.append(&message_timestamp_label(msg.timestamp_ms, is_selected));
        bubble.set_overflow(gtk::Overflow::Hidden);
        return bubble;
    }

    // Muted-keyword collapse (moderation.md § Muted keywords;
    // content-moderation-and-ranking.md § Q3): a decrypted message whose body
    // matches the user's muted list is collapsed behind a placeholder + a
    // one-tap, session-local reveal (the mute itself stays — un-muting the
    // term is what stops it collapsing for *future* messages). Mirrors the
    // `dm-message-deleted` tombstone + the `load-remote-content-button` reveal
    // gesture above. This is a hide/collapse, NOT a spam-queue flag — it does
    // NOT feed `LocalDetectionStore` / the moderation queue.
    if !crate::conversations::is_muted_revealed(&msg.message_id)
        && fauna_core::scoring::muted_keywords_collapse(
            &crate::conversations::muted_keywords_cache(),
            &msg.body,
        )
    {
        let placeholder = gtk::Label::new(Some(
            crate::i18n::strings::conversations::detail::MUTED_WORD,
        ));
        placeholder.add_css_class("dim-label");
        placeholder.add_css_class("message-deleted");
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_xalign(0.0);
        crate::testid::set_test_id(&placeholder, ids::DM_MESSAGE_MUTED);
        bubble.append(&placeholder);

        let reveal =
            gtk::Button::with_label(crate::i18n::strings::conversations::detail::MUTED_REVEAL);
        reveal.add_css_class("flat");
        reveal.set_halign(gtk::Align::Start);
        crate::testid::set_test_id(&reveal, ids::DM_MESSAGE_MUTED_REVEAL_BUTTON);
        // Reveal in place: clear the placeholder + button, render the full
        // content the same way the normal (non-muted) path below does. Reads
        // the message off `latest_by_id` at click time, not a captured `msg`
        // clone — this closure survives untouched across renders where this
        // bubble isn't rebuilt, so a captured snapshot would go stale the
        // moment some OTHER field of this message changed before the tap.
        let b = bubble.clone();
        let mid = msg.message_id.clone();
        let sh = self_handle.to_string();
        let cap = *caps;
        let tid = thread_id.clone();
        let orp = on_reply.clone();
        let cl = client.clone();
        let rth = rt.clone();
        let latest = Rc::clone(latest_by_id);
        reveal.connect_clicked(move |_| {
            crate::conversations::reveal_muted(mid.clone());
            let Some(m) = latest.borrow().get(&mid).cloned() else {
                return;
            };
            while let Some(ch) = b.first_child() {
                b.remove(&ch);
            }
            append_full_content(
                &b,
                &m,
                &sh,
                &cap,
                &tid,
                show_reply_all,
                orp.clone(),
                &cl,
                &rth,
                is_selected,
                &latest,
            );
            b.set_overflow(gtk::Overflow::Hidden);
        });
        bubble.append(&reveal);
        bubble.append(&message_timestamp_label(msg.timestamp_ms, is_selected));
        bubble.set_overflow(gtk::Overflow::Hidden);
        return bubble;
    }

    // Content-policy `collapse` floor (family-safety.md § Content policy): render
    // collapsed with a session-local reveal — the same shape as the muted collapse
    // above but its own reveal set (`REVEALED_CONTENT`). `Block` above already
    // returned, so only `Collapse` reaches here; the floor itself persists (the
    // guardian relaxing it is what stops future collapse).
    if content_verdict == RenderVerdict::Collapse
        && !crate::conversations::is_content_revealed(&msg.message_id)
    {
        let placeholder =
            gtk::Label::new(Some(crate::i18n::strings::family::CONTENT_COLLAPSED_NOTICE));
        placeholder.add_css_class("dim-label");
        placeholder.add_css_class("message-deleted");
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_xalign(0.0);
        bubble.append(&placeholder);

        let reveal = gtk::Button::with_label(crate::i18n::strings::family::CONTENT_REVEAL_BUTTON);
        reveal.add_css_class("flat");
        reveal.set_halign(gtk::Align::Start);
        // Reveal in place: clear the placeholder + button, render the full
        // content the same way the normal (non-collapsed) path below does.
        // Reads off `latest_by_id` at click time — see the muted-reveal arm's
        // comment above.
        let b = bubble.clone();
        let mid = msg.message_id.clone();
        let sh = self_handle.to_string();
        let cap = *caps;
        let tid = thread_id.clone();
        let orp = on_reply.clone();
        let cl = client.clone();
        let rth = rt.clone();
        let latest = Rc::clone(latest_by_id);
        reveal.connect_clicked(move |_| {
            crate::conversations::reveal_content(mid.clone());
            let Some(m) = latest.borrow().get(&mid).cloned() else {
                return;
            };
            while let Some(ch) = b.first_child() {
                b.remove(&ch);
            }
            append_full_content(
                &b,
                &m,
                &sh,
                &cap,
                &tid,
                show_reply_all,
                orp.clone(),
                &cl,
                &rth,
                is_selected,
                &latest,
            );
            b.set_overflow(gtk::Overflow::Hidden);
        });
        bubble.append(&reveal);
        bubble.append(&message_timestamp_label(msg.timestamp_ms, is_selected));
        bubble.set_overflow(gtk::Overflow::Hidden);
        return bubble;
    }

    append_full_content(
        &bubble,
        msg,
        self_handle,
        caps,
        thread_id,
        show_reply_all,
        on_reply,
        client,
        rt,
        is_selected,
        latest_by_id,
    );
    bubble.set_overflow(gtk::Overflow::Hidden);
    bubble
}

/// A message's `dm-message-timestamp`, carrying the `selected` automation
/// attribute — the observable for `SearchNav::Mail`'s second half
/// (`conversations.md` § The selected message; `ui/search.md` § State & data
/// shape). Mirrors tui's `message_timestamp_element`: every arm above and
/// [`append_full_content`] below builds its timestamp through this one
/// constructor, since the timestamp is the one bubble child painted whether
/// the message is deleted, legal-takedown'd, content-blocked, muted, content-
/// collapsed, or normal — so a mail search hit on any of those still has
/// somewhere to land its mark. `selected` is always present (`"true"`/
/// `"false"`), never omitted when false, so a test can tell "not selected"
/// apart from "this app never painted the attribute" (testing.md point 6).
fn message_timestamp_label(then_ms: i64, is_selected: bool) -> gtk::Label {
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    let time_label = gtk::Label::new(Some(&crate::i18n::conversation_timestamp(then_ms, now_ms)));
    time_label.add_css_class("message-time");
    time_label.set_halign(gtk::Align::End);
    crate::testid::set_test_id(&time_label, ids::DM_MESSAGE_TIMESTAMP);
    crate::testid::set_test_attr(
        &time_label,
        "selected",
        if is_selected { "true" } else { "false" },
    );
    time_label
}

/// The full (non-collapsed) bubble content: top row (sender | badges |
/// timestamp), the shared `RenderDocument` body walk, remote-image /
/// link-preview reveals, aggregated reactions, the reply/⋯-actions row, and
/// the content-label badge. Split out of [`build`] so the muted-keyword
/// reveal button ([`build`]'s collapse arm) can rebuild a bubble's content in
/// place without re-running the deleted/legal-takedown/muted checks (a
/// revealed message is, by definition, past all three).
#[allow(clippy::too_many_arguments)]
fn append_full_content(
    bubble: &gtk::Box,
    msg: &MessageSnapshot,
    _self_handle: &str,
    caps: &ThreadCapabilities,
    thread_id: &ThreadId,
    show_reply_all: bool,
    on_reply: OnReply,
    client: &Rc<FaunaClient>,
    rt: &tokio::runtime::Handle,
    is_selected: bool,
    latest_by_id: &LatestSnapshotById,
) {
    let is_self = msg.is_own;

    // Top row: sender | badges | timestamp.
    let top_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    // `sender_display` is empty until contact-name resolution lands
    // (conversations.md § Where logic lives) — fall back to the shared
    // `TypedAddress::display` switch, same as `reply_quote_author`.
    let sender_text = if msg.sender_display.is_empty() {
        msg.sender.display()
    } else {
        msg.sender_display.clone()
    };
    let sender_label = gtk::Label::new(Some(&sender_text));
    sender_label.add_css_class("message-sender");
    sender_label.set_halign(gtk::Align::Start);
    sender_label.set_hexpand(true);
    sender_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    crate::testid::set_test_id(&sender_label, ids::DM_SENDER);
    top_row.append(&sender_label);

    if msg.badges.encrypted {
        let l = gtk::Label::new(Some("\u{1f512}"));
        l.set_tooltip_text(Some(
            crate::i18n::strings::conversations::detail::BADGE_ENCRYPTED,
        ));
        l.add_css_class("dim-label");
        crate::testid::set_test_id(&l, ids::ENCRYPTED_BADGE);
        top_row.append(&l);
    }
    if msg.badges.signed {
        let l = gtk::Label::new(Some("\u{270D}"));
        l.set_tooltip_text(Some(
            crate::i18n::strings::conversations::detail::BADGE_SIGNED,
        ));
        l.add_css_class("dim-label");
        crate::testid::set_test_id(&l, ids::SIGNED_BADGE);
        top_row.append(&l);
    }
    if msg.badges.verified {
        let l = gtk::Label::new(Some("\u{2713}"));
        l.set_tooltip_text(Some(
            crate::i18n::strings::conversations::detail::BADGE_VERIFIED,
        ));
        l.add_css_class("dim-label");
        crate::testid::set_test_id(&l, ids::VERIFIED_BADGE);
        top_row.append(&l);
    }
    // No message-level `c2pa-badge`: provenance is a per-attachment verdict the
    // document walk paints beside its attachment (`views/document.rs`,
    // `conversations.md` § Attachments "C2PA on-device").

    // Per-message timestamp — the SAME shared contextual bucketer the conversation
    // list row uses (`crate::i18n::conversation_timestamp` →
    // `fauna_core::format::conversation_timestamp_display`, value-formatting.md
    // § Conversation timestamp): today → a local 24h clock, Yesterday / a weekday,
    // else a local short date. Replaces the old hand-rolled naive-UTC `HH:MM`, which
    // ignored the local zone (the same bug the list row already fixed) and diverged
    // from the other apps (render-model.md § D5-adjacent; priorities #1/#4).
    // Also carries the `selected` attribute — see `message_timestamp_label`.
    top_row.append(&message_timestamp_label(msg.timestamp_ms, is_selected));

    bubble.append(&top_row);

    // Body — walk the shared `RenderDocument` the conversations manager already built
    // (markdown / plaintext / inbound-HTML all collapse to one typed node tree —
    // render-model.md § D1); the body is no longer re-parsed at render time. The walk is a
    // per-block `gtk::Label` renderer: `GtkLabel` measures its height-for-width correctly on
    // the first pass, so the bubble renders at the right height immediately on an in-place
    // rebuild — unlike a `GtkTextView`, which reports its minimum height at its minimum
    // width and flashed/stuck too-tall (or clipped) until a resize re-measured. See
    // `document::render_to_widget`. The walk paints each remote `![]()` image from the
    // block's authoritative `revealed` flag — blocked placeholder or fetched picture (D3).
    let body_box = document::render_to_widget(
        &msg.document,
        rt,
        &crate::media_loads::MediaScope::conversations(),
    );
    let body: gtk::Widget = body_box.upcast();
    body.set_hexpand(true);
    crate::testid::set_test_id(&body, ids::DM_MESSAGE_TEXT);
    bubble.append(&body);

    // Inline remote images render BLOCKED until revealed (html-mail Slice 3 — the
    // privacy perimeter: untrusted inbound mail/DM content must never auto-fetch
    // remote refs / tracking pixels). The per-message `load-remote-content-button`
    // appears iff the document still has a blocked remote image, and DISPATCHES to the
    // manager — `reveal_remote_images(message_id)` flips the manager-owned reveal state and
    // re-emits; the observer rebuild then re-walks this document with `revealed: true`, so
    // the button is simply absent from the rebuilt bubble (render-model.md § D3). No
    // client-side reveal state remains.
    if msg.document.has_blocked_remote_images() {
        let mid = msg.message_id.clone();
        document::attach_reveal_button(bubble, move || {
            crate::conversations::manager().reveal_remote_images(mid.clone());
        });
    }

    // Link-preview cards (render-model.md § D4): one per Resolved `LinkPreview` block in the
    // message `document` (the producer emits one per standalone bare-url paragraph in a
    // Markdown body; the shared `ConversationsManager::resolve_link_preview` folds it to
    // `Resolved`, fired fire-once in the detail render loop). The og:image is extracted +
    // painted here through the client's blob loader and gated on its `revealed` flag — the
    // same async-byte-load-stays-client + D3-reveal idiom as the feed card and the bubble's
    // remote images above (the `load-remote-content-button` reveals body images + the og:image
    // together, for free). Resolving/Failed paint no card (the inline body link already shows).
    for lp in msg.document.resolved_link_previews() {
        bubble.append(&document::build_link_preview_card(
            &lp,
            client,
            &crate::media_loads::MediaScope::conversations(),
        ));
    }

    // Attachments are no longer appended here: the manager projects each into an
    // `Attachment` block after the body (render-model.md § D2), so `render_to_widget`
    // paints them in body order as part of the one document walk above.

    // Aggregated reactions under the bubble (`dm-reaction-pill[i]` — emoji + count,
    // own highlighted, tap-to-toggle). Rendered only when the message carries any;
    // the count/highlight are the shared `MessageSnapshot.reactions` aggregate.
    if !msg.reactions.is_empty() {
        bubble.append(&build_reaction_pills(msg, thread_id, rt));
    }

    // Action row: ⋯ actions (reactions / delete) | reply | reply-all.
    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    action_row.set_halign(if is_self {
        gtk::Align::End
    } else {
        gtk::Align::Start
    });
    action_row.set_margin_top(2);

    // ⋯ per-bubble actions — shown iff ≥1 action is available for this message
    // (react OR own-deletable OR spam-flaggable), so mail bubbles stay clean.
    // Capability-gated, never rail-branched. Mark-as-spam is offered on any
    // received message (`!is_own`) — you flag others' content as spam, not your
    // own; the training itself is best-effort (silent no-op if mail isn't enabled,
    // exactly like a local moderation correction).
    let can_react = caps.supports_reactions;
    let can_delete = caps.supports_message_delete && msg.is_own;
    let can_flag_spam = !msg.is_own;
    if can_react || can_delete || can_flag_spam {
        let actions_btn = build_actions_button(
            msg,
            thread_id,
            can_react,
            can_delete,
            can_flag_spam,
            client,
            rt,
            latest_by_id,
        );
        action_row.append(&actions_btn);
    }

    let reply_btn = gtk::Button::from_icon_name("mail-reply-sender-symbolic");
    reply_btn.add_css_class("flat");
    crate::testid::set_test_id(&reply_btn, ids::DM_REPLY_BUTTON);
    {
        let on_reply = on_reply.clone();
        let mid = msg.message_id.clone();
        reply_btn.connect_clicked(move |_| on_reply(mid.clone(), false));
    }
    action_row.append(&reply_btn);

    // Reply-all — seeds the editable To line with every participant but self.
    // Rendered unconditionally, visibility gated on the rail capability
    // (capability gating: never rail-branch the widget tree).
    let reply_all_btn = gtk::Button::from_icon_name("mail-reply-all-symbolic");
    reply_all_btn.add_css_class("flat");
    reply_all_btn.set_visible(show_reply_all);
    crate::testid::set_test_id(&reply_all_btn, ids::DM_REPLY_ALL_BUTTON);
    {
        let on_reply = on_reply.clone();
        let mid = msg.message_id.clone();
        reply_all_btn.connect_clicked(move |_| on_reply(mid.clone(), true));
    }
    action_row.append(&reply_all_btn);
    bubble.append(&action_row);

    // `content-label-badge` — the highest-confidence classifier verdict on
    // this message, if any, via the shared `content_label_style` map (same
    // idiom as the moderation queue and feed post-cards). `msg.badges
    // .content_warning` is a DIFFERENT, unrelated wire field (never populated
    // by `libs/fauna-conversations` — `msg.labels` is the real classifier
    // data path, populated by `observe_local_detection`).
    if let Some(entry) = fauna_core::content_category::primary_content_label(&msg.labels) {
        let badge = crate::views::moderation::build_content_label_badge(&entry.category);
        badge.set_halign(gtk::Align::Start);
        bubble.append(&badge);
    }
}

/// The aggregated-reaction pill row under a bubble. Each pill is a button
/// ("emoji count"); own reactions (`reacted_by_me`) carry a highlight class.
/// Tapping a pill re-toggles that emoji through the shared manager.
fn build_reaction_pills(
    msg: &MessageSnapshot,
    thread_id: &ThreadId,
    rt: &tokio::runtime::Handle,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.set_halign(gtk::Align::Start);
    row.set_margin_top(2);
    for group in &msg.reactions {
        let pill = gtk::Button::with_label(&format!("{} {}", group.emoji, group.count));
        pill.add_css_class("dm-reaction-pill");
        if group.reacted_by_me {
            pill.add_css_class("dm-reaction-pill-mine");
        }
        crate::testid::set_test_id(&pill, ids::DM_REACTION_PILL);
        // A reaction toggle posts a sealed application message through the same
        // MLS-channel seam as a delete/rename/eviction — unconditional, no rail
        // branch (`Action::ToggleReaction`, tui's `conversations/mod.rs::wire_kind`).
        // Pills are torn down + rebuilt fresh every message-stream rebuild, so
        // there is no stale-declaration risk here.
        crate::offline_gate::declare_wire_kind(&pill, "fauna.conversations.channel.send");
        let tid = thread_id.clone();
        let mid = msg.message_id.clone();
        let emoji = group.emoji.clone();
        let rt = rt.clone();
        pill.connect_clicked(move |_| {
            let (tid, mid, emoji) = (tid.clone(), mid.clone(), emoji.clone());
            rt.spawn(async move {
                crate::conversations::manager()
                    .toggle_reaction(tid, mid, emoji)
                    .await;
            });
        });
        row.append(&pill);
    }
    row
}

/// The per-bubble ⋯ `dm-message-actions-button` and its `dm-message-actions-menu`
/// flyout. The flyout is a `gtk::Popover` parented to the button (so its
/// quick-set / delete children stay out of the showing widget tree until the ⋯
/// button pops it up — the e2e "flyout opened" signal). `can_react` adds the
/// reaction quick-set + the GTK emoji-chooser "more" button; `can_delete` adds
/// the delete → confirm two-step; `can_flag_spam` adds the mark-as-spam item.
/// The caller only constructs this when at least one is available.
#[allow(clippy::too_many_arguments)]
fn build_actions_button(
    msg: &MessageSnapshot,
    thread_id: &ThreadId,
    can_react: bool,
    can_delete: bool,
    can_flag_spam: bool,
    client: &Rc<FaunaClient>,
    rt: &tokio::runtime::Handle,
    latest_by_id: &LatestSnapshotById,
) -> gtk::Button {
    use crate::i18n::strings::conversations::detail as s;

    let btn = gtk::Button::from_icon_name("view-more-symbolic");
    btn.add_css_class("flat");
    crate::testid::set_test_id(&btn, ids::DM_MESSAGE_ACTIONS_BUTTON);

    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 6);
    menu.set_margin_top(6);
    menu.set_margin_bottom(6);
    menu.set_margin_start(6);
    menu.set_margin_end(6);
    crate::testid::set_test_id(&menu, ids::DM_MESSAGE_ACTIONS_MENU);

    if can_react {
        let quickset = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        for emoji in QUICKSET_EMOJIS {
            let opt = gtk::Button::with_label(emoji);
            opt.add_css_class("flat");
            opt.add_css_class("dm-reaction-quickset");
            crate::testid::set_test_id(&opt, ids::DM_REACTION_OPTION);
            // Same kind as the reaction pills above — `Action::ToggleReaction`
            // is unconditional; the quick-set row is rebuilt fresh per bubble.
            crate::offline_gate::declare_wire_kind(&opt, "fauna.conversations.channel.send");
            let tid = thread_id.clone();
            let mid = msg.message_id.clone();
            let emoji = emoji.to_string();
            let rt = rt.clone();
            let pop = popover.clone();
            opt.connect_clicked(move |_| {
                pop.popdown();
                let (tid, mid, emoji) = (tid.clone(), mid.clone(), emoji.clone());
                let rt = rt.clone();
                rt.spawn(async move {
                    crate::conversations::manager()
                        .toggle_reaction(tid, mid, emoji)
                        .await;
                });
            });
            quickset.append(&opt);
        }
        menu.append(&quickset);

        // "More reactions" → the GTK emoji chooser (the one sanctioned divergence,
        // same class as the attachment file-picker); the picked emoji toggles.
        let more = gtk::Button::with_label(s::MORE_REACTIONS);
        more.add_css_class("flat");
        crate::testid::set_test_id(&more, ids::DM_REACTION_MORE_BUTTON);
        let chooser = gtk::EmojiChooser::new();
        chooser.set_parent(&more);
        {
            let tid = thread_id.clone();
            let mid = msg.message_id.clone();
            let rt = rt.clone();
            let pop = popover.clone();
            chooser.connect_emoji_picked(move |_, text| {
                pop.popdown();
                let (tid, mid, emoji) = (tid.clone(), mid.clone(), text.to_string());
                let rt = rt.clone();
                rt.spawn(async move {
                    crate::conversations::manager()
                        .toggle_reaction(tid, mid, emoji)
                        .await;
                });
            });
        }
        {
            let chooser = chooser.clone();
            more.connect_clicked(move |_| chooser.popup());
        }
        menu.append(&more);
    }

    if can_delete {
        // Sender-only delete: a destructive two-step inside the same flyout —
        // tapping `dm-message-delete-button` reveals the confirm prompt +
        // `dm-message-delete-confirm-button`, which posts the cooperative
        // tombstone via the shared manager.
        let del = gtk::Button::with_label(s::DELETE_MESSAGE);
        del.add_css_class("flat");
        del.add_css_class("destructive-action");
        crate::testid::set_test_id(&del, ids::DM_MESSAGE_DELETE_BUTTON);

        let prompt = gtk::Label::new(Some(s::DELETE_MESSAGE_CONFIRM_TITLE));
        prompt.add_css_class("dim-label");
        prompt.set_visible(false);

        let confirm = gtk::Button::with_label(s::DELETE_MESSAGE_CONFIRM);
        confirm.add_css_class("destructive-action");
        confirm.set_visible(false);
        crate::testid::set_test_id(&confirm, ids::DM_MESSAGE_DELETE_CONFIRM_BUTTON);
        // The cooperative tombstone posts through the same MLS-channel seam as
        // a reaction/rename/eviction — unconditional, no rail branch
        // (`Action::ConfirmDeleteMessage`). `del`/`prompt`/`confirm` are rebuilt
        // fresh with every bubble, so no stale-declaration risk.
        crate::offline_gate::declare_wire_kind(&confirm, "fauna.conversations.channel.send");

        {
            let del2 = del.clone();
            let prompt = prompt.clone();
            let confirm2 = confirm.clone();
            del.connect_clicked(move |_| {
                del2.set_visible(false);
                prompt.set_visible(true);
                confirm2.set_visible(true);
            });
        }
        {
            let tid = thread_id.clone();
            let mid = msg.message_id.clone();
            let rt = rt.clone();
            let pop = popover.clone();
            confirm.connect_clicked(move |_| {
                pop.popdown();
                let (tid, mid) = (tid.clone(), mid.clone());
                let rt = rt.clone();
                rt.spawn(async move {
                    crate::conversations::manager()
                        .delete_message(tid, mid)
                        .await;
                });
            });
        }
        menu.append(&del);
        menu.append(&prompt);
        menu.append(&confirm);
    }

    if can_flag_spam {
        // Mark-as-spam — the live `Insert` consumer (mail-spam.md § Wire shapes).
        // Trains the sealed tier-1 spam model over the retained decrypted body AND
        // writes a sealed training-history row the `mail-spam` page renders + undoes.
        // Fire-and-forget via the FaunaClient bg runtime (like a moderation
        // correction); silent no-op when mail isn't enabled.
        let spam_btn = gtk::Button::with_label(s::MARK_AS_SPAM);
        spam_btn.add_css_class("flat");
        crate::testid::set_test_id(&spam_btn, ids::DM_MESSAGE_MARK_AS_SPAM_BUTTON);
        // The page's one genuinely desensitizing gesture: training the sealed
        // tier-1 spam model reseals it and PUTs it to the mail bridge — there is
        // deliberately no server-train fallback for conversation content
        // (`Action::MarkMessageSpam`, tui's `conversations/mod.rs::wire_kind`).
        crate::offline_gate::declare_wire_kind(&spam_btn, "fauna.bridges.put_spam_model");
        let client = Rc::clone(client);
        let mid = msg.message_id.clone();
        let pop = popover.clone();
        let latest = Rc::clone(latest_by_id);
        spam_btn.connect_clicked(move |_| {
            pop.popdown();
            // Read the newest body/subject off `latest_by_id` rather than a
            // render-time capture — see the muted-reveal arm's comment in
            // `build` for why a reused bubble's closures can't trust one.
            let Some(m) = latest.borrow().get(&mid).cloned() else {
                return;
            };
            client.mark_message_spam(mid.0.clone(), m.body, m.subject_line);
        });
        menu.append(&spam_btn);
    }

    popover.set_child(Some(&menu));
    popover.set_parent(&btn);
    {
        let pop = popover.clone();
        btn.connect_clicked(move |_| pop.popup());
    }
    btn
}
