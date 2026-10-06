//! Thread header strip above the messages list.
//!
//! Mirror of `apps/fauna-windows/.../Controls/ThreadHeader.xaml{,.cs}`.
//! Layout: title + protocol-icon | rename-button (cap-gated) | overflow,
//! with a row of `thread-member-chip[i]` + `thread-add-participant-button`
//! (cap-gated) below.
//!
//! Per spec section 4 + capabilities matrix:
//! - `thread-rename-button` visible iff `caps.supports_rename` (MLS groups).
//! - `thread-add-participant-button` visible iff `caps.supports_membership_change`
//!   (Fauna 1:1 + groups, Smtp, ActivityPub).
//!
//! Chip IDs sit on the inner Label widget per the windows lesson — Box
//! containers don't reliably expose their accessible-description property
//! to AT-SPI when nested.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use fauna_conversations::{ThreadCapabilities, TypedAddress, snapshot::ThreadDetail};
use fauna_core::data::MemberReview;
use fauna_core::identity::ActorId;

type RemoveMemberCb = Rc<RefCell<Option<Box<dyn Fn(TypedAddress)>>>>;
type KeepMemberCb = Rc<RefCell<Option<Box<dyn Fn(ActorId)>>>>;
type VoidCb = Rc<RefCell<Option<Box<dyn Fn()>>>>;

#[derive(Clone)]
pub struct ThreadHeader {
    pub root: gtk::Box,
    title_label: gtk::Label,
    protocol_icon: gtk::Label,
    members_box: gtk::Box,
    rename_btn: gtk::Button,
    add_participant_btn: gtk::Button,
    room_class_label: gtk::Label,
    /// Holds `conversation-guardian-state` while the nest reports one for the
    /// room's peer — emptied, never hidden, so no stale marker lingers.
    guardian_slot: gtk::Box,
    room_settings_btn: gtk::Button,
    _on_rename: VoidCb,
    _on_add_participant: VoidCb,
    _on_room_settings: VoidCb,
    on_remove_member: RemoveMemberCb,
    on_keep_member: KeepMemberCb,
}

impl ThreadHeader {
    pub fn new() -> Self {
        // `accessible_role(Group)`: a plain `gtk::Box` defaults to role
        // `Generic`, which Linux AT-SPI prunes from the tree, so `thread-header`
        // never resolves for `is_visible`/`count` even though its children
        // (rename / add-participant buttons, member chips) do. `Group` keeps the
        // container discoverable — same fix as `contacts-view`
        // (views/contacts/list.rs; tracked internally).
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.set_margin_top(8);
        root.set_margin_bottom(8);
        crate::testid::set_test_id(&root, ids::THREAD_HEADER);

        // Top row: title + protocol-icon | rename-button | overflow.
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let title_label = gtk::Label::new(None);
        title_label.set_halign(gtk::Align::Start);
        title_label.set_hexpand(true);
        title_label.add_css_class("title-3");
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        top.append(&title_label);

        let protocol_icon = gtk::Label::new(None);
        protocol_icon.add_css_class("dim-label");
        crate::testid::set_test_id(&protocol_icon, ids::PROTOCOL_ICON);
        top.append(&protocol_icon);

        let rename_btn =
            gtk::Button::with_label(crate::i18n::strings::conversations::unified::THREAD_RENAME);
        rename_btn.add_css_class("flat");
        crate::testid::set_test_id(&rename_btn, ids::THREAD_RENAME_BUTTON);
        // The click only opens `rename_overlay::show`'s confirm dialog, but the
        // ceremony behind it can end in exactly one kind — `supports_rename` is
        // true only for a bound FaunaMls group (`capabilities.rs`), so
        // `thread-rename-confirm` (tagged via `tag_response_button` on a
        // transient `adw::MessageDialog` with no persistent widget reference)
        // never needs a different kind — so the ENTRY is what declares. Same
        // idiom as `admin.rs::build_factory_reset_section`'s
        // `ADMIN_FACTORY_RESET_BUTTON`.
        crate::offline_gate::declare_wire_kind(&rename_btn, "fauna.conversations.channel.send");
        top.append(&rename_btn);

        // The room's class, stated on the header ("the class is on the thread
        // header", `conversation-rooms.md` § The three classes), and beside it
        // the policy editor's door. Both are built unconditionally and hidden
        // where the rail models no room; the button is GREYED — never hidden —
        // when the viewer's role may not set policy (§ Architectural rules 5).
        let room_class_label = gtk::Label::new(None);
        room_class_label.add_css_class("dim-label");
        crate::testid::set_test_id(&room_class_label, ids::THREAD_ROOM_CLASS);
        top.append(&room_class_label);
        let guardian_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        top.append(&guardian_slot);

        let room_settings_btn = gtk::Button::with_label(
            crate::i18n::strings::conversations::unified::THREAD_ROOM_SETTINGS,
        );
        room_settings_btn.add_css_class("flat");
        crate::testid::set_test_id(&room_settings_btn, ids::THREAD_ROOM_SETTINGS_BUTTON);
        // Every staged change Save commits is a policy commit on the channel —
        // the same kind rename declares, and constant for all five edits, so
        // the ENTRY declares it (the `rename_btn` idiom above).
        crate::offline_gate::declare_wire_kind(
            &room_settings_btn,
            "fauna.conversations.channel.send",
        );
        top.append(&room_settings_btn);

        root.append(&top);

        // Members row: chips horizontally + add-participant-button at end.
        let members_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let members_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        members_box.set_hexpand(true);
        members_row.append(&members_box);

        let add_participant_btn = gtk::Button::with_label(
            crate::i18n::strings::conversations::unified::THREAD_ADD_PARTICIPANT,
        );
        add_participant_btn.add_css_class("flat");
        crate::testid::set_test_id(&add_participant_btn, ids::THREAD_ADD_PARTICIPANT_BUTTON);
        members_row.append(&add_participant_btn);

        root.append(&members_row);

        let on_rename: VoidCb = Rc::new(RefCell::new(None));
        let on_add_participant: VoidCb = Rc::new(RefCell::new(None));

        {
            let cb = on_rename.clone();
            rename_btn.connect_clicked(move |_| {
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            });
        }
        {
            let cb = on_add_participant.clone();
            add_participant_btn.connect_clicked(move |_| {
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            });
        }
        let on_room_settings: VoidCb = Rc::new(RefCell::new(None));
        {
            let cb = on_room_settings.clone();
            room_settings_btn.connect_clicked(move |_| {
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            });
        }

        Self {
            root,
            title_label,
            protocol_icon,
            members_box,
            rename_btn,
            add_participant_btn,
            room_class_label,
            guardian_slot,
            room_settings_btn,
            _on_rename: on_rename,
            _on_add_participant: on_add_participant,
            _on_room_settings: on_room_settings,
            on_remove_member: Rc::new(RefCell::new(None)),
            on_keep_member: Rc::new(RefCell::new(None)),
        }
    }

    /// Re-render from the current ThreadDetail snapshot.
    pub fn render(&self, detail: &ThreadDetail, member_reviews: &[MemberReview]) {
        // Shared empty-label fallback (`fauna_core::format::thread_label_display`):
        // a blank thread label renders the canonical `(no subject)` placeholder.
        let title_text = fauna_core::format::thread_label_display(&detail.label)
            .resolve(crate::i18n::strings::lookup);
        self.title_label.set_text(&title_text);
        self.protocol_icon
            .set_text(crate::source_glyph::source_glyph_emoji(detail.glyph));

        // Member chips — clear + rebuild. When the rail supports membership
        // change, each chip is clickable and removes its participant
        // (`thread-member-chip[i] → manager.remove_participant`, conversations.md
        // § User actions); capability-gated, never rail-branched (Rule 5).
        while let Some(child) = self.members_box.first_child() {
            self.members_box.remove(&child);
        }
        // A chip removes its participant when the rail's membership is mutable
        // AND the viewer's role in a governed room may remove one — the roles
        // table applied in shared Rust (`RoomSnapshot::gate`), never re-derived
        // here (`conversation-rooms.md` § Roles and authorization).
        let membership_mutable = detail.capabilities.supports_membership_change;
        let removable = membership_mutable && detail.capabilities.can_remove_members;
        for (i, (display, addr)) in detail
            .participant_displays
            .iter()
            .zip(detail.participants.iter())
            .enumerate()
        {
            // The member's role on a governed room: the `role` attribute a
            // driver reads off the chip, and the localized owner/admin mark in
            // its text. `None` on a policy-less room and on every non-room thread —
            // there are no roles to mark, not "everyone is a member".
            let role = detail
                .room
                .as_ref()
                .and_then(|room| room.members.get(i))
                .and_then(|member| member.role);
            let display = &fauna_conversations::member_chip_text(display, role);
            // The post-succession review pair, scoped INSIDE this chip
            // (`identity-succession.md` § Propagation → *MLS groups*, item
            // 3a). `None` when the participant carries no Fauna actor id
            // (a mail/ActivityPub informational chip) or is not raised.
            let reviewed_person = addr
                .person_actor_id()
                .filter(|person| fauna_core::data::is_under_review(member_reviews, person));
            let greyed = membership_mutable && !removable;
            let (chip, chip_label) =
                build_member_chip(display, removable, greyed, role, reviewed_person, {
                    let cb = self.on_keep_member.clone();
                    move |person| {
                        if let Some(f) = cb.borrow().as_ref() {
                            f(person);
                        }
                    }
                });
            if removable {
                // On the LABEL — the widget carrying `thread-member-chip` — not
                // the pill around it: the element a driver (and the automation
                // gate) resolves must be the one that acts, and a gesture on the
                // box left the id-carrying label with nothing to press, so no
                // tier_3 journey could ever drive a removal here.
                let gesture = gtk::GestureClick::new();
                let cb = self.on_remove_member.clone();
                let addr = addr.clone();
                gesture.connect_released(move |_, _, _, _| {
                    if let Some(f) = cb.borrow().as_ref() {
                        f(addr.clone());
                    }
                });
                chip_label.add_controller(gesture);
            } else if !membership_mutable {
                // (A `greyed` chip — the rail's membership is mutable but the
                // viewer's ROLE may not remove, a plain member of a governed
                // room — takes neither branch: it is still the Remove control,
                // greyed by `build_member_chip`, never hidden and never demoted
                // to this address popover, which a user and a driver both read
                // as a live control — `ui/conversations.md` § Architectural
                // rules 5.)
                //
                // Informational chip (mail): clicking reveals the full
                // address / contact in a popover — never removes
                // (`conversations.md` § Participants vs reply recipients).
                let popover = gtk::Popover::new();
                popover.set_autohide(true);
                let addr_label = gtk::Label::new(Some(&addr.display()));
                addr_label.set_margin_start(8);
                addr_label.set_margin_end(8);
                addr_label.set_margin_top(4);
                addr_label.set_margin_bottom(4);
                addr_label.set_selectable(true);
                popover.set_child(Some(&addr_label));
                popover.set_parent(&chip);
                let gesture = gtk::GestureClick::new();
                gesture.connect_released(move |_, _, _, _| popover.popup());
                chip.add_controller(gesture);
            }
            self.members_box.append(&chip);
        }

        // `add-participant-confirm` lives in a transient `adw::MessageDialog`
        // (add_participant_overlay.rs) with no persistent widget reference, and
        // — unlike rename above — its wire kind is NOT constant: only a bound
        // FaunaMls group's add reaches the wire
        // (`fauna.conversations.keypackage.fetch`); a FaunaMls 1:1 forks a fresh
        // thread and a non-FaunaMls rail that also shows this button (a bridge whose
        // declared vector offers `supports_membership_change`) mutates the snapshot in place,
        // issuing nothing (`fauna_conversations::capabilities::is_in_place_mls_group`,
        // the same discriminant `AddParticipantState::in_place_mls_group` stamps
        // at `manager.rs::open_add_participant`). So the ENTRY re-declares every
        // render off the CURRENTLY-VIEWED thread's own `(rail, flavor)` — the
        // paint deciding — rather than declaring once at
        // construction like rename.
        //
        // ⚠ Known gap: declaring nothing on the `false` leg (rule 3 — no wire
        // call, no kind) cannot retract an EARLIER `true`-leg declaration on
        // this same persistent button, because `declare_wire_kind` has no
        // "undeclare" primitive. A session that views a bound-MLS-group thread
        // and then a non-in-place thread (a FaunaMls 1:1, or an ActivityPub thread)
        // without the app restarting in between may see this button stay
        // OnlineOnly-gated for the second thread even though its add-participant
        // issues nothing. Not fixed here — fixing it needs a change to the
        // shared `offline_gate.rs` registry, out of this sweep's scope.
        if fauna_conversations::capabilities::is_in_place_mls_group(
            detail.rail,
            detail.flavor.clone(),
        ) {
            crate::offline_gate::declare_wire_kind(
                &self.add_participant_btn,
                "fauna.conversations.keypackage.fetch",
            );
        }

        self.apply_room(detail);
        self.apply_capabilities(&detail.capabilities);
    }

    /// The room's class statement and the editor's door — read off the
    /// projected room, never computed here (`conversation-rooms.md` § The
    /// three classes: the class is a pure function of the member set,
    /// computed in shared Rust). Both leave the tree where the rail models no
    /// room; where it does, the button is painted and merely greyed off
    /// `can_set_policy` — a policy-less room greys it too, having no policy to
    /// edit.
    fn apply_room(&self, detail: &ThreadDetail) {
        while let Some(child) = self.guardian_slot.first_child() {
            self.guardian_slot.remove(&child);
        }
        match detail.room.as_ref() {
            Some(room) => {
                self.room_class_label.set_text(room.class.label());
                crate::testid::set_test_attr(
                    &self.room_class_label,
                    "class",
                    room.class.attr_token(),
                );
                self.room_class_label.set_visible(true);
                // The family gate's marker in the detail, the row's twin — the
                // thread below it stays fully readable (`family-safety.md`
                // § The bridge-DM gate).
                if let Some(state) = detail.guardian_state {
                    self.guardian_slot
                        .append(&super::list::guardian_state_label(state));
                }
                self.room_settings_btn.set_visible(true);
                self.room_settings_btn
                    .set_sensitive(detail.capabilities.can_set_policy);
            }
            None => {
                self.room_class_label.set_visible(false);
                self.room_settings_btn.set_visible(false);
            }
        }
    }

    pub fn on_room_settings(&self, f: impl Fn() + 'static) {
        *self._on_room_settings.borrow_mut() = Some(Box::new(f));
    }

    fn apply_capabilities(&self, caps: &ThreadCapabilities) {
        // Render unconditionally; gate on visibility
        // (capability gating, never rail branches).
        self.rename_btn.set_visible(caps.supports_rename);
        self.add_participant_btn
            .set_visible(caps.supports_membership_change);
        // ...and GREY it — never hide it — when the viewer's role in a
        // governed room may not invite (`ui/conversations.md`
        // § Architectural rules 5; the roles table is applied in shared Rust).
        self.add_participant_btn.set_sensitive(caps.can_invite);
    }

    pub fn on_rename(&self, f: impl Fn() + 'static) {
        *self._on_rename.borrow_mut() = Some(Box::new(f));
    }

    pub fn on_add_participant(&self, f: impl Fn() + 'static) {
        *self._on_add_participant.borrow_mut() = Some(Box::new(f));
    }

    /// Set the callback fired when a member chip is clicked on a
    /// membership-change-capable thread — wired to `manager.remove_participant`.
    pub fn on_remove_member(&self, f: impl Fn(TypedAddress) + 'static) {
        *self.on_remove_member.borrow_mut() = Some(Box::new(f));
    }

    /// Set the callback fired when a flagged member's Keep button is
    /// pressed — the pair's Keep half (`identity-succession.md` §
    /// Propagation → *MLS groups*, item 3a; Remove is the chip itself).
    pub fn on_keep_member(&self, f: impl Fn(ActorId) + 'static) {
        *self.on_keep_member.borrow_mut() = Some(Box::new(f));
    }
}

/// The chip pill and, separately, the label inside it that carries
/// `thread-member-chip` — the widget a removal gesture must sit on.
///
/// `greyed`: the chip is a Remove control the viewer's role may not use — the
/// id-carrying label goes insensitive, so the automation gate refuses to drive
/// it, while the review pair's Keep beside it (an account-plane write, not a
/// removal) stays live.
fn build_member_chip(
    text: &str,
    removable: bool,
    greyed: bool,
    role: Option<fauna_conversations::RoomRole>,
    reviewed_person: Option<ActorId>,
    on_keep_click: impl Fn(ActorId) + 'static,
) -> (gtk::Box, gtk::Label) {
    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    chip.add_css_class("pill");
    // The pill is the chip's automation SCOPE while its id rides the label
    // below: the label is what a driver presses and a role greys, and the
    // review pair renders beside it here, so a `scope="thread-member-chip[i]"`
    // query must continue from the pill — a label has no children to find the
    // pair among (tui states the same relation as `Element::within`).
    crate::testid::set_test_scope(&chip, ids::THREAD_MEMBER_CHIP);

    let label = gtk::Label::new(Some(text));
    label.set_margin_start(8);
    label.set_margin_end(8);
    label.set_margin_top(2);
    label.set_margin_bottom(2);
    crate::testid::set_test_id(&label, ids::THREAD_MEMBER_CHIP);
    if greyed {
        label.set_sensitive(false);
    }
    // The chip's `role` state — a state of an existing element, ui.yaml's
    // ratified shape for one (`recipient-resolve-status`'s `state`), so a
    // driver reads the role off the chip it already addresses.
    if let Some(role) = role {
        crate::testid::set_test_attr(&label, "role", role.attr_token());
    }
    if removable {
        // The chip's click (wired in `render` above) → `manager.remove_participant`
        // posts through the same `post_app_message` seam as a reaction/delete/
        // rename — unconditional, no rail branch (`Action::RemoveMember`,
        // `apps/fauna-tui/src/conversations/mod.rs::wire_kind`). An informational
        // (mail) chip is never removable, so it never reaches here — and since
        // chips are torn down + rebuilt fresh every `render` call, there is no
        // stale-declaration risk across a rail switch the way the persistent
        // `add_participant_btn`/`send_btn` have.
        crate::offline_gate::declare_wire_kind(&label, "fauna.conversations.channel.send");
    }
    chip.append(&label);

    // The post-succession review pair, scoped INSIDE this chip — present only
    // while the participant carries an open review item
    // (`identity-succession.md` § Propagation → *MLS groups*). Remove is
    // deliberately NOT re-rendered: the chip itself already is it (a tap
    // posts the remove Commit when `removable`), exactly as
    // `nest-trust-grant-revoke` is the grant plane's Remove half.
    if let Some(person) = reviewed_person {
        let mark = gtk::Label::new(Some(
            crate::i18n::strings::conversations::detail::MEMBER_UNATTESTED_MARK,
        ));
        mark.set_margin_start(4);
        mark.add_css_class("caption");
        mark.add_css_class("warning");
        crate::testid::set_test_id(&mark, ids::THREAD_MEMBER_UNATTESTED_MARK);
        chip.append(&mark);

        let keep =
            gtk::Button::with_label(crate::i18n::strings::conversations::detail::MEMBER_KEEP);
        keep.add_css_class("flat");
        keep.set_margin_start(2);
        crate::testid::set_test_id(&keep, ids::THREAD_MEMBER_KEEP_BUTTON);
        crate::offline_gate::declare_wire_kind(&keep, "fauna.account.state.put");
        keep.connect_clicked(move |_| on_keep_click(person));
        chip.append(&keep);
    }

    (chip, label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_conversations::address::Rail;
    use fauna_conversations::capabilities::derive_capabilities;
    use fauna_conversations::compose::ComposeState;
    use fauna_conversations::room::{
        HistoryPolicy, JoinRule, PrincipalKind, RoomClass, RoomMemberSnapshot, RoomPolicySnapshot,
        RoomRole, RoomSnapshot,
    };
    use fauna_conversations::thread::{ThreadFlavor, ThreadId};
    use fauna_core::data::MemberUnattestedReason;

    fn detail_with_one_participant(actor_id: ActorId) -> ThreadDetail {
        let rail = Rail::FaunaMls;
        let flavor = ThreadFlavor::MlsGroup;
        ThreadDetail {
            thread_id: ThreadId("t-1".into()),
            rail,
            glyph: rail.glyph(),
            flavor: flavor.clone(),
            label: "test group".into(),
            participants: vec![TypedAddress::Fauna {
                handle: "alice".into(),
                actor_id,
            }],
            participant_displays: vec!["alice".into()],
            capabilities: derive_capabilities(rail, flavor),
            messages: Vec::new(),
            compose: ComposeState::default(),
            selected_message_id: None,
            bridge: None,
            guardian_state: None,
            room: None,
        }
    }

    /// The load-bearing rendering (`identity-succession.md` § Propagation →
    /// *MLS groups*, item 3a): a flagged member's chip carries the review
    /// pair, scoped inside it, with no Remove twin (the chip itself is
    /// Remove).
    ///
    /// Resolved the way a scoped query resolves — from the chip's id through
    /// the container it declared — never by a flat search of the header. The
    /// flat form stayed green while `thread-member-chip` sat on a childless
    /// label and no `scope="thread-member-chip[i]"` query could reach the pair.
    #[test]
    fn a_flagged_member_gets_the_review_pair_scoped_inside_their_chip() {
        crate::testid::run_on_gtk_thread(|| {
            let person = ActorId([7u8; 32]);
            let header = ThreadHeader::new();
            let reviews = vec![MemberReview {
                person,
                reasons: vec![MemberUnattestedReason::CompromiseWindow],
            }];
            header.render(&detail_with_one_participant(person), &reviews);

            let root: gtk::Widget = header.root.clone().upcast();
            let chip = crate::automation::find::find_in(&root, ids::THREAD_MEMBER_CHIP)
                .expect("the member has a chip");
            let scope = crate::automation::find::scope_container(chip, ids::THREAD_MEMBER_CHIP);
            assert!(
                crate::automation::find::find_in(&scope, ids::THREAD_MEMBER_UNATTESTED_MARK)
                    .is_some(),
                "a flagged member's chip must carry the review mark"
            );
            assert!(
                crate::automation::find::find_in(&scope, ids::THREAD_MEMBER_KEEP_BUTTON).is_some(),
                "a flagged member's chip must carry the Keep button"
            );
        });
    }

    #[test]
    fn an_unflagged_member_gets_no_review_pair() {
        crate::testid::run_on_gtk_thread(|| {
            let person = ActorId([9u8; 32]);
            let header = ThreadHeader::new();
            header.render(&detail_with_one_participant(person), &[]);

            let root: gtk::Widget = header.root.clone().upcast();
            assert!(
                crate::automation::find::find_in(&root, ids::THREAD_MEMBER_UNATTESTED_MARK)
                    .is_none(),
                "an unflagged member must not carry the review mark"
            );
            assert!(
                crate::automation::find::find_in(&root, ids::THREAD_MEMBER_KEEP_BUTTON).is_none(),
                "an unflagged member must not carry the Keep button"
            );
        });
    }

    /// Pins the Keep button's wire, not just its presence — the same
    /// dropped-command trap `the_contact_confirm_id_rides_an_activatable_button`
    /// pins for `contact-confirm` (testing.md point 11).
    #[test]
    fn pressing_keep_fires_the_callback_with_the_flagged_person() {
        crate::testid::run_on_gtk_thread(|| {
            let person = ActorId([3u8; 32]);
            let header = ThreadHeader::new();
            let reviews = vec![MemberReview {
                person,
                reasons: vec![MemberUnattestedReason::CompromiseWindow],
            }];
            header.render(&detail_with_one_participant(person), &reviews);

            let seen: Rc<RefCell<Option<ActorId>>> = Rc::new(RefCell::new(None));
            {
                let seen = seen.clone();
                header.on_keep_member(move |p| *seen.borrow_mut() = Some(p));
            }

            let root: gtk::Widget = header.root.clone().upcast();
            let keep = crate::automation::find::find_in(&root, ids::THREAD_MEMBER_KEEP_BUTTON)
                .expect("Keep button present");
            keep.downcast_ref::<gtk::Button>()
                .expect("Keep is a button")
                .emit_clicked();

            assert_eq!(*seen.borrow(), Some(person));
        });
    }

    /// A governed room seeded onto `detail_with_one_participant`'s thread:
    /// two Fauna seats, alice the viewer and owner, bob a plain member.
    fn governed_two_seats(viewer_role: RoomRole) -> ThreadDetail {
        let mut detail = detail_with_one_participant(ActorId([7u8; 32]));
        detail.participants.push(TypedAddress::Fauna {
            handle: "bob".into(),
            actor_id: ActorId([8u8; 32]),
        });
        detail.participant_displays.push("bob".into());
        let room = RoomSnapshot {
            class: RoomClass::EndToEnd,
            members: vec![
                RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(RoomRole::Owner),
                },
                RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(RoomRole::Member),
                },
            ],
            policy: Some(RoomPolicySnapshot {
                version: 1,
                name: None,
                join_rule: JoinRule::Invite,
                history_policy: HistoryPolicy::None,
            }),
            my_role: Some(viewer_role),
            nest_read: None,
            labelers: None,
            awaiting_key: false,
            moderation_unverified: false,
            pending_invites: None,
        };
        detail.capabilities = room.gate(detail.capabilities);
        detail.room = Some(room);
        detail
    }

    fn has_class(w: &gtk::Widget, class: &str) -> bool {
        w.css_classes().iter().any(|c| c == class)
    }

    /// The header states the room's class with its driver-facing `class`
    /// attribute, and marks each chip's role — the surfaces
    /// `conversation-rooms.md` § Implementation status today lists for the
    /// six-app trickle-down (`ui/conversations.md` § Element IDs).
    #[test]
    fn a_governed_room_states_its_class_and_marks_each_chip_with_a_role() {
        crate::testid::run_on_gtk_thread(|| {
            let header = ThreadHeader::new();
            header.render(&governed_two_seats(RoomRole::Owner), &[]);
            let root: gtk::Widget = header.root.clone().upcast();

            let class = crate::automation::find::find_in(&root, ids::THREAD_ROOM_CLASS)
                .expect("thread-room-class is painted on a room");
            assert!(class.is_visible());
            assert!(
                has_class(&class, "test-attr-class-end-to-end"),
                "the class attribute carries the shared token, not a per-app word"
            );

            // `thread-member-chip[i]`'s `role` attribute, index-parallel with
            // the participants: alice owns the room, bob is a plain member.
            let mut chips: Vec<gtk::Widget> = Vec::new();
            crate::automation::find::collect_in(&root, ids::THREAD_MEMBER_CHIP, &mut chips);
            assert_eq!(chips.len(), 2);
            assert!(has_class(&chips[0], "test-attr-role-owner"));
            assert!(has_class(&chips[1], "test-attr-role-member"));
        });
    }

    /// § Architectural rules 5: greyed, never hidden. A plain member sees the
    /// editor's door and the add-participant affordance PAINTED and
    /// insensitive — the roles table is applied in shared Rust, and this app
    /// never re-derives it.
    #[test]
    fn a_plain_member_sees_the_room_affordances_greyed_not_hidden() {
        crate::testid::run_on_gtk_thread(|| {
            let header = ThreadHeader::new();
            header.render(&governed_two_seats(RoomRole::Member), &[]);
            let root: gtk::Widget = header.root.clone().upcast();

            let settings =
                crate::automation::find::find_in(&root, ids::THREAD_ROOM_SETTINGS_BUTTON)
                    .expect("thread-room-settings-button is painted whenever the thread is a room");
            assert!(settings.is_visible(), "painted, never hidden");
            assert!(
                !settings.is_sensitive(),
                "greyed: a member may not set policy"
            );

            let add = crate::automation::find::find_in(&root, ids::THREAD_ADD_PARTICIPANT_BUTTON)
                .expect("thread-add-participant-button is painted on a mutable rail");
            assert!(add.is_visible());
            assert!(!add.is_sensitive(), "greyed off can_invite, not hidden");

            // The chip IS the Remove control on a mutable rail, so a member who
            // may not remove sees it greyed — not demoted to the mail chip's
            // address popover, which a driver (and a user) reads as live.
            let mut chips: Vec<gtk::Widget> = Vec::new();
            crate::automation::find::collect_in(&root, ids::THREAD_MEMBER_CHIP, &mut chips);
            assert_eq!(chips.len(), 2, "painted, never hidden");
            assert!(
                chips.iter().all(|chip| !chip.is_sensitive()),
                "greyed off can_remove_members, not hidden and not a live popover"
            );
        });
    }

    /// The owner's chips on the same room are live Remove controls — the
    /// greying above is the role's, never the rail's — and the removal sits on
    /// the widget that carries `thread-member-chip` itself, the one a driver
    /// presses: a gesture on the pill around it left the id with nothing to
    /// press, so no journey could drive a removal (the room journeys' first
    /// linux run). Pins the wire, not just the sensitivity.
    #[test]
    fn the_owner_sees_live_remove_chips_on_the_same_room() {
        crate::testid::run_on_gtk_thread(|| {
            let header = ThreadHeader::new();
            let detail = governed_two_seats(RoomRole::Owner);
            header.render(&detail, &[]);
            let root: gtk::Widget = header.root.clone().upcast();

            let mut chips: Vec<gtk::Widget> = Vec::new();
            crate::automation::find::collect_in(&root, ids::THREAD_MEMBER_CHIP, &mut chips);
            assert_eq!(chips.len(), 2);
            assert!(chips.iter().all(|chip| chip.is_sensitive()));

            let removed: Rc<RefCell<Option<TypedAddress>>> = Rc::new(RefCell::new(None));
            {
                let removed = removed.clone();
                header.on_remove_member(move |addr| *removed.borrow_mut() = Some(addr));
            }
            let controllers = chips[1].observe_controllers();
            let gesture = (0..controllers.n_items())
                .find_map(|i| controllers.item(i).and_downcast::<gtk::GestureClick>())
                .expect("the id-carrying chip carries its own click gesture");
            gesture.emit_by_name::<()>("pressed", &[&1i32, &0.0f64, &0.0f64]);
            gesture.emit_by_name::<()>("released", &[&1i32, &0.0f64, &0.0f64]);
            assert_eq!(
                removed.borrow().as_ref(),
                Some(&detail.participants[1]),
                "pressing chip[1] removes the participant it names"
            );
        });
    }

    /// A thread the rail models no room for paints neither room surface —
    /// "absent where the rail models no room" (`ui/conversations.md`
    /// § Element IDs).
    #[test]
    fn a_non_room_thread_paints_no_room_surfaces() {
        crate::testid::run_on_gtk_thread(|| {
            let header = ThreadHeader::new();
            header.render(&detail_with_one_participant(ActorId([7u8; 32])), &[]);
            let root: gtk::Widget = header.root.clone().upcast();

            // `find_in` prunes non-showing subtrees, exactly as the AT-SPI
            // tree a driver reads does — so a hidden widget is *absent*, which
            // is the contract's own word.
            assert!(
                crate::automation::find::find_in(&root, ids::THREAD_ROOM_CLASS).is_none(),
                "thread-room-class is absent where the rail models no room"
            );
            assert!(
                crate::automation::find::find_in(&root, ids::THREAD_ROOM_SETTINGS_BUTTON).is_none(),
                "and so is the editor's door"
            );
        });
    }
}
