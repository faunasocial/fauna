use adw::prelude::*;
use fauna_ui_ids as ids;
use std::rc::Rc;
use std::sync::Arc;

use tokio::runtime::Handle;

use crate::client::FaunaClient;
use crate::feed::host::LinuxFeedManager;
use fauna_client_bluesky::bluesky::BlueskyPost;
use fauna_feed::PostSummary;

#[cfg(feature = "payments")]
use super::post_list::build_tip_row;
use super::post_list::{
    append_tag_chips, build_post_image, build_post_proxied_image, build_post_video,
    build_reply_dialog, engagement_indicator_content, format_timestamp,
};
use crate::i18n::strings::{common, feed};
use crate::views::document;

/// Build a detail view for a single feed post (a snapshot [`PostSummary`]).
///
/// Shows full content (body + folded quoted-post / media embeds, walked from
/// `post.document`), metadata badges, and action buttons (Reply / Repost). The
/// `on_back` callback is invoked when the user presses the Back button.
pub fn build_post_detail<F: Fn() + 'static>(
    post: &PostSummary,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
    rt: &Handle,
    on_back: F,
) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    crate::testid::set_test_id(&outer, ids::FEED_POST_DETAIL_DIALOG);

    // ── Header with Back button and title ────────────────────────────────
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(common::FEED))));

    let back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
    back_btn.set_tooltip_text(Some(crate::i18n::strings::common::BACK));
    back_btn.connect_clicked(move |_| on_back());
    header.pack_start(&back_btn);

    outer.append(&header);

    // Scrollable content area.
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);

    // ── Legal-takedown tombstone (moderation.md § Categories & enforcement
    //    item 1): the nest withheld the sealed body under a legal obligation,
    //    so the detail collapses — like the DM bubble's identical branch
    //    (`views/conversations/message_bubble.rs`) — to the shared localized
    //    tombstone rendered IN PLACE OF the (empty) body: no author, no
    //    badges, no tags, no image, no quoted-post embed, no actions. Checked
    //    before any of those paint, and returns early — never a blank dialog
    //    and never the page error surface standing in for a body. ──────────
    if let Some(reference) = &post.legal_takedown_ref {
        let tombstone = gtk::Label::new(Some(
            &crate::i18n::strings::moderation::legal_takedown::tombstone(reference),
        ));
        tombstone.add_css_class("dim-label");
        tombstone.set_halign(gtk::Align::Start);
        tombstone.set_wrap(true);
        tombstone.set_xalign(0.0);
        crate::testid::set_test_id(&tombstone, ids::FEED_POST_DETAIL_BODY);
        content.append(&tombstone);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&content)
            .build();
        outer.append(&scrolled);
        return outer;
    }

    // ── Author ───────────────────────────────────────────────────────────
    let author_label = gtk::Label::new(Some(&super::post_list::author_label_text(post)));
    author_label.set_halign(gtk::Align::Start);
    author_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    author_label.add_css_class("heading");
    crate::testid::set_test_id(&author_label, ids::FEED_POST_DETAIL_AUTHOR);
    content.append(&author_label);

    // ── Unverified-source badge ──────────────────────────────────────────
    // Shown iff THIS client failed to verify the decoded post's signed
    // envelope (`PostSummary::verification == Failed`; security.md § Client
    // display of unverified content). Post-detail is a decode path, so unlike a
    // feed-list card this can legitimately be `Verified`/`Failed`.
    if let Some(badge) = super::post_list::build_unverified_badge(post.verification) {
        badge.set_halign(gtk::Align::Start);
        content.append(&badge);
    }

    // ── Delegated-origin badge (the D10 audit surface) ───────────────────
    // Shown iff an external app authored this post as the account, through the
    // delegated authoring sub-key (`PostSummary::authoring_origin == Delegated`;
    // atproto-pds-full.md § D10 → *Audit*). Mirrors the list card, so opening a
    // post never loses the marker the feed showed.
    if let Some(badge) = super::post_list::build_delegated_origin_badge(post.authoring_origin) {
        badge.set_halign(gtk::Align::Start);
        content.append(&badge);
    }

    // ── Source icon badge(s) ─────────────────────────────────────────────
    let source_badges = super::post_list::build_protocol_badges(&post.source);
    if !source_badges.is_empty() {
        let source_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for badge in &source_badges {
            source_row.append(badge);
        }
        let source_label = gtk::Label::new(Some(&post.source));
        source_label.add_css_class("dim-label");
        source_row.append(&source_label);
        source_row.set_halign(gtk::Align::Start);
        content.append(&source_row);
    }

    // ── Timestamp ────────────────────────────────────────────────────────
    let time_label = gtk::Label::new(Some(&format_timestamp(post.timestamp)));
    time_label.set_halign(gtk::Align::Start);
    time_label.add_css_class("dim-label");
    time_label.add_css_class("caption");
    content.append(&time_label);

    // ── Metadata badges row ──────────────────────────────────────────────
    if post.has_media || post.is_reply {
        let badges = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        badges.set_margin_top(4);

        if post.has_media {
            let badge = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let icon = gtk::Image::from_icon_name("image-x-generic-symbolic");
            icon.set_pixel_size(14);
            let label = gtk::Label::new(Some(feed::post::HAS_MEDIA));
            label.add_css_class("dim-label");
            label.add_css_class("caption");
            badge.append(&icon);
            badge.append(&label);
            badges.append(&badge);
        }

        if post.is_reply {
            let badge = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let icon = gtk::Image::from_icon_name("mail-reply-sender-symbolic");
            icon.set_pixel_size(14);
            let label = gtk::Label::new(Some(common::REPLY));
            label.add_css_class("dim-label");
            label.add_css_class("caption");
            badge.append(&icon);
            badge.append(&label);
            badges.append(&badge);
        }

        content.append(&badges);
    }

    // ── Separator ────────────────────────────────────────────────────────
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    // ── Full body — walk the shared `RenderDocument` (`PostSummary.document`,
    //    render-model.md § D6), the SAME walker the Conversations page uses. No
    //    bespoke flat-text/`> `-blockquote rendering; markdown structure is honoured
    //    uniformly; each remote image paints from its authoritative `revealed` flag
    //    (blocked placeholder or fetched picture — D3). Tags are the snapshot facet
    //    list (their own `tag-chip` ids). ──
    let body_box = document::render_to_widget(
        &post.document,
        rt,
        &crate::media_loads::MediaScope::Feed(Arc::clone(manager)),
    );
    crate::testid::set_test_id(&body_box, ids::FEED_POST_DETAIL_BODY);
    content.append(&body_box);
    // Reveal button appears iff the document still has a blocked remote image; it DISPATCHES
    // `FeedManager::reveal_remote_images(post_id)` (the same dispatch the post card uses —
    // D3). The manager re-emits, the feed observer rebuild repaints this open detail (its
    // embed signature includes `has_blocked_remote_images`, so the flip triggers a re-walk),
    // and the button is absent from the rebuilt detail. No client-side reveal state remains.
    if post.document.has_blocked_remote_images() {
        let m = Arc::clone(manager);
        let pid = post.post_id.clone();
        document::attach_reveal_button(&content, move || {
            m.reveal_remote_images(pid.clone());
        });
    }
    append_tag_chips(&content, &post.tags);

    // ── Media image (`post-image`) — extracted from the document `Image` block
    //    (render-model.md § D6) and painted through the client blob loader (the
    //    shared walker has no blob loader). The folded `QuotedPost` card is
    //    already painted by the body walker above. Closes the prior gap where the
    //    detail showed only a "has media" badge and no image. ──
    //    A bridged post's own picture / video (render-model.md § D6c) takes
    //    the same slot when there is no blob one, as on the list card.
    if let Some(hash) = post.document.first_image_hash() {
        content.append(&build_post_image(hash, manager, client));
    } else if let Some(path) = post.document.proxied_post_image() {
        content.append(&build_post_proxied_image(path, manager, client));
    }
    // `video-thumbnail` — the D6b `Video` sibling of `Image` (render-model.md
    // § D6b); mutually exclusive with the image branch above, same fold.
    if let Some(hash) = post.document.first_video_hash() {
        content.append(&build_post_video(hash));
    } else if let Some(path) = post.document.proxied_post_video() {
        content.append(&build_post_video(path));
    }

    // ── Tip surface (`monetization.md` § Tips) — the same surface the list
    //    card paints, resolved lazily by the same fire-once trigger. Gated on
    //    the RENDER for the reason `build_tip_row`'s own doc comment gives:
    //    the record it reads is ungated and inert, so an ungated call here
    //    would still ship the ids (`dynamic-features.md` § Platform-family
    //    surface excision). ─────────────────────────────────────────────────
    #[cfg(feature = "payments")]
    if let Some(tip_row) = build_tip_row(post) {
        content.append(&tip_row);
    }

    // ── Separator ────────────────────────────────────────────────────────
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    // ── Action buttons ───────────────────────────────────────────────────
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_margin_top(8);

    let reply_btn = gtk::Button::with_label(common::REPLY);
    reply_btn.add_css_class("suggested-action");
    actions.append(&reply_btn);

    let repost_btn = gtk::Button::with_label(feed::post::REPOST);
    crate::testid::set_test_id(&repost_btn, ids::FEED_REPOST_BUTTON);
    crate::offline_gate::declare_wire_kind(&repost_btn, "fauna.posts.interact");
    actions.append(&repost_btn);

    // "View Thread" button for Bluesky posts.
    if post.source == "bluesky" {
        let thread_btn = gtk::Button::with_label(feed::post::VIEW_THREAD);
        thread_btn.add_css_class("flat");
        actions.append(&thread_btn);

        let c = Rc::clone(client);
        let pid = post.post_id.clone();
        thread_btn.connect_clicked(move |_| {
            c.fetch_bluesky_thread(&pid);
        });
    }

    content.append(&actions);

    // Wire Reply button.
    {
        let c = Rc::clone(client);
        let pid = post.post_id.clone();
        reply_btn.connect_clicked(move |btn| {
            let dialog = build_reply_dialog(&c, &pid);
            if let Some(root) = btn.root()
                && let Some(win) = root.downcast_ref::<gtk::Window>()
            {
                dialog.set_transient_for(Some(win));
            }
            dialog.present();
        });
    }

    // Wire Repost button.
    {
        let c = Rc::clone(client);
        let pid = post.post_id.clone();
        repost_btn.connect_clicked(move |_| {
            c.interact_with_post(&pid, "repost", None);
        });
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&content)
        .build();

    outer.append(&scrolled);
    outer
}

/// Build a thread view showing the full Bluesky thread context.
///
/// `posts` is the flat `bluesky.feed.thread` list: ancestors (oldest first),
/// the focal post, then its direct replies. `focal_index` is the nest-named
/// focal post; ancestors render dim, the focal is highlighted, replies render
/// normally.
pub fn build_thread_view<F: Fn() + 'static>(
    posts: &[BlueskyPost],
    focal_idx: usize,
    client: &Rc<FaunaClient>,
    on_back: F,
) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // ── Header with Back button ───────────────────────────────────────────
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(feed::post::THREAD))));

    let back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
    back_btn.set_tooltip_text(Some(crate::i18n::strings::common::BACK));
    back_btn.connect_clicked(move |_| on_back());
    header.pack_start(&back_btn);

    outer.append(&header);

    // ── Thread content ────────────────────────────────────────────────────
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    if posts.is_empty() {
        let status = adw::StatusPage::builder()
            .title(feed::post::NO_THREAD_DATA)
            .description(feed::post::NO_THREAD_DESC)
            .icon_name("dialog-information-symbolic")
            .vexpand(true)
            .build();
        content.append(&status);
    } else {
        for (i, post) in posts.iter().enumerate() {
            let card = build_thread_post_card(post, client);

            if i == focal_idx {
                // Highlight the focal post.
                card.add_css_class("card");
            } else if i < focal_idx {
                // Ancestor: dim styling.
                card.add_css_class("dim-label");
            }

            // Add a thread connector line between posts.
            if i > 0 {
                let connector = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                connector.set_halign(gtk::Align::Center);
                connector.set_height_request(16);
                let line = gtk::Separator::new(gtk::Orientation::Vertical);
                connector.append(&line);
                content.append(&connector);
            }

            content.append(&card);
        }
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&content)
        .build();

    outer.append(&scrolled);
    outer
}

/// Build a single post card for the thread view from a typed [`BlueskyPost`].
fn build_thread_post_card(post: &BlueskyPost, _client: &Rc<FaunaClient>) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.set_margin_top(8);
    card.set_margin_bottom(8);
    card.set_margin_start(16);
    card.set_margin_end(16);

    // Author line: display_name or handle.
    let handle = post.author_handle.as_str();
    let author = post
        .author_display_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(if handle.is_empty() { "Unknown" } else { handle });

    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let author_label = gtk::Label::new(Some(author));
    author_label.set_halign(gtk::Align::Start);
    author_label.set_hexpand(true);
    author_label.add_css_class("heading");
    top_line.append(&author_label);

    if !handle.is_empty() && handle != author {
        let handle_label = gtk::Label::new(Some(&format!("@{}", handle)));
        handle_label.add_css_class("dim-label");
        handle_label.add_css_class("caption");
        handle_label.set_halign(gtk::Align::End);
        top_line.append(&handle_label);
    }

    // Bluesky icon.
    let bsky_icon = gtk::Image::from_icon_name("weather-clear-symbolic");
    bsky_icon.set_pixel_size(14);
    bsky_icon.set_tooltip_text(Some("Bluesky"));
    top_line.append(&bsky_icon);

    card.append(&top_line);

    // Post text.
    let text = post.text.as_str();

    if !text.is_empty() {
        let body_label = gtk::Label::new(Some(text));
        body_label.set_halign(gtk::Align::Start);
        body_label.set_wrap(true);
        body_label.set_xalign(0.0);
        body_label.set_selectable(true);
        card.append(&body_label);
    }

    // Engagement counts.
    if let Some(stats) =
        build_thread_engagement_stats(post.reply_count, post.repost_count, post.like_count)
    {
        card.append(&stats);
    }

    card
}

/// The engagement row for a Bluesky-bridged thread-ancestor card: icon + bare
/// number per nonzero count (`feed.md` § Interaction bar shape, ratified
/// 2026-06-27) — not the main feed card's spelled-out-English/always-plural
/// shape, and not the main card's live `interaction_button`s either. Static
/// (no button, no click, no ui.yaml test id): a thread ancestor's `post.id`
/// is the Bluesky AT-URI (`fauna_bridge_atproto::translate::id`), not a
/// Fauna post id, so no `fauna.posts.interact` path exists here — only the
/// focal post, once navigated to, gets the live interactive bar. Returns
/// `None` when every count is 0 (nothing to show, matching the prior
/// hand-rolled behavior of omitting the whole row).
fn build_thread_engagement_stats(
    reply_count: u64,
    repost_count: u64,
    like_count: u64,
) -> Option<gtk::Box> {
    if reply_count == 0 && repost_count == 0 && like_count == 0 {
        return None;
    }

    let stats = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    stats.set_margin_top(4);

    if reply_count > 0 {
        let indicator =
            engagement_indicator_content("mail-reply-sender-symbolic", reply_count as i64);
        indicator.set_tooltip_text(Some(common::REPLY));
        stats.append(&indicator);
    }
    if repost_count > 0 {
        let indicator =
            engagement_indicator_content("media-playlist-repeat-symbolic", repost_count as i64);
        indicator.set_tooltip_text(Some(feed::post::REPOST));
        stats.append(&indicator);
    }
    if like_count > 0 {
        let indicator = engagement_indicator_content("emblem-favorite-symbolic", like_count as i64);
        indicator.set_tooltip_text(Some(feed::LIKE_TOOLTIP));
        stats.append(&indicator);
    }

    Some(stats)
}

#[cfg(test)]
mod tests {
    use super::super::first_label;
    use super::*;

    /// Nonzero counts render icon + bare number each (feed.md § Interaction
    /// bar shape) — not the old hand-rolled "N replies"/"N reposts"/"N likes"
    /// English strings — in reply/repost/like order, skipping any zero count.
    #[test]
    fn thread_engagement_stats_show_icon_and_bare_number() {
        crate::testid::run_on_gtk_thread(|| {
            let stats =
                build_thread_engagement_stats(1, 0, 5).expect("nonzero counts render a row");
            let mut child = stats.first_child();
            let mut counts = Vec::new();
            while let Some(indicator) = child {
                let label = first_label(&indicator).expect("count label present");
                counts.push(label.text().to_string());
                child = indicator.next_sibling();
            }
            // reply=1, repost=0 (omitted), like=5.
            assert_eq!(counts, vec!["1", "5"]);
        });
    }

    /// All-zero counts render no row at all — nothing to show on a fresh
    /// thread ancestor.
    #[test]
    fn thread_engagement_stats_hidden_when_all_zero() {
        crate::testid::run_on_gtk_thread(|| {
            assert!(build_thread_engagement_stats(0, 0, 0).is_none());
        });
    }
}
