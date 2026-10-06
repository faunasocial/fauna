//! Integration round-trip for the **room plane's floor roster** — the
//! `fauna.conversations.room.roster_report` kind and the two tables it
//! writes (`rooms` + `room_members`).
//!
//! Authority: `docs/goal/behavior/conversation-rooms.md` § The floor roster.
//! An end-to-end room's floor roster is a **member-reported mirror**: the
//! MLS group is the membership authority and the nest cannot see inside it,
//! so after every membership commit the committing device reports the
//! resulting roster to the room's home nest. The nest stores the report and
//! uses it for everything a nest legitimately decides about membership —
//! routing fan-out, the custody serve door, the relay gate, succession
//! targets — and decides **nothing** about confidentiality: a wrong report
//! cannot make a non-member read a message, because reading is MLS's.
//!
//! (It once had a sibling for the dormant Mechanism-B group plane, retired
//! with that plane under the alpha carve-out.) The community class's own
//! operations (create / invite / send / set_policy / transfer) are a later
//! slice of the same row; what is pinned here is the roster the custody
//! door reads.

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::recovery::RecoveryKey;
use fauna_nest::{
    account_handlers,
    bridge_method_allowlist::{CallerClass, is_permitted},
    conversations_handlers,
    db::CacheDb,
    pending_actions, recovery_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::account::AccountDeleteRequest;
use fauna_protocol::{
    conversations::{
        RoomAcceptInviteReply, RoomAcceptInviteRequest, RoomBackfillGenerationsReply,
        RoomBackfillGenerationsRequest, RoomCreateReply, RoomCreateRequest, RoomGenerationsReply,
        RoomGenerationsRequest, RoomInviteReply, RoomInviteRequest, RoomLeaveReply,
        RoomLeaveRequest, RoomListInvitesReply, RoomListInvitesRequest, RoomListRosterReply,
        RoomListRosterRequest, RoomPendingInviteWire, RoomPublishGenerationReply,
        RoomPublishGenerationRequest, RoomRemoveRequest, RoomRevokeInviteReply,
        RoomRevokeInviteRequest, RoomRosterEntryWire, RoomRosterReportReply,
        RoomRosterReportRequest, RoomSearchReply, RoomSearchRequest, RoomSetPolicyReply,
        RoomSetPolicyRequest, RoomSetReceptionKeyReply, RoomSetReceptionKeyRequest,
        RoomTransferOwnershipReply, RoomTransferOwnershipRequest,
    },
    decode_strict as decode, encode_canonical,
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    (b.build(), state)
}

fn report_payload(room_hex: &str, members: Vec<RoomRosterEntryWire>) -> Bytes {
    sequenced_report_payload(room_hex, members, Some(1), None)
}

/// A report naming the log position of the commit it follows — what a
/// current client sends (`RoomRosterReportRequest::commit_seq`).
/// [`report_payload`] is the unordered shape a report that follows no commit
/// (a leave, the birth report, the floor backfill) takes.
fn sequenced_report_payload(
    room_hex: &str,
    members: Vec<RoomRosterEntryWire>,
    policy_version: Option<u64>,
    commit_seq: Option<i64>,
) -> Bytes {
    let req = RoomRosterReportRequest {
        room_id: room_hex.into(),
        members,
        policy_version,
        commit_seq,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Stand in for membership commits landing on the room's log: the channel's
/// commit high-water mark is what `channel.send` advances when a
/// `ChannelEnvelope::Commit` lands (`segments::conv::append`), and it is the
/// bound the report door holds a claimed position to.
///
/// `sender` is the actor the nest authenticated on that send — the authorship
/// half of the mark, which the door binds a report at the newest position to
/// (`conversation-rooms.md` § The floor roster). `None` stands for a mark that
/// names no sender: a row written before schema 64, or an advance by a
/// nest-side writer with no member behind it. The ordering cases below use
/// `None` deliberately, so each one keeps testing the ordering rule alone —
/// and together they pin the declared residue that an unattributed mark
/// admits a positioned report as it always did.
async fn land_commit_at(
    state: &AppState,
    room_id: &[u8; 32],
    seq: i64,
    sender: Option<&ActorKeypair>,
) {
    let sender = sender.map(|k| k.actor_id().0);
    state
        .db
        .set_channel_commit_watermark(room_id, seq, sender.as_ref())
        .await
        .expect("commit watermark");
}

fn entry(actor: &ActorKeypair, role: &str) -> RoomRosterEntryWire {
    RoomRosterEntryWire {
        actor: hex::encode(actor.actor_id().0),
        role: Some(role.into()),
        extra: std::collections::BTreeMap::new(),
    }
}

/// Seat `actor` on the channel's routing roster — the report door's second
/// gate. In production a `channel.send` writes this row; here it stands for
/// the reporter being a live participant of the channel.
async fn seat_on_routing_roster(state: &AppState, actor: &ActorKeypair, room_id: &[u8; 32]) {
    state
        .db
        .register_actor_channel(&actor.actor_id().0, room_id)
        .await
        .expect("routing roster row");
}

fn room_id_of(hex_str: &str) -> [u8; 32] {
    hex::decode(hex_str).unwrap().try_into().unwrap()
}

// ── the happy path ──────────────────────────────────────────────

#[tokio::test]
async fn a_report_from_a_member_becomes_the_rooms_floor_roster() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let room_hex = hex::encode([7u8; 32]);
    let room_id = room_id_of(&room_hex);

    seat_on_routing_roster(&state, &owner, &room_id).await;

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(
            &room_hex,
            vec![
                entry(&owner, "owner"),
                entry(&admin, "admin"),
                entry(&member, "member"),
            ],
        ),
    )
    .await
    .expect("a member's report is stored");
    let reply: RoomRosterReportReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.members, 3, "the ack counts the stored roster");

    for (who, expected) in [(&owner, "owner"), (&admin, "admin"), (&member, "member")] {
        assert_eq!(
            state
                .db
                .get_room_member_role(&room_id, &who.actor_id().0)
                .await
                .expect("floor roster read")
                .as_deref(),
            Some(expected),
            "the reported role is what the floor roster holds"
        );
    }

    // The room record is minted by the first report: the class is DERIVED
    // from the member set (all user principals ⇒ end-to-end), never stored
    // as a choice (`conversation-rooms.md` § The three classes).
    let room = state
        .db
        .get_room(&room_id)
        .await
        .expect("room read")
        .expect("the first report mints the room record");
    assert_eq!(
        room.class, "end_to_end",
        "class derived from the member set"
    );
    assert_eq!(
        room.owner_id.as_deref(),
        Some(owner.actor_id().0.as_slice()),
        "the owner entry names the room's owner"
    );
    assert_eq!(room.policy_version, Some(1));
}

// ── authorization ───────────────────────────────────────────────

#[tokio::test]
async fn a_reporter_who_was_never_a_member_is_refused() {
    // *Report, never guess* binds the reporter to the room it reports: a
    // caller that neither names itself nor is a live member of the roster it
    // replaces is reporting somebody else's room, which no honest commit
    // produces.
    let (router, state) = router_with_db_only().await;
    let stranger = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let room_hex = hex::encode([8u8; 32]);
    let room_id = room_id_of(&room_hex);

    seat_on_routing_roster(&state, &stranger, &room_id).await;

    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&member, "owner")]),
    )
    .await
    .expect_err("a reporter outside its own roster is refused");
    assert_eq!(
        err.code, "fauna.conversations.permission_denied",
        "the refusal is the conversations family's permission_denied: {err:?}"
    );
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &member.actor_id().0)
            .await
            .expect("floor roster read"),
        None,
        "nothing was stored"
    );
}

#[tokio::test]
async fn a_departing_members_final_report_is_accepted_exactly_once() {
    // A self-leave is a membership commit the LEAVING device authors, so the
    // roster it produces is by construction one the reporter is absent from
    // (`conversation-rooms.md` § The floor roster → *End-to-end rooms*: the
    // committing device reports the resulting roster). Refusing it would
    // leave the departure invisible to the nest until some other member
    // happened to commit — and the custody serve door, which reads this
    // roster, would keep serving the leaver's grants meanwhile.
    //
    // It is one-shot: after the departure the leaver is no longer a live
    // member, so a second report from it satisfies neither shape.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let leaver = ActorKeypair::generate();
    let room_hex = hex::encode([17u8; 32]);
    let room_id = room_id_of(&room_hex);

    seat_on_routing_roster(&state, &owner, &room_id).await;
    seat_on_routing_roster(&state, &leaver, &room_id).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&leaver, "member")],
        ),
    )
    .await
    .expect("the room's roster");

    // The leaver's own final report, naming only who remains.
    dispatch(
        &router,
        state.clone(),
        leaver.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&owner, "owner")]),
    )
    .await
    .expect("a departing member's final report is accepted");
    assert!(
        !state
            .db
            .is_room_member(&room_id, &leaver.actor_id().0)
            .await
            .unwrap(),
        "the departure landed"
    );

    // And exactly once — this is the RATCHET, the property that makes the
    // floor roster non-self-assertable. The departed member's routing row
    // deliberately survives its departure, so without the ratchet it could
    // walk straight back in by naming itself in a fresh report, re-opening
    // the custody serve door over the room's conv scope. The gate reads the
    // STORED roster, where it is no longer live, so it cannot.
    let err = dispatch(
        &router,
        state.clone(),
        leaver.actor_id().0,
        "fauna.conversations.room.roster_report",
        // A well-formed roster (exactly one owner) that simply re-seats the
        // departed reporter — so what refuses it is the authorization gate,
        // not shape validation.
        report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&leaver, "member")],
        ),
    )
    .await
    .expect_err("a departed member cannot report itself back in");
    assert_eq!(
        err.code, "fauna.conversations.permission_denied",
        "the refusal is permission_denied: {err:?}"
    );
    assert!(
        !state
            .db
            .is_room_member(&room_id, &leaver.actor_id().0)
            .await
            .unwrap(),
        "the refused report changed nothing — the departed member stayed out"
    );
}

#[tokio::test]
async fn a_stranger_on_the_routing_roster_cannot_replace_an_existing_roster() {
    // The ratchet's other face: the routing roster is self-registered by a
    // single `channel.send`, so a row there proves knowledge of the channel
    // id, not membership (`conversation-rooms.md` § The floor roster →
    // *What the floor roster is not*). Once a room HAS a floor roster, that
    // knowledge buys nothing — the replacement must come from a live member.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();
    let room_hex = hex::encode([18u8; 32]);
    let room_id = room_id_of(&room_hex);

    seat_on_routing_roster(&state, &owner, &room_id).await;
    seat_on_routing_roster(&state, &stranger, &room_id).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&owner, "owner")]),
    )
    .await
    .expect("the room's own roster");

    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(
            &room_hex,
            vec![entry(&stranger, "owner"), entry(&owner, "member")],
        ),
    )
    .await
    .expect_err("a non-member cannot replace a room's roster");
    assert_eq!(
        err.code, "fauna.conversations.permission_denied",
        "the refusal is permission_denied: {err:?}"
    );
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &owner.actor_id().0)
            .await
            .unwrap()
            .as_deref(),
        Some("owner"),
        "the real owner was not demoted"
    );
    assert!(
        !state
            .db
            .is_room_member(&room_id, &stranger.actor_id().0)
            .await
            .unwrap(),
        "the stranger did not seat itself"
    );
}

#[tokio::test]
async fn a_reporter_off_the_routing_roster_is_refused() {
    // The channel's routing roster is the second gate: it proves the caller
    // is a live participant of this channel on this nest. It is NOT the
    // membership authority (`conversation-rooms.md` § The floor roster →
    // *What the floor roster is not*) — it is why a caller that has never
    // touched the channel cannot mint a roster for an id it merely guessed.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let room_hex = hex::encode([9u8; 32]);

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&owner, "owner")]),
    )
    .await
    .expect_err("a caller off the channel's routing roster is refused");
    assert_eq!(
        err.code, "fauna.conversations.permission_denied",
        "the refusal is the conversations family's permission_denied: {err:?}"
    );
}

// ── the wholesale replace ───────────────────────────────────────

#[tokio::test]
async fn a_later_report_replaces_the_roster_wholesale_and_absorbs_removals() {
    // The report replaces the previous one wholesale. A principal the new
    // report drops is `Removed`-absorbed rather than deleted — the shape the
    // succession axis declares for a membership row
    // (`conversation-rooms.md` § The home nest → *The succession axis*).
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let leaver = ActorKeypair::generate();
    let room_hex = hex::encode([10u8; 32]);
    let room_id = room_id_of(&room_hex);

    seat_on_routing_roster(&state, &owner, &room_id).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&leaver, "member")],
        ),
    )
    .await
    .expect("first report");
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &leaver.actor_id().0)
            .await
            .unwrap()
            .as_deref(),
        Some("member")
    );

    // The commit that removed them: the next report simply omits them.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&owner, "owner")]),
    )
    .await
    .expect("second report");

    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &leaver.actor_id().0)
            .await
            .expect("floor roster read"),
        None,
        "a dropped principal is no longer a live member"
    );
    let history = state
        .db
        .list_floor_roster_including_removed(&room_id)
        .await
        .expect("roster history read");
    let dropped = history
        .iter()
        .find(|m| m.principal_id == leaver.actor_id().0)
        .expect("the dropped row is absorbed as history, not deleted");
    assert!(
        dropped.removed_at.is_some(),
        "the dropped row carries a removal stamp"
    );
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &owner.actor_id().0)
            .await
            .unwrap()
            .as_deref(),
        Some("owner"),
        "the surviving member is untouched"
    );
}

#[tokio::test]
async fn a_re_added_principal_comes_back_live_on_the_same_row() {
    // Absorbing a removal must not strand the row: a later report naming the
    // principal again clears the stamp rather than colliding on the primary
    // key.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let rejoiner = ActorKeypair::generate();
    let room_hex = hex::encode([11u8; 32]);
    let room_id = room_id_of(&room_hex);

    seat_on_routing_roster(&state, &owner, &room_id).await;

    for members in [
        vec![entry(&owner, "owner"), entry(&rejoiner, "member")],
        vec![entry(&owner, "owner")],
        vec![entry(&owner, "owner"), entry(&rejoiner, "admin")],
    ] {
        dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.roster_report",
            report_payload(&room_hex, members),
        )
        .await
        .expect("report");
    }

    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &rejoiner.actor_id().0)
            .await
            .unwrap()
            .as_deref(),
        Some("admin"),
        "the re-add comes back live, with the newly reported role"
    );
}

// ── the order of reports ────────────────────────────────────────
//
// Every membership and policy commit owes a report, each built independently
// by whichever device committed, and nothing sequences their delivery — so a
// report can reach the home nest after the report of a LATER commit. Applied
// as it arrives, it would put back a member the later commit removed and the
// roles the later commit rewrote. What orders them is the log position of the
// commit each one follows, held to the newest commit the room's log has
// really carried (`conversation-rooms.md` § The floor roster).

/// A room whose log has carried commits up to `tip`, with `owner` on the
/// routing roster — the door's first gate — and nothing reported yet.
async fn room_with_commits_through(
    state: &AppState,
    owner: &ActorKeypair,
    room_byte: u8,
    tip: i64,
) -> (String, [u8; 32]) {
    let room_hex = hex::encode([room_byte; 32]);
    let room_id = room_id_of(&room_hex);
    seat_on_routing_roster(state, owner, &room_id).await;
    land_commit_at(state, &room_id, tip, None).await;
    (room_hex, room_id)
}

async fn live_role(state: &AppState, room_id: &[u8; 32], who: &ActorKeypair) -> Option<String> {
    state
        .db
        .get_room_member_role(room_id, &who.actor_id().0)
        .await
        .expect("floor roster read")
}

#[tokio::test]
async fn a_report_arriving_after_a_later_commits_report_rolls_nothing_back() {
    // Commit 10 seated `leaver`, with `stayer` a plain member under policy 1.
    // Commit 12 appointed `stayer` admin (policy 2), and commit 14 removed
    // `leaver`. Commit 14's report lands first; commit 10's straggles in
    // after it. The floor must keep commit 14's answer on BOTH axes —
    // membership and roles — which `policy_version` alone could never
    // decide: an ordinary remove does not change it.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let stayer = ActorKeypair::generate();
    let leaver = ActorKeypair::generate();
    let (room_hex, room_id) = room_with_commits_through(&state, &owner, 40, 14).await;

    let later: RoomRosterReportReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.roster_report",
            sequenced_report_payload(
                &room_hex,
                vec![entry(&owner, "owner"), entry(&stayer, "admin")],
                Some(2),
                Some(14),
            ),
        )
        .await
        .expect("commit 14's report is stored"),
    )
    .unwrap();
    assert_eq!(later.superseded_by, None, "the first report is applied");

    let straggler: RoomRosterReportReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.roster_report",
            sequenced_report_payload(
                &room_hex,
                vec![
                    entry(&owner, "owner"),
                    entry(&stayer, "member"),
                    entry(&leaver, "member"),
                ],
                Some(1),
                Some(10),
            ),
        )
        .await
        .expect("a late report is not an error — the floor is newer than it"),
    )
    .unwrap();
    assert_eq!(
        straggler.superseded_by,
        Some(14),
        "the ack names the later commit whose roster the floor keeps"
    );
    assert_eq!(
        straggler.members, 2,
        "the ack counts the floor as it stands"
    );

    assert_eq!(
        live_role(&state, &room_id, &leaver).await,
        None,
        "the member commit 14 removed stays removed"
    );
    assert_eq!(
        live_role(&state, &room_id, &stayer).await.as_deref(),
        Some("admin"),
        "the role commit 12 granted is not rolled back"
    );
    assert_eq!(
        state
            .db
            .get_room(&room_id)
            .await
            .unwrap()
            .unwrap()
            .policy_version,
        Some(2),
        "nor is the policy version the roles were read under"
    );
}

#[tokio::test]
async fn reports_in_commit_order_each_replace_the_roster() {
    // The guard orders reports; it must not stop the ordinary case. And a
    // report at the position the floor already holds — a retry of the same
    // commit's report — changes nothing.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let joiner = ActorKeypair::generate();
    let (room_hex, room_id) = room_with_commits_through(&state, &owner, 41, 12).await;

    for (position, members) in [
        (10, vec![entry(&owner, "owner")]),
        (12, vec![entry(&owner, "owner"), entry(&joiner, "member")]),
    ] {
        let reply: RoomRosterReportReply = decode(
            &dispatch(
                &router,
                state.clone(),
                owner.actor_id().0,
                "fauna.conversations.room.roster_report",
                sequenced_report_payload(&room_hex, members, Some(1), Some(position)),
            )
            .await
            .expect("an in-order report is stored"),
        )
        .unwrap();
        assert_eq!(
            reply.superseded_by, None,
            "commit {position}'s report applies"
        );
    }
    assert_eq!(
        live_role(&state, &room_id, &joiner).await.as_deref(),
        Some("member"),
        "commit 12 seated the joiner"
    );

    let replay: RoomRosterReportReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.roster_report",
            sequenced_report_payload(&room_hex, vec![entry(&owner, "owner")], Some(1), Some(12)),
        )
        .await
        .expect("a second report at the same position is not an error"),
    )
    .unwrap();
    assert_eq!(replay.superseded_by, Some(12));
    assert_eq!(
        live_role(&state, &room_id, &joiner).await.as_deref(),
        Some("member"),
        "one commit has one roster: a second report for it changes nothing"
    );
}

#[tokio::test]
async fn a_report_cannot_claim_a_commit_the_rooms_log_never_carried() {
    // The position is a client's claim, so it is held to what the nest itself
    // observes: the newest commit on the room's log. Without that bound one
    // member could report at an enormous position and freeze the floor
    // against every honest report after it — the next real commit would land
    // far below the claim. With it, the highest claimable position is the
    // newest real commit, and the next honest commit lands above it.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (room_hex, room_id) = room_with_commits_through(&state, &owner, 42, 12).await;

    for claimed in [13, i64::MAX] {
        let err = dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.roster_report",
            sequenced_report_payload(
                &room_hex,
                vec![entry(&owner, "owner"), entry(&member, "admin")],
                Some(1),
                Some(claimed),
            ),
        )
        .await
        .expect_err("a position past the newest commit on the log is refused");
        assert_eq!(
            err.code, "fauna.conversations.invalid_params",
            "position {claimed}: {err:?}"
        );
    }
    assert!(
        state.db.get_room(&room_id).await.unwrap().is_none(),
        "a refused report stores nothing, not even the room record"
    );

    // The next real commit's report still lands.
    land_commit_at(&state, &room_id, 13, None).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&member, "member")],
            Some(1),
            Some(13),
        ),
    )
    .await
    .expect("an honest report at the real tip is stored");
    assert_eq!(
        live_role(&state, &room_id, &member).await.as_deref(),
        Some("member")
    );

    // A room whose log has carried no commit at all bounds every claim.
    let (bare_hex, bare_id) = room_with_commits_through(&state, &owner, 43, 0).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(&bare_hex, vec![entry(&owner, "owner")], Some(1), Some(1)),
    )
    .await
    .expect_err("no commit on the log, so no report can follow one");
    assert!(state.db.get_room(&bare_id).await.unwrap().is_none());
}

// ── whose commit a position names ───────────────────────────────
//
// Ordering by position alone leaves the FIRST report at a position winning
// it, and the loser is written nowhere. So the report the rule exists to
// protect — the committing device's own — had no standing over a bystander's,
// and the bystander with the most to gain is the member the commit removed:
// until the remover's report lands it is still a live member of the stored
// floor and still on the routing roster. The nest observes the commit's
// sender at the same append it observes the position, so at the newest
// position only that commit's sender may report
// (`conversation-rooms.md` § The floor roster).

/// A room whose floor already holds `owner` (owner) and `other` (member),
/// reported at position 10 by `owner`, whose commit it was.
async fn room_with_a_reported_floor(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    other: &ActorKeypair,
    room_byte: u8,
) -> (String, [u8; 32]) {
    let room_hex = hex::encode([room_byte; 32]);
    let room_id = room_id_of(&room_hex);
    seat_on_routing_roster(state, owner, &room_id).await;
    seat_on_routing_roster(state, other, &room_id).await;
    land_commit_at(state, &room_id, 10, Some(owner)).await;
    dispatch(
        router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(
            &room_hex,
            vec![entry(owner, "owner"), entry(other, "member")],
            Some(1),
            Some(10),
        ),
    )
    .await
    .expect("the committing device's own report seats the floor");
    (room_hex, room_id)
}

#[tokio::test]
async fn the_member_a_commit_removes_cannot_claim_that_commits_position() {
    // `owner` removes `leaver` by the commit at 11. In the window before
    // `owner`'s report lands, `leaver` is still a live member of the stored
    // floor (gate 2 admits it) and still on the routing roster (gate 1 — the
    // routing row survives a departure), and 11 is a real position within the
    // bound. Ordering alone would let `leaver`'s report at 11 land first,
    // seat itself, and leave `owner`'s honest report at 11 superseded and
    // written nowhere — durably, since the floor would then move only on the
    // NEXT membership or policy commit.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let leaver = ActorKeypair::generate();
    let (room_hex, room_id) =
        room_with_a_reported_floor(&router, &state, &owner, &leaver, 60).await;

    // The Remove commit lands at 11; `owner` sent it.
    land_commit_at(&state, &room_id, 11, Some(&owner)).await;

    let err = dispatch(
        &router,
        state.clone(),
        leaver.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&leaver, "member")],
            Some(1),
            Some(11),
        ),
    )
    .await
    .expect_err("a report at this commit's position must come from its sender");
    assert_eq!(err.code, "fauna.conversations.permission_denied", "{err:?}");

    // Refused, not silently superseded: the floor is untouched, so the honest
    // report still has a position to claim.
    assert_eq!(
        state
            .db
            .get_room(&room_id)
            .await
            .unwrap()
            .unwrap()
            .roster_commit_seq,
        Some(10),
        "a refused report advances nothing"
    );

    let honest: RoomRosterReportReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.roster_report",
            sequenced_report_payload(&room_hex, vec![entry(&owner, "owner")], Some(1), Some(11)),
        )
        .await
        .expect("the remover's own report at its own commit's position lands"),
    )
    .unwrap();
    assert_eq!(
        honest.superseded_by, None,
        "the honest report is applied, not dropped"
    );
    assert_eq!(
        live_role(&state, &room_id, &leaver).await,
        None,
        "the removed member does not keep its seat"
    );
}

#[tokio::test]
async fn authorship_binds_the_newest_position_and_nothing_else() {
    // The binding is narrow by construction, and both halves of that narrowness
    // are declared residue of § The floor roster. A position BELOW the newest
    // commit is unverifiable — the nest keeps one sender, the newest commit's —
    // and is admitted; being anchored lower, it is replaced by the honest
    // report that follows rather than superseding it. And a mark that names no
    // sender at all (a row from before the column, or an advance by a
    // nest-side writer) admits a positioned report exactly as it did before.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (room_hex, room_id) =
        room_with_a_reported_floor(&router, &state, &owner, &member, 61).await;

    // Commits at 11 (owner's) and 12 (owner's). 11 is now below the newest.
    land_commit_at(&state, &room_id, 12, Some(&owner)).await;

    dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&member, "admin")],
            Some(1),
            Some(11),
        ),
    )
    .await
    .expect("a position below the newest commit is admitted — its sender is not kept");

    // And the honest report at the newest position replaces it, which is why
    // admitting the lower one costs the floor nothing.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&member, "member")],
            Some(1),
            Some(12),
        ),
    )
    .await
    .expect("the sender of the newest commit reports it");
    assert_eq!(
        live_role(&state, &room_id, &member).await.as_deref(),
        Some("member"),
        "the honest report at the newer position wins"
    );

    // An unattributed advance leaves the position unbound: a bystander's
    // report at it is admitted, as it was before the sender was recorded.
    land_commit_at(&state, &room_id, 13, None).await;
    dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&member, "admin")],
            Some(1),
            Some(13),
        ),
    )
    .await
    .expect("a mark that names no sender binds nobody");
    assert_eq!(
        live_role(&state, &room_id, &member).await.as_deref(),
        Some("admin"),
        "the declared residue: an unattributed position is admitted"
    );
}

#[tokio::test]
async fn an_unordered_report_applies_and_keeps_the_floors_position() {
    // A report that follows no commit names no position and applies
    // unordered. What it must not do is reset the
    // position the floor holds: a current client's report of a commit older
    // than that position is still stale after the unordered one lands.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let a = ActorKeypair::generate();
    let b = ActorKeypair::generate();
    let (room_hex, room_id) = room_with_commits_through(&state, &owner, 44, 12).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&a, "member")],
            Some(1),
            Some(12),
        ),
    )
    .await
    .expect("the sequenced report is stored");

    let unordered: RoomRosterReportReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.roster_report",
            report_payload(&room_hex, vec![entry(&owner, "owner"), entry(&b, "member")]),
        )
        .await
        .expect("an unordered report is stored"),
    )
    .unwrap();
    assert_eq!(unordered.superseded_by, None);
    assert_eq!(
        live_role(&state, &room_id, &b).await.as_deref(),
        Some("member")
    );
    assert_eq!(live_role(&state, &room_id, &a).await, None);

    let stale: RoomRosterReportReply = decode(
        &dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.roster_report",
            sequenced_report_payload(
                &room_hex,
                vec![entry(&owner, "owner"), entry(&a, "member")],
                Some(1),
                Some(11),
            ),
        )
        .await
        .expect("a stale report is not an error"),
    )
    .unwrap();
    assert_eq!(
        stale.superseded_by,
        Some(12),
        "the unordered report left the floor's position where it was"
    );
    assert_eq!(live_role(&state, &room_id, &a).await, None);
}

#[tokio::test]
async fn the_ratchet_still_decides_before_the_order_does() {
    // Gate 2 is unchanged by ordering: a reporter the stored roster no longer
    // holds live is refused as before, whatever position it names — a fresh
    // position does not buy a departed member its way back in.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let evicted = ActorKeypair::generate();
    let (room_hex, room_id) = room_with_commits_through(&state, &owner, 45, 20).await;
    seat_on_routing_roster(&state, &evicted, &room_id).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(&room_hex, vec![entry(&owner, "owner")], Some(1), Some(19)),
    )
    .await
    .expect("the owner's report is stored");

    let err = dispatch(
        &router,
        state.clone(),
        evicted.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&evicted, "member")],
            Some(1),
            Some(20),
        ),
    )
    .await
    .expect_err("a reporter off the stored roster is refused");
    assert_eq!(err.code, "fauna.conversations.permission_denied", "{err:?}");
    assert_eq!(live_role(&state, &room_id, &evicted).await, None);
}

// ── a policy-less room reports no roles ──────────────────────────────

#[tokio::test]
async fn a_policy_less_room_reports_members_with_no_roles() {
    // A policy-less room (a 1:1, or a group whose context carries no
    // policy) has no roles, and the report says so: `role` is absent and
    // `policy_version` is `None`. The roster
    // is still the membership record the custody door reads — roles are
    // what a policy-less room lacks, not membership.
    let (router, state) = router_with_db_only().await;
    let a = ActorKeypair::generate();
    let b = ActorKeypair::generate();
    let room_hex = hex::encode([12u8; 32]);
    let room_id = room_id_of(&room_hex);

    seat_on_routing_roster(&state, &a, &room_id).await;

    let req = RoomRosterReportRequest {
        room_id: room_hex.clone(),
        members: vec![
            RoomRosterEntryWire {
                actor: hex::encode(a.actor_id().0),
                role: None,
                extra: Default::default(),
            },
            RoomRosterEntryWire {
                actor: hex::encode(b.actor_id().0),
                role: None,
                extra: Default::default(),
            },
        ],
        policy_version: None,
        commit_seq: None,
        extra: Default::default(),
    };
    dispatch(
        &router,
        state.clone(),
        a.actor_id().0,
        "fauna.conversations.room.roster_report",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("a policy-less room's report is stored");

    let roster = state
        .db
        .list_floor_roster(&room_id)
        .await
        .expect("floor roster read");
    assert_eq!(roster.len(), 2, "both members are on the floor roster");
    assert!(
        roster.iter().all(|m| m.role.is_none()),
        "a policy-less room's roster carries no roles"
    );
    let room = state.db.get_room(&room_id).await.unwrap().unwrap();
    assert_eq!(room.policy_version, None);
    assert_eq!(
        room.owner_id, None,
        "a policy-less room has no owner entry to name one"
    );

    // A 1:1's birth report is exactly this shape — two role-less principals,
    // no policy, no position, the creator naming itself on an empty floor
    // (`conversation-rooms.md` § The floor roster → *End-to-end rooms*, the
    // birth report) — and the custody serve door's read admits its members by
    // membership, never rank (`custody_admission.rs`), so a DM's own members
    // are served their room's segments where the door once failed closed.
    for who in [&a, &b] {
        assert!(
            state
                .db
                .is_room_member(&room_id, &who.actor_id().0)
                .await
                .unwrap(),
            "a role-less member is a member to the custody door"
        );
    }
}

// ── malformed input ─────────────────────────────────────────────

#[tokio::test]
async fn a_role_outside_the_three_role_vocabulary_is_refused() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let room_hex = hex::encode([13u8; 32]);
    let room_id = room_id_of(&room_hex);
    seat_on_routing_roster(&state, &owner, &room_id).await;

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&owner, "superuser")]),
    )
    .await
    .expect_err("a role outside owner/admin/member is refused");
    assert_eq!(
        err.code, "fauna.conversations.invalid_params",
        "the refusal is the conversations family's invalid_params: {err:?}"
    );
}

#[tokio::test]
async fn a_duplicate_principal_in_one_report_is_refused() {
    // One entry per principal — a report naming a principal twice has no
    // single answer for its role, and silently taking the last would let a
    // report mean two things.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let room_hex = hex::encode([14u8; 32]);
    let room_id = room_id_of(&room_hex);
    seat_on_routing_roster(&state, &owner, &room_id).await;

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&owner, "member")],
        ),
    )
    .await
    .expect_err("a duplicate principal is refused");
    assert_eq!(
        err.code, "fauna.conversations.invalid_params",
        "the refusal is the conversations family's invalid_params: {err:?}"
    );
}

#[tokio::test]
async fn an_empty_report_is_refused() {
    // A room with no members is not a room; an empty report is far more
    // likely a bug than a membership fact, and accepting it would sever
    // every consumer of the roster at once.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let room_hex = hex::encode([15u8; 32]);
    let room_id = room_id_of(&room_hex);
    seat_on_routing_roster(&state, &owner, &room_id).await;

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![]),
    )
    .await
    .expect_err("an empty roster is refused");
    assert_eq!(
        err.code, "fauna.conversations.invalid_params",
        "the refusal is the conversations family's invalid_params: {err:?}"
    );
}

#[tokio::test]
async fn more_than_one_owner_is_refused() {
    // "Exactly one owner" (`conversation-rooms.md` § Roles and
    // authorization) is a property of the roster, so the floor refuses a
    // report that does not have it — an owner-ambiguous roster would give
    // succession two targets.
    let (router, state) = router_with_db_only().await;
    let a = ActorKeypair::generate();
    let b = ActorKeypair::generate();
    let room_hex = hex::encode([16u8; 32]);
    let room_id = room_id_of(&room_hex);
    seat_on_routing_roster(&state, &a, &room_id).await;

    let err = dispatch(
        &router,
        state.clone(),
        a.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&a, "owner"), entry(&b, "owner")]),
    )
    .await
    .expect_err("two owners are refused");
    assert_eq!(
        err.code, "fauna.conversations.invalid_params",
        "the refusal is the conversations family's invalid_params: {err:?}"
    );
}

// ── the caller-class gate ───────────────────────────────────────

#[test]
fn the_report_kind_is_user_class_only() {
    // End-user room membership: the same caller-class gate the rest of the
    // conversations family carries. `Admin` is deliberately absent from the
    // refusal list — an admin is a user with an extra role and inherits the
    // whole User set by the allowlist's blanket rule, and this kind is
    // caller-scoped anyway (the handler binds the report to the calling
    // actor's own membership, so an admin reporting reaches only rooms it is
    // itself a member of).
    assert!(is_permitted(
        CallerClass::User,
        "fauna.conversations.room.roster_report"
    ));
    for class in [
        CallerClass::BridgeMta,
        CallerClass::BridgeMda,
        CallerClass::BridgeAtprotoPds,
        CallerClass::Custodian,
        CallerClass::ContentProcessor,
    ] {
        assert!(
            !is_permitted(class, "fauna.conversations.room.roster_report"),
            "{class:?} has no room-membership role"
        );
    }
}

// ── the birth ceremony — `fauna.conversations.room.create` ──────
//
// The community class's room is born by a **ceremony**, not by a report:
// the creator signs the room's initial policy, the id commits to that birth
// record, and the nest seats the founding roster itself
// (`conversation-rooms.md` § The room; § The home nest — "a room is born on
// its creating member's home nest").
//
// The birth record is what closes the report door's declared bootstrap
// bound: a room that has one is floor-authoritative, and
// `roster_report` — the *member-reported mirror* door — is refused on it.

/// The binding birth salt around 24 copies of `byte` — the one shape of salt a
/// community room is founded with.
fn birth_salt(byte: u8) -> [u8; 32] {
    fauna_mls::room_policy::binding_birth_salt(&[byte; 24])
}

fn birth_payload(owner: &ActorKeypair, salt: [u8; 32], name: Option<&str>) -> Bytes {
    let policy =
        fauna_mls::room_policy::RoomPolicy::initial(owner.actor_id(), name.map(Into::into));
    let signed = policy
        .sign(owner)
        .expect("owner signs its own birth policy");
    let req = RoomCreateRequest {
        salt: hex::encode(salt),
        policy: encode_canonical(&signed).unwrap().to_vec(),
        reception_pubkey: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

async fn create_room(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    salt: [u8; 32],
) -> RoomCreateReply {
    let bytes = dispatch(
        router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.create",
        birth_payload(owner, salt, Some("the square")),
    )
    .await
    .expect("the owner's own birth record is admitted");
    decode(&bytes).unwrap()
}

#[tokio::test]
async fn a_room_is_born_with_its_owner_and_its_home_nest_on_the_floor() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();

    let reply = create_room(&router, &state, &owner, birth_salt(3)).await;
    let room_id = room_id_of(&reply.room_id);

    // The id commits to the birth record — the creator's key and the salt —
    // so nobody can name a room id they did not create.
    assert_eq!(
        room_id,
        fauna_mls::room_policy::derive_room_id(&owner.actor_id(), &birth_salt(3))
            .expect("the id derives from the birth core"),
        "room_id is content-derived from the birth record, never caller-chosen"
    );

    let room = state
        .db
        .get_room(&room_id)
        .await
        .expect("room read")
        .expect("the ceremony minted the room record");
    // § Don't do these — a community room is made by the home nest being a
    // MEMBER, and the class is DERIVED from that member set (rule 1), never
    // a field the caller chose. The ceremony seats the nest at birth, so the
    // ordered derivation lands on `community` with nothing stored as a
    // choice.
    assert_eq!(room.class, "community");
    assert_eq!(room.owner_id.as_deref(), Some(&owner.actor_id().0[..]));
    assert_eq!(room.policy_version, Some(1));

    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &owner.actor_id().0)
            .await
            .expect("floor read"),
        Some("owner".to_string()),
        "the creator is the room's one owner"
    );
    let nest_principal = state.nest_identity.public_key_bytes();
    assert!(
        state
            .db
            .is_room_member(&room_id, &nest_principal)
            .await
            .expect("floor read"),
        "the home nest is a founding member — that is what makes the class community"
    );
    let roster = state.db.list_floor_roster(&room_id).await.expect("roster");
    assert_eq!(roster.len(), 2, "owner + home nest, nobody else");
    let nest_row = roster
        .iter()
        .find(|m| m.principal_id == nest_principal)
        .expect("the nest's roster row");
    assert_eq!(nest_row.principal_kind, "nest");
    assert_eq!(
        nest_row.role.as_deref(),
        Some("member"),
        "the nest reads; it never invites, removes or mints (§ Don't do these)"
    );
}

/// **Row 749.** A founder's reception key of the right length (1216 bytes)
/// but a FIPS-203-invalid ML-KEM half is refused — before the fix a length
/// check alone admitted it, binding a seat that would later freeze every
/// mint over the room. Refused before anything is written: no room is
/// founded on this id at all, not even keyless.
#[tokio::test]
async fn room_create_refuses_a_fips_invalid_founder_key() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let salt = birth_salt(0x78);
    let policy =
        fauna_mls::room_policy::RoomPolicy::initial(owner.actor_id(), Some("the square".into()));
    let signed = policy
        .sign(&owner)
        .expect("owner signs its own birth policy");
    let req = RoomCreateRequest {
        salt: hex::encode(salt),
        policy: encode_canonical(&signed).unwrap().to_vec(),
        reception_pubkey: vec![0xFFu8; fauna_mls::wrapped_blob::XWING_ENCAPS_KEY_LEN],
        extra: std::collections::BTreeMap::new(),
    };
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.create",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("a FIPS-203-invalid founder key is refused");
    assert_eq!(err.code, "fauna.conversations.invalid_params");

    let room_id = fauna_mls::room_policy::derive_room_id(&owner.actor_id(), &salt)
        .expect("the id derives from the birth record");
    assert!(
        state.db.get_room(&room_id).await.unwrap().is_none(),
        "no room was founded"
    );
}

/// A birth salt without the binding mark founds no room: it is the lever a
/// non-conforming founder would pull to mint a room whose id reads as one that
/// predates the room signature — the room a splice of another room's versions
/// targets. The owner's own, correctly signed birth record is refused for the
/// salt alone, and nothing is written.
#[tokio::test]
async fn room_create_refuses_a_birth_salt_without_the_binding_mark() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let unmarked = [0x5au8; 32];
    assert!(!fauna_mls::room_policy::is_binding_birth_salt(&unmarked));
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.create",
        birth_payload(&owner, unmarked, Some("the square")),
    )
    .await
    .expect_err("an unmarked birth salt founds no room");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
    let reason = format!("{:?}", err.details);
    assert!(
        reason.contains("birth salt refused") && reason.contains("binding mark"),
        "refused for the salt, not the record: {reason}"
    );

    let room_id = fauna_mls::room_policy::derive_room_id(&owner.actor_id(), &unmarked)
        .expect("the id derives from the birth core");
    assert!(
        state.db.get_room(&room_id).await.unwrap().is_none(),
        "no room was founded"
    );
}

#[tokio::test]
async fn a_birth_record_signed_by_anyone_but_its_owner_is_refused() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();

    // A policy naming `owner` as owner, signed by `stranger` — the signature
    // verifies under its own signer, so only the signer↔owner bond refuses
    // it. Rule 6: the nest "cannot author" a policy, and it refuses what the
    // signature does not cover.
    let policy =
        fauna_mls::room_policy::RoomPolicy::initial(owner.actor_id(), Some("not yours".into()));
    let signed = policy.sign(&stranger).expect("stranger signs");
    let req = RoomCreateRequest {
        salt: hex::encode(birth_salt(9)),
        policy: encode_canonical(&signed).unwrap().to_vec(),
        reception_pubkey: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.create",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("a birth record whose signer is not its owner is refused");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

#[tokio::test]
async fn a_birth_record_the_caller_did_not_author_is_refused() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let relayer = ActorKeypair::generate();

    // Correctly self-signed by `owner`, but dispatched by someone else. A
    // birth record is the creating principal's own act (§ The home nest —
    // "a room is born on its CREATING member's home nest"), so a third party
    // may not mint a room in the owner's name on this nest.
    let err = dispatch(
        &router,
        state.clone(),
        relayer.actor_id().0,
        "fauna.conversations.room.create",
        birth_payload(&owner, birth_salt(4), None),
    )
    .await
    .expect_err("only the creating principal mints its own room");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

#[tokio::test]
async fn a_replayed_birth_record_re_derives_the_same_room() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();

    let first = create_room(&router, &state, &owner, birth_salt(5)).await;
    let second = create_room(&router, &state, &owner, birth_salt(5)).await;
    assert_eq!(
        first.room_id, second.room_id,
        "the id is content-derived, so a replay is the same room"
    );
    let room_id = room_id_of(&first.room_id);
    assert_eq!(
        state
            .db
            .list_floor_roster(&room_id)
            .await
            .expect("roster")
            .len(),
        2,
        "a replay does not duplicate the founding roster"
    );
}

// ── the bound the birth record closes ───────────────────────────

#[tokio::test]
async fn a_ceremony_born_rooms_floor_is_not_replaceable_by_a_report() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let attacker = ActorKeypair::generate();

    let reply = create_room(&router, &state, &owner, birth_salt(6)).await;
    let room_id = room_id_of(&reply.room_id);

    // The attacker is a legitimate member of the community room and has sent
    // at least once, so it clears BOTH of the report door's existing gates:
    // it is on the channel's routing roster, and it is a live member of the
    // stored roster. Without a provenance check it would now replace the
    // authoritative roster wholesale and name itself owner.
    seat_on_routing_roster(&state, &attacker, &room_id).await;
    state
        .db
        .replace_floor_roster(
            &room_id,
            "community",
            "",
            Some(1),
            None,
            &[
                fauna_nest::db::rooms::ReportedMember {
                    principal_id: owner.actor_id().0,
                    principal_kind: "user".into(),
                    role: Some("owner".into()),
                    home_node_url: String::new(),
                    reception_pubkey: Vec::new(),
                },
                fauna_nest::db::rooms::ReportedMember {
                    principal_id: attacker.actor_id().0,
                    principal_kind: "user".into(),
                    role: Some("member".into()),
                    home_node_url: String::new(),
                    reception_pubkey: Vec::new(),
                },
            ],
        )
        .await
        .expect("seat the attacker as an ordinary member");

    let err = dispatch(
        &router,
        state.clone(),
        attacker.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(
            &reply.room_id,
            vec![entry(&attacker, "owner"), entry(&owner, "member")],
        ),
    )
    .await
    .expect_err("the mirror door does not write a floor-authoritative room");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &owner.actor_id().0)
            .await
            .expect("floor read"),
        Some("owner".to_string()),
        "the real owner still owns the room"
    );
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &attacker.actor_id().0)
            .await
            .expect("floor read"),
        Some("member".to_string()),
        "and the attacker did not promote itself"
    );
}

// ── the roster read — `fauna.conversations.room.list_roster` ────

#[tokio::test]
async fn a_member_reads_the_floor_roster_and_a_stranger_does_not() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(8)).await;
    let req = RoomListRosterRequest {
        room_id: created.room_id.clone(),
        at_policy_version: None,
        extra: std::collections::BTreeMap::new(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.list_roster",
        payload.clone(),
    )
    .await
    .expect("a member reads its own room's floor");
    let reply: RoomListRosterReply = decode(&bytes).unwrap();
    assert_eq!(reply.members.len(), 2);
    let nest_hex = hex::encode(state.nest_identity.public_key_bytes());
    let nest_entry = reply
        .members
        .iter()
        .find(|m| m.principal == nest_hex)
        .expect("the home nest is rendered on the roster");
    assert_eq!(nest_entry.kind, "nest");
    assert_eq!(nest_entry.role.as_deref(), Some("member"));

    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.list_roster",
        payload,
    )
    .await
    .expect_err("a room's membership is not public");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

/// **The roster read's handle is JOINED from `users.handle`, and is absent —
/// not guessed — for a principal this nest holds no user row for**
/// (`conversation-rooms.md` § Implementation status today, the roster bullet;
/// § The floor roster for why it may not ride the member report).
///
/// The three answers this asserts are the whole contract the client's
/// id-keyed handle read consumes:
///
/// - a **local user with a handle** → `Some(handle)` + this nest's domain, so
///   the member seats as the canonical `handle@domain`;
/// - a **local user with no handle set** → `None`, because the empty string
///   `users.handle` defaults to is "no usable handle", not a name;
/// - a principal with **no local `users` row at all** (a member homed on
///   another nest, and the room's own nest principal) → `None` for both
///   fields. That is the decided federation answer, not an oversight: such a
///   member renders as its elided actor id.
///
/// The join is also what makes the handle *unforgeable by a member*: it is
/// read from this nest's own user records at read time, so no roster report —
/// which for an end-to-end room is a member-reported mirror — can assert one.
#[tokio::test]
async fn the_roster_read_joins_the_handle_it_knows_and_elides_the_rest() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let nameless_local = ActorKeypair::generate();
    let foreign = ActorKeypair::generate();

    state
        .db
        .create_user_with_handle(&owner.actor_id().0, "free", "alice", None)
        .await
        .expect("a local user with a handle");
    state
        .db
        .create_user_with_handle(&nameless_local.actor_id().0, "free", "", None)
        .await
        .expect("a local user who never set a handle");
    // `foreign` deliberately gets NO `users` row — it stands for a member
    // homed on another nest.

    let created = create_room(&router, &state, &owner, birth_salt(11)).await;
    let room_id = room_id_of(&created.room_id);
    let seat = |kp: &ActorKeypair, role: &str, home: &str| fauna_nest::db::rooms::ReportedMember {
        principal_id: kp.actor_id().0,
        principal_kind: "user".into(),
        role: Some(role.to_string()),
        home_node_url: home.to_string(),
        reception_pubkey: Vec::new(),
    };
    state
        .db
        .replace_floor_roster(
            &room_id,
            "community",
            "",
            Some(1),
            None,
            &[
                seat(&owner, "owner", ""),
                seat(&nameless_local, "member", ""),
                // A member whose home is elsewhere — the case the handle join
                // structurally cannot answer.
                seat(&foreign, "member", "https://other.nest.example"),
                // The room's own nest principal, carried through the wholesale
                // replace so the room stays shaped as it was born.
                fauna_nest::db::rooms::ReportedMember {
                    principal_id: state.nest_identity.public_key_bytes(),
                    principal_kind: "nest".into(),
                    role: Some("member".into()),
                    home_node_url: String::new(),
                    reception_pubkey: Vec::new(),
                },
            ],
        )
        .await
        .expect("seat the three users on the floor");

    let payload = Bytes::from(
        encode_canonical(&RoomListRosterRequest {
            room_id: created.room_id.clone(),
            at_policy_version: None,
            extra: std::collections::BTreeMap::new(),
        })
        .unwrap()
        .to_vec(),
    );
    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.list_roster",
        payload,
    )
    .await
    .expect("a member reads its own room's floor");
    let reply: RoomListRosterReply = decode(&bytes).unwrap();

    let of = |kp: &ActorKeypair| {
        let hex = kp.actor_id().to_hex();
        reply
            .members
            .iter()
            .find(|m| m.principal == hex)
            .expect("the principal is on the roster")
            .clone()
    };

    let owner_row = of(&owner);
    assert_eq!(
        owner_row.handle.as_deref(),
        Some("alice"),
        "a local user's handle is joined from its own `users` row"
    );
    assert_eq!(
        owner_row.domain.as_deref(),
        Some(state.handle_domain().as_str()),
        "paired with this nest's handle domain, so the member seats as the \
         canonical `handle@domain`"
    );

    let nameless_row = of(&nameless_local);
    assert_eq!(
        nameless_row.handle, None,
        "an empty `users.handle` is no usable handle, carried as absent \
         rather than as an empty name"
    );
    assert_eq!(
        nameless_row.domain.as_deref(),
        Some(state.handle_domain().as_str()),
        "the principal is still local, so its domain is known even when its \
         handle is not"
    );

    let foreign_row = of(&foreign);
    assert_eq!(
        foreign_row.handle, None,
        "a principal with no local `users` row - a member homed on another \
         nest - is elided, never guessed: this nest caches no federated \
         profile, exactly as `fauna.contacts.list` answers for a federated peer"
    );
    assert_eq!(foreign_row.domain, None, "and carries no domain either");

    let nest_hex = hex::encode(state.nest_identity.public_key_bytes());
    let nest_row = reply
        .members
        .iter()
        .find(|m| m.principal == nest_hex)
        .expect("the home nest is on the roster");
    assert_eq!(
        nest_row.handle, None,
        "the room's own nest principal is not a user and holds no handle"
    );
}

#[test]
fn the_birth_and_roster_read_kinds_are_user_class_only() {
    for kind in [
        "fauna.conversations.room.create",
        "fauna.conversations.room.list_roster",
    ] {
        assert!(is_permitted(CallerClass::User, kind));
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::BridgeAtprotoPds,
            CallerClass::Custodian,
            CallerClass::ContentProcessor,
        ] {
            assert!(!is_permitted(class, kind), "{class:?} founds no rooms");
        }
    }
}

// ── the membership half — invite / accept / remove / leave ──────
//
// A community room's floor is AUTHORITATIVE (`conversation-rooms.md`
// § The floor roster), so these four doors are the room's membership, not a
// mirror of one. The role rules are the same table the end-to-end class
// enforces in `judge_commit` — owner and admins invite and remove, the
// owner is not removable, a member leaves — applied at the floor instead of
// in a commit verdict.

fn invite_payload(
    inviter: &ActorKeypair,
    room_id: &[u8; 32],
    invitee: &ActorKeypair,
    role: fauna_mls::room_policy::RoomRole,
) -> Bytes {
    let invite = fauna_mls::room_policy::RoomInvite {
        room_id: room_id.to_vec(),
        invitee: invitee.actor_id(),
        role,
        policy_version: 1,
    };
    let signed = invite.sign(inviter).expect("the inviter signs its own act");
    let req = RoomInviteRequest {
        invite: encode_canonical(&signed).unwrap().to_vec(),
        invitee_node: String::new(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn accept_payload(room_hex: &str) -> Bytes {
    accept_payload_with_key(room_hex, Vec::new())
}

fn accept_payload_with_key(room_hex: &str, reception_pubkey: Vec<u8>) -> Bytes {
    let req = RoomAcceptInviteRequest {
        room_id: room_hex.into(),
        reception_pubkey,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn leave_payload(room_hex: &str) -> Bytes {
    let req = RoomLeaveRequest {
        room_id: room_hex.into(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn remove_payload(room_hex: &str, target: &ActorKeypair) -> Bytes {
    let req = RoomRemoveRequest {
        room_id: room_hex.into(),
        principal: hex::encode(target.actor_id().0),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn list_invites_payload(room_hex: &str) -> Bytes {
    let req = RoomListInvitesRequest {
        room_id: room_hex.into(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn revoke_invite_payload(room_hex: &str, invitee: &ActorKeypair) -> Bytes {
    let req = RoomRevokeInviteRequest {
        room_id: room_hex.into(),
        invitee: hex::encode(invitee.actor_id().0),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Let `who` accept conversations from `inviter` — the invitee's own reach
/// policy, which gates a room invitation exactly as it gates a group Welcome
/// (`conversation-rooms.md` § Join rules and invites). Without this edge the
/// stored default (`allow_knock`) refuses a first-reach invitation, which is
/// the ratified behaviour and is pinned by
/// `an_invitation_to_a_stranger_is_refused_by_their_reach_policy` below.
async fn accept_contact(state: &AppState, who: &ActorKeypair, from: &ActorKeypair) {
    state
        .db
        .upsert_contact(&who.actor_id().0, &from.actor_id().0, "accepted")
        .await
        .expect("contact edge");
}

/// Invite `who` and have them accept — the two-step seating every test below
/// needs before it has a room with more than its founder in it.
async fn seat_member(
    router: &RpcRouter,
    state: &Arc<AppState>,
    inviter: &ActorKeypair,
    room_hex: &str,
    room_id: &[u8; 32],
    who: &ActorKeypair,
    role: fauna_mls::room_policy::RoomRole,
) {
    accept_contact(state, who, inviter).await;
    dispatch(
        router,
        state.clone(),
        inviter.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(inviter, room_id, who, role),
    )
    .await
    .expect("the invitation is recorded");
    dispatch(
        router,
        state.clone(),
        who.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(room_hex),
    )
    .await
    .expect("the invitee accepts");
}

/// Seat `who` as an ADMIN — the honest two-step, because an admin rank comes
/// from the owner-signed policy and never from an invitation: the nest
/// cannot author a policy, so seating an admin the policy does not name
/// would make the nest enforce a rank members do not render.
///
/// Reads the room's current version and admin set off the floor rather than
/// tracking them in the test, so a caller can seat several admins in
/// sequence without book-keeping.
async fn seat_admin(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    room_hex: &str,
    room_id: &[u8; 32],
    who: &ActorKeypair,
) {
    seat_member(
        router,
        state,
        owner,
        room_hex,
        room_id,
        who,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    let room = state
        .db
        .get_room(room_id)
        .await
        .expect("room read")
        .expect("the room record");
    let mut admins: Vec<ActorId> = state
        .db
        .list_floor_roster(room_id)
        .await
        .expect("roster")
        .into_iter()
        .filter(|m| m.principal_kind == "user" && m.role.as_deref() == Some("admin"))
        .map(|m| ActorId(m.principal_id))
        .collect();
    admins.push(who.actor_id());

    let mut policy =
        fauna_mls::room_policy::RoomPolicy::initial(owner.actor_id(), Some("the square".into()));
    policy.version = room.policy_version.unwrap_or(1) + 1;
    policy.set_admins(admins);
    dispatch(
        router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(room_hex, owner, &policy),
    )
    .await
    .expect("the owner appoints the admin");
}

#[tokio::test]
async fn an_invite_does_not_seat_a_member_and_acceptance_does() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let guest = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(10)).await;
    let room_id = room_id_of(&created.room_id);
    accept_contact(&state, &guest, &owner).await;

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("the owner may invite");
    let reply: RoomInviteReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.role, "member");

    // The whole point of the two-step: a room's floor never names someone
    // who has not agreed to be there.
    assert!(
        !state
            .db
            .is_room_member(&room_id, &guest.actor_id().0)
            .await
            .expect("floor read"),
        "an invitation alone seats nobody"
    );

    let accept_bytes = dispatch(
        &router,
        state.clone(),
        guest.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(&created.room_id),
    )
    .await
    .expect("the invitee accepts its own invitation");
    let accepted: RoomAcceptInviteReply = decode(&accept_bytes).unwrap();
    assert_eq!(accepted.role, "member");
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &guest.actor_id().0)
            .await
            .expect("floor read"),
        Some("member".to_string()),
        "acceptance is what seats the member"
    );

    // And the accept is one-shot: replaying it must not re-seat a principal
    // the room may have removed in between.
    let err = dispatch(
        &router,
        state.clone(),
        guest.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(&created.room_id),
    )
    .await
    .expect_err("a settled invitation is not pending");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

#[tokio::test]
async fn a_plain_member_may_not_invite_under_the_invite_join_rule() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(11)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    // `RoomPolicy::initial` is invite-only, so this is the default room.
    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &member,
            &room_id,
            &stranger,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect_err("the invite join rule reserves invitations to owner and admins");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // An admin, by contrast, may — the same `judge_commit` rule the
    // end-to-end class applies to an Add.
    let admin = ActorKeypair::generate();
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;
    accept_contact(&state, &stranger, &admin).await;
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &admin,
            &room_id,
            &stranger,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("an admin invites");
}

#[tokio::test]
async fn an_invitation_is_the_inviters_own_signed_act() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let relayer = ActorKeypair::generate();
    let guest = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(12)).await;
    let room_id = room_id_of(&created.room_id);
    accept_contact(&state, &guest, &owner).await;

    // Correctly signed by the owner, dispatched by somebody else.
    let err = dispatch(
        &router,
        state.clone(),
        relayer.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect_err("a third party does not relay an invitation into this nest");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // And an invitation naming the OWNER role is refused: a room has exactly
    // one owner and it moves by transfer. `RoomInvite::sign` refuses to
    // produce one at all, so an honest client cannot even build this — the
    // payload here is hand-signed past that guard, which is exactly the
    // shape a dishonest one would send.
    let invite = fauna_mls::room_policy::RoomInvite {
        room_id: room_id.to_vec(),
        invitee: guest.actor_id(),
        role: fauna_mls::room_policy::RoomRole::Owner,
        policy_version: 1,
    };
    assert!(
        invite.sign(&owner).is_err(),
        "an honest client cannot sign an owner invitation"
    );
    let signed = fauna_mls::room_policy::SignedRoomInvite {
        invite,
        inviter: owner.actor_id(),
        signature: vec![0u8; 64],
    };
    let req = RoomInviteRequest {
        invite: encode_canonical(&signed).unwrap().to_vec(),
        invitee_node: String::new(),
        extra: std::collections::BTreeMap::new(),
    };
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("ownership is transferred, never invited");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

/// **An invitation is DELIVERED to the invitee's inbox, in the same act that
/// records it** — the hop the whole community class hung on
/// (`conversation-rooms.md` § Join rules and invites: an invite is "delivered to
/// the invitee's home nest through the inbox plane").
///
/// Before this, `room.invite` wrote its own table and stopped, so the accept
/// door's `room_id` could only be filled by a human reading the id out of a log.
/// What is pinned here is the whole production flow, through the real doors:
/// the inviter dispatches `room.invite` → the handler records the invitation and
/// enqueues one `InboxEnvelope` in one transaction → the **invitee's own**
/// `fauna.inbox.fetch` returns it → its payload is the inviter's signed act
/// verbatim, which verifies under the inviter and names the room the invitee
/// then accepts into.
///
/// The "same act" half is what the two-in-one DB method buys, and it is asserted
/// by the negative case at the end: an invitation the door REFUSES delivers
/// nothing, so an invitee is never knocked at about a room it cannot join.
#[tokio::test]
async fn an_invitation_is_delivered_to_the_invitees_inbox_naming_the_room() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
    let router = b.build();

    let owner = ActorKeypair::generate();
    let guest = ActorKeypair::generate();
    let created = create_room(&router, &state, &owner, birth_salt(77)).await;
    let room_id = room_id_of(&created.room_id);
    accept_contact(&state, &guest, &owner).await;

    // Nothing is standing before the invitation — so what the fetch returns
    // below cannot be residue this room did not put there.
    let empty: fauna_protocol::inbox::InboxFetchReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.inbox.fetch",
            fetch_payload(),
        )
        .await
        .expect("the invitee reads its own inbox"),
    )
    .unwrap();
    assert!(empty.items.is_empty(), "the invitee's inbox starts empty");

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("the owner invites");

    let reply: fauna_protocol::inbox::InboxFetchReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.inbox.fetch",
            fetch_payload(),
        )
        .await
        .expect("the invitee reads its own inbox"),
    )
    .unwrap();
    assert_eq!(reply.items.len(), 1, "one delivered invitation");

    let env = fauna_protocol::inbox::InboxEnvelope::from_canonical_bytes(&reply.items[0].payload)
        .expect("the row is a canonical inbox envelope");
    assert_eq!(
        env.kind,
        fauna_protocol::inbox::InboxKind::RoomInvite,
        "the room plane mints no read kind — it rides the inbox plane's own discriminator"
    );
    let payload = env.decode_room_invite().expect("the payload decodes");
    assert!(
        payload.room_node.is_none(),
        "a room this nest homes needs no next hop"
    );
    let signed: fauna_mls::room_policy::SignedRoomInvite =
        fauna_core::encoding::canonical_decode(&payload.signed_invite)
            .expect("the signed act survived the envelope verbatim");
    signed
        .verify_signature()
        .expect("the delivered record verifies under the inviter it names");
    assert_eq!(signed.inviter, owner.actor_id());
    assert_eq!(signed.invite.invitee, guest.actor_id());
    assert_eq!(
        signed.invite.room_id,
        room_id.to_vec(),
        "the delivered record NAMES the room — this is the id the invitee accepts with"
    );

    // And it is the id that works: the invitee accepts using nothing but what
    // the delivery told it.
    let accepted: RoomAcceptInviteReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.conversations.room.accept_invite",
            accept_payload(&hex::encode(&signed.invite.room_id)),
        )
        .await
        .expect("the invitee accepts the room it was told about"),
    )
    .unwrap();
    assert_eq!(accepted.role, "member");

    // Accepting consumes the knock: the nest itself acks
    // the standing envelope in the same transaction that seats the invitee,
    // so nothing is left telling an already-seated member to accept.
    let after_accept: fauna_protocol::inbox::InboxFetchReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.inbox.fetch",
            fetch_payload(),
        )
        .await
        .expect("the invitee reads its own inbox"),
    )
    .unwrap();
    assert!(
        after_accept.items.is_empty(),
        "accepting leaves no standing invitation for this room"
    );

    // A refused invitation delivers nothing: re-inviting a seated member is
    // rejected, and the record and the knock are written together or not at
    // all, so no envelope lands.
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect_err("a seated member is not invited again");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
    let after: fauna_protocol::inbox::InboxFetchReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.inbox.fetch",
            fetch_payload(),
        )
        .await
        .expect("the invitee reads its own inbox"),
    )
    .unwrap();
    assert!(
        after.items.is_empty(),
        "the refused invitation delivered nothing"
    );
}

/// **N calls to `room.invite` for one (room, invitee) leave exactly one
/// un-acked inbox row, never N**. Before this, the inbox
/// was an unbounded append log for this door alone: a lost-reply retry — or
/// any inviter the invitee's reach policy admits, repeatedly — piled a fresh
/// durable, un-quota'd envelope on every call, hiding genuine invitations
/// behind the peek's page limit. Pinned with the security review's own probe
/// shape (three calls), plus the quota half the same fix buys.
#[tokio::test]
async fn repeated_invites_leave_exactly_one_standing_envelope_and_are_quota_charged() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    *state.enforce_tier_quotas.write().await = true;
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
    let router = b.build();

    let owner = ActorKeypair::generate();
    let guest = ActorKeypair::generate();
    let created = create_room(&router, &state, &owner, birth_salt(79)).await;
    let room_id = room_id_of(&created.room_id);
    accept_contact(&state, &guest, &owner).await;
    state
        .db
        .create_user(&guest.actor_id().0, "free", "guest")
        .await
        .expect("the invitee is a registered tenant so quota accounting applies");

    for _ in 0..3 {
        dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.invite",
            invite_payload(
                &owner,
                &room_id,
                &guest,
                fauna_mls::room_policy::RoomRole::Member,
            ),
        )
        .await
        .expect("re-inviting a not-yet-answered invitee refreshes it, never errors");
    }

    let after_three_calls: fauna_protocol::inbox::InboxFetchReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.inbox.fetch",
            fetch_payload(),
        )
        .await
        .expect("the invitee reads its own inbox"),
    )
    .unwrap();
    assert_eq!(
        after_three_calls.items.len(),
        1,
        "three calls for one (room, invitee) still leave exactly one un-acked row"
    );

    let envelope_size = after_three_calls.items[0].payload.len() as i64;
    let charged = state
        .db
        .get_user(&guest.actor_id().0)
        .await
        .unwrap()
        .expect("registered above")
        .inbox_bytes_used;
    assert_eq!(
        charged, envelope_size,
        "the one standing envelope is charged against the invitee's inbox quota, \
         not left outside it — three deliveries and two refunds, net one charge"
    );

    let accepted: RoomAcceptInviteReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.conversations.room.accept_invite",
            accept_payload(&created.room_id),
        )
        .await
        .expect("the invitee accepts"),
    )
    .unwrap();
    assert_eq!(accepted.role, "member");

    let after_accept: fauna_protocol::inbox::InboxFetchReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.inbox.fetch",
            fetch_payload(),
        )
        .await
        .expect("the invitee reads its own inbox"),
    )
    .unwrap();
    assert!(
        after_accept.items.is_empty(),
        "accepting consumes the standing envelope this invitation delivered"
    );
    let bytes_after_accept = state
        .db
        .get_user(&guest.actor_id().0)
        .await
        .unwrap()
        .unwrap()
        .inbox_bytes_used;
    assert_eq!(
        bytes_after_accept, 0,
        "the accepted envelope's charge is refunded, exactly as an ordinary ack refunds it"
    );
}

/// An invitee registered at a tier with no inbox headroom refuses even a
/// **first** invitation — the quota check runs inside the same transaction
/// that would deliver and charge the envelope, and a refusal there returns
/// without committing, so nothing is recorded, delivered, or charged
/// . Mutate the in-transaction check out and
/// this reds.
#[tokio::test]
async fn a_first_invitation_over_quota_is_refused_and_delivers_nothing() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    *state.enforce_tier_quotas.write().await = true;
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
    let router = b.build();

    let owner = ActorKeypair::generate();
    let guest = ActorKeypair::generate();
    let created = create_room(&router, &state, &owner, birth_salt(82)).await;
    let room_id = room_id_of(&created.room_id);
    accept_contact(&state, &guest, &owner).await;
    // The seeded `backup` tier is storage-only (`max_inbox_bytes == 0`), so
    // any envelope at all is over quota.
    state
        .db
        .create_user(&guest.actor_id().0, "backup", "guest")
        .await
        .expect("the invitee is registered at a tier with no inbox headroom");

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect_err("a tier with no inbox headroom refuses even a first invitation");
    assert_eq!(err.code, "fauna.conversations.forbidden");

    assert!(
        state
            .db
            .get_pending_room_invite(&room_id, &guest.actor_id().0)
            .await
            .expect("invite read")
            .is_none(),
        "a refused invitation is not recorded"
    );
    let after: fauna_protocol::inbox::InboxFetchReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.inbox.fetch",
            fetch_payload(),
        )
        .await
        .expect("the invitee reads its own inbox"),
    )
    .unwrap();
    assert!(
        after.items.is_empty(),
        "the refused invitation delivered nothing"
    );
    assert_eq!(
        state
            .db
            .get_user(&guest.actor_id().0)
            .await
            .unwrap()
            .expect("registered above")
            .inbox_bytes_used,
        0,
        "the refused invitation charged nothing"
    );
}

/// A pending invitee whose quota is exactly filled by its own standing
/// envelope still accepts a re-invitation that merely replaces it — the
/// refund the re-invitation performs runs, inside the transaction, *before*
/// the quota check, so the check never sees the envelope it is about to
/// consume as the reason there is no room .
/// Mutate the refund-before-check order back and this reds.
#[tokio::test]
async fn a_reinvitation_that_nets_zero_is_not_refused_by_its_own_standing_envelope() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    *state.enforce_tier_quotas.write().await = true;
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
    let router = b.build();

    let owner = ActorKeypair::generate();
    let guest = ActorKeypair::generate();
    let created = create_room(&router, &state, &owner, birth_salt(83)).await;
    let room_id = room_id_of(&created.room_id);
    accept_contact(&state, &guest, &owner).await;
    state
        .db
        .create_user(&guest.actor_id().0, "free", "guest")
        .await
        .expect("the invitee is a registered tenant so quota accounting applies");

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("the first invitation is delivered under the free tier's ample quota");

    let envelope_size = state
        .db
        .get_user(&guest.actor_id().0)
        .await
        .unwrap()
        .expect("registered above")
        .inbox_bytes_used;

    // Shrink the invitee's tier to exactly the standing envelope's size, so
    // the invitee sits AT quota with no headroom at all.
    let mut tier = state
        .db
        .get_tier("free")
        .await
        .unwrap()
        .expect("free tier seeded");
    tier.max_inbox_bytes = envelope_size;
    state
        .db
        .update_tier(&tier)
        .await
        .expect("shrink the free tier's inbox ceiling");

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("a re-invitation that nets zero is not refused by its own standing envelope");

    let after: fauna_protocol::inbox::InboxFetchReply = decode(
        &dispatch(
            &router,
            state.clone(),
            guest.actor_id().0,
            "fauna.inbox.fetch",
            fetch_payload(),
        )
        .await
        .expect("the invitee reads its own inbox"),
    )
    .unwrap();
    assert_eq!(
        after.items.len(),
        1,
        "the re-invitation replaced the standing envelope, not appended to it"
    );
    assert_eq!(
        state
            .db
            .get_user(&guest.actor_id().0)
            .await
            .unwrap()
            .expect("registered above")
            .inbox_bytes_used,
        envelope_size,
        "still charged exactly once, sitting at the tier's own ceiling"
    );
}

/// A cursor-less `fauna.inbox.fetch` at the handler default — the peek every
/// app's invitation list makes.
fn fetch_payload() -> Bytes {
    Bytes::from(
        encode_canonical(&fauna_protocol::inbox::InboxFetchRequest {
            limit: 0,
            after_id: None,
            extra: std::collections::BTreeMap::new(),
        })
        .unwrap()
        .to_vec(),
    )
}

/// **The roster read serves the signed policy its version names** — the read a
/// policy change is authored from.
///
/// Before it, a client could learn a room's policy *version* and nothing else:
/// no door served the bytes. That left `set_policy` unusable rather than merely
/// awkward, because a change is a **replacement** at `stored + 1` — an app
/// authoring one without the stored document would carry defaults for every
/// field it did not show the user, and storing it would silently reset them.
///
/// It rides this read rather than a `room.get_policy` kind because it is the
/// same read, and it widens nothing: `list_roster` is admitted only to a live
/// member of the room, and a member is exactly who renders and verifies a
/// policy.
#[tokio::test]
async fn the_roster_read_serves_the_signed_policy_its_version_names() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let created = create_room(&router, &state, &owner, birth_salt(0x9a)).await;

    let read = |actor: [u8; 32]| {
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.room.list_roster",
            Bytes::from(
                encode_canonical(&RoomListRosterRequest {
                    room_id: created.room_id.clone(),
                    at_policy_version: None,
                    extra: std::collections::BTreeMap::new(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
    };

    let reply: RoomListRosterReply = decode(
        &read(owner.actor_id().0)
            .await
            .expect("a member reads the roster"),
    )
    .unwrap();
    let blob = reply
        .policy
        .expect("the read carries the policy, not only its version");
    let signed: fauna_mls::room_policy::SignedRoomPolicy =
        fauna_core::encoding::canonical_decode(&blob).expect("the stored bytes decode");
    signed
        .verify_signature_community()
        .expect("they are the bytes their author signed — the member verifies for itself");
    assert_eq!(signed.signer, owner.actor_id());
    assert_eq!(signed.policy.owner, owner.actor_id());
    assert_eq!(
        Some(signed.policy.version),
        reply.policy_version,
        "the bytes are the version the same reply names — a client that trusted one and \
         authored against the other would build a replacement the ratchet refuses"
    );

    // And the policy is no more public than the roster already was: the read
    // itself is member-gated, so a stranger who knows the room id learns
    // neither the members nor the policy.
    let err = read(ActorKeypair::generate().actor_id().0)
        .await
        .expect_err("a non-member reads nothing here");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

/// **The roster read serves a superseded policy version by number** — what a
/// member judges a floor delete record against (`conversation-rooms.md`
/// § Roles and authorization → *Delete any message — the mechanism* →
/// *Community rooms*: "the home nest retains every superseded signed policy
/// version and serves one by number").
#[tokio::test]
async fn the_roster_read_serves_a_superseded_policy_version_by_number() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let created = create_room(&router, &state, &owner, birth_salt(0x9b)).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &owner,
            &policy_at(&owner, 2, &[], "renamed"),
        ),
    )
    .await
    .expect("the owner supersedes the founding version");

    let read = |at: Option<u64>| {
        let router = &router;
        let state = state.clone();
        let room_id = created.room_id.clone();
        let actor = owner.actor_id().0;
        async move {
            let bytes = dispatch(
                router,
                state,
                actor,
                "fauna.conversations.room.list_roster",
                Bytes::from(
                    encode_canonical(&RoomListRosterRequest {
                        room_id,
                        at_policy_version: at,
                        extra: std::collections::BTreeMap::new(),
                    })
                    .unwrap()
                    .to_vec(),
                ),
            )
            .await
            .expect("a member reads the roster");
            decode::<RoomListRosterReply>(&bytes).unwrap()
        }
    };

    let reply = read(Some(1)).await;
    assert_eq!(
        reply.policy_version,
        Some(2),
        "the roles are still the current version's"
    );
    let at: fauna_mls::room_policy::SignedRoomPolicy = fauna_core::encoding::canonical_decode(
        &reply
            .policy_at_version
            .expect("the founding version is retained"),
    )
    .expect("stored as signed");
    assert_eq!(at.policy.version, 1, "served by the number it carries");
    assert_eq!(at.signer, owner.actor_id());

    // … and beside it the birth salt, which is what lets the member ANCHOR
    // what it was served rather than believe it (§ *Members verify what they
    // paint*): version 1 proves itself the founder's against the room id, and
    // version 2 proves it follows — by the one judge both sides run.
    let salt: [u8; 32] = reply
        .birth_salt
        .expect("the birth salt rides the versioned read")
        .as_slice()
        .try_into()
        .expect("32 bytes");
    let room: [u8; 32] = hex::decode(&created.room_id).unwrap().try_into().unwrap();
    let mut chain = fauna_mls::room_policy::CommunityPolicyChain::anchor(&room, &salt, at)
        .expect("the served founding version anchors to the room id");
    let second: fauna_mls::room_policy::SignedRoomPolicy = fauna_core::encoding::canonical_decode(
        &read(Some(2))
            .await
            .policy_at_version
            .expect("the current version is served by number too"),
    )
    .unwrap();
    chain
        .extend(second, &fauna_mls::room_policy::names_only)
        .expect("the served chain verifies link by link");

    let unheld = read(Some(9)).await;
    assert!(
        unheld.policy_at_version.is_none(),
        "a version the room never held serves nothing — the member fails closed"
    );
    let plain = read(None).await;
    assert!(
        plain.policy_at_version.is_none() && plain.birth_salt.is_none(),
        "and a read that names none is the read it always was"
    );
}

#[tokio::test]
async fn an_owner_or_admin_removes_a_member_and_a_member_does_not() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let other = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(13)).await;
    let room_id = room_id_of(&created.room_id);
    for who in [&member, &other] {
        seat_member(
            &router,
            &state,
            &owner,
            &created.room_id,
            &room_id,
            who,
            fauna_mls::room_policy::RoomRole::Member,
        )
        .await;
    }
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;

    // A plain member holds no removal rank.
    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&created.room_id, &other),
    )
    .await
    .expect_err("only an owner or admin removes");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // An admin does.
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&created.room_id, &other),
    )
    .await
    .expect("an admin removes a member");
    assert!(
        !state
            .db
            .is_room_member(&room_id, &other.actor_id().0)
            .await
            .expect("floor read"),
        "the removed principal is off the live floor"
    );
    // Absorbed as history, not deleted — the succession axis's shape.
    assert!(
        state
            .db
            .list_floor_roster_including_removed(&room_id)
            .await
            .expect("roster")
            .iter()
            .any(|m| m.principal_id == other.actor_id().0 && m.removed_at.is_some()),
        "the row stays as history with a removal stamp"
    );
}

#[tokio::test]
async fn the_owners_membership_is_not_removable_and_the_owner_cannot_leave() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(14)).await;
    let room_id = room_id_of(&created.room_id);
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;

    // An owner-less roster would strand invite and remove forever, so
    // neither door can produce one.
    let err = dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&created.room_id, &owner),
    )
    .await
    .expect_err("an admin does not remove the owner");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&created.room_id),
    )
    .await
    .expect_err("the owner transfers before leaving");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &owner.actor_id().0)
            .await
            .expect("floor read"),
        Some("owner".to_string()),
        "the room still has its owner"
    );
}

#[tokio::test]
async fn a_member_leaves_and_a_removed_member_cannot_walk_back_in() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(15)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    let bytes = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&created.room_id),
    )
    .await
    .expect("a member leaves");
    let reply: RoomLeaveReply = decode(&bytes).unwrap();
    assert_eq!(reply.members, 2, "owner + home nest remain");

    // The departure clears the settled invitation with it, so replaying the
    // accept cannot re-seat the leaver — the roster's own version of the
    // report door's ratchet, and the same attack in a different coat.
    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(&created.room_id),
    )
    .await
    .expect_err("a departed member does not re-seat itself on an old invitation");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // Re-invitation is the honest way back, and it preserves the original
    // join stamp on the same row.
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;
    assert!(
        state
            .db
            .is_room_member(&room_id, &member.actor_id().0)
            .await
            .expect("floor read"),
        "a re-invited principal comes back"
    );
}

#[tokio::test]
async fn the_home_nests_membership_is_not_removable_by_the_membership_door() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(16)).await;
    let nest_hex = hex::encode(state.nest_identity.public_key_bytes());
    let req = RoomRemoveRequest {
        room_id: created.room_id.clone(),
        principal: nest_hex,
        extra: std::collections::BTreeMap::new(),
    };

    // Removing the home nest is the materialization grant's REVOKE — it
    // rotates the room generation and deletes the derived views. Unseating
    // it here alone would leave the nest holding a live wrap over a room
    // whose roster says it is gone.
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.remove",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("the read-grant revoke ceremony owns this, and it is not built");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

#[tokio::test]
async fn the_membership_doors_refuse_a_room_whose_authority_is_its_mls_group() {
    // The mirror image of the report door's gate 0: a room born through a
    // roster report has its membership decided by an MLS group the nest
    // cannot read, so the floor doors must not write it. One room, one
    // membership authority.
    //
    // `room.leave` is not in this list, and deliberately so: a departure
    // asserts only the caller's own absence, which is exactly what a member
    // of a mirror may say about itself (`conversation-rooms.md` § Roles and
    // authorization → *Leaving — the mechanism*). Its end-to-end cases are
    // the `an_end_to_end_*` tests below.
    let (router, state) = router_with_db_only().await;
    let member = ActorKeypair::generate();
    let guest = ActorKeypair::generate();
    let room_hex = hex::encode([17u8; 32]);
    let room_id = room_id_of(&room_hex);

    seat_on_routing_roster(&state, &member, &room_id).await;
    dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&member, "owner")]),
    )
    .await
    .expect("the mirror door mints the room");

    for (kind, payload) in [
        (
            "fauna.conversations.room.invite",
            invite_payload(
                &member,
                &room_id,
                &guest,
                fauna_mls::room_policy::RoomRole::Member,
            ),
        ),
        (
            "fauna.conversations.room.accept_invite",
            accept_payload(&room_hex),
        ),
        (
            "fauna.conversations.room.remove",
            remove_payload(&room_hex, &guest),
        ),
        (
            "fauna.conversations.room.list_invites",
            list_invites_payload(&room_hex),
        ),
        (
            "fauna.conversations.room.revoke_invite",
            revoke_invite_payload(&room_hex, &guest),
        ),
    ] {
        let err = dispatch(&router, state.clone(), member.actor_id().0, kind, payload)
            .await
            .err()
            .unwrap_or_else(|| panic!("{kind} must refuse a room the mirror door owns"));
        assert_eq!(
            err.code, "fauna.conversations.permission_denied",
            "{kind} refuses on provenance: {err:?}"
        );
    }
}

// ── the end-to-end departure ────────────────────────────────────
//
// An end-to-end room's member leaves by the self-scoped leave door, which
// stamps the CALLER's row and touches no one else's (`conversation-rooms.md`
// § Roles and authorization → *Leaving — the mechanism*). It used to leave by
// a final roster report built from the leaver's local group view, naming no
// position — which the replace takes wholesale, so a leaver that had not yet
// folded the newest membership commit rolled the floor back to its own stale
// view.

/// An end-to-end room whose floor holds `owner` (owner) and every one of
/// `members` (member), reported at position 10 by `owner`, whose commit it was.
async fn end_to_end_floor(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    members: &[&ActorKeypair],
    room_byte: u8,
) -> (String, [u8; 32]) {
    let room_hex = hex::encode([room_byte; 32]);
    let room_id = room_id_of(&room_hex);
    seat_on_routing_roster(state, owner, &room_id).await;
    let mut roster = vec![entry(owner, "owner")];
    for m in members {
        seat_on_routing_roster(state, m, &room_id).await;
        roster.push(entry(m, "member"));
    }
    land_commit_at(state, &room_id, 10, Some(owner)).await;
    dispatch(
        router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(&room_hex, roster, Some(1), Some(10)),
    )
    .await
    .expect("the committing device's own report seats the floor");
    (room_hex, room_id)
}

#[tokio::test]
async fn an_end_to_end_leave_moves_the_leavers_seat_and_no_one_elses() {
    // `owner` removes `evicted` and adds `newcomer` by the commit at 11, and
    // reports it. `leaver` has not folded 11 yet — its engine still names
    // `evicted` and not `newcomer`. Whatever its view, its departure must not
    // re-seat the one or drop the other: the door takes no roster from it.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let leaver = ActorKeypair::generate();
    let evicted = ActorKeypair::generate();
    let newcomer = ActorKeypair::generate();
    let (room_hex, room_id) =
        end_to_end_floor(&router, &state, &owner, &[&leaver, &evicted], 70).await;

    land_commit_at(&state, &room_id, 11, Some(&owner)).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        sequenced_report_payload(
            &room_hex,
            vec![
                entry(&owner, "owner"),
                entry(&leaver, "member"),
                entry(&newcomer, "member"),
            ],
            Some(1),
            Some(11),
        ),
    )
    .await
    .expect("the remover's report at its own commit lands");

    let bytes = dispatch(
        &router,
        state.clone(),
        leaver.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&room_hex),
    )
    .await
    .expect("a member of an end-to-end room leaves by the leave door");
    let reply: RoomLeaveReply = decode(&bytes).unwrap();
    assert_eq!(reply.members, 2, "owner + newcomer remain");

    assert_eq!(live_role(&state, &room_id, &leaver).await, None, "left");
    assert_eq!(
        live_role(&state, &room_id, &evicted).await,
        None,
        "the member commit 11 removed stays removed"
    );
    assert_eq!(
        live_role(&state, &room_id, &newcomer).await.as_deref(),
        Some("member"),
        "the member commit 11 added stays on the floor"
    );
    assert_eq!(
        live_role(&state, &room_id, &owner).await.as_deref(),
        Some("owner"),
        "and the room keeps its owner"
    );
    let room = state.db.get_room(&room_id).await.unwrap().unwrap();
    assert_eq!(
        room.roster_commit_seq,
        Some(11),
        "a departure is not a commit: the floor's position does not move"
    );
    assert_eq!(
        room.owner_id.as_deref(),
        Some(owner.actor_id().0.as_slice()),
        "nor does its owner"
    );
}

#[tokio::test]
async fn a_departures_retry_converges_and_a_stranger_is_still_refused() {
    // A leave whose reply was lost is retried by its user. The unseat already
    // landed, so the retry finds no live seat — and answering that as a
    // refusal told a user who had left that they could not, on every retry,
    // for ever. A caller the floor has stamped departed is answered
    // as a converged departure; a caller the floor never seated is still
    // refused. And the report door's ratchet keeps the departed caller out:
    // the routing row survives a departure, but the stored floor no longer
    // names it live.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let leaver = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();
    let (room_hex, room_id) = end_to_end_floor(&router, &state, &owner, &[&leaver], 71).await;

    dispatch(
        &router,
        state.clone(),
        leaver.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&room_hex),
    )
    .await
    .expect("leaves");
    let retry: RoomLeaveReply = decode(
        &dispatch(
            &router,
            state.clone(),
            leaver.actor_id().0,
            "fauna.conversations.room.leave",
            leave_payload(&room_hex),
        )
        .await
        .expect("a departed caller's retry converges"),
    )
    .unwrap();
    assert_eq!(retry.members, 1, "and moved nothing: the owner remains");

    seat_on_routing_roster(&state, &stranger, &room_id).await;
    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&room_hex),
    )
    .await
    .expect_err("a caller the floor never seated has nothing to leave");
    assert_eq!(err.code, "fauna.conversations.permission_denied", "{err:?}");

    let err = dispatch(
        &router,
        state.clone(),
        leaver.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(
            &room_hex,
            vec![entry(&owner, "owner"), entry(&leaver, "member")],
        ),
    )
    .await
    .expect_err("and it does not report itself back in");
    assert_eq!(err.code, "fauna.conversations.permission_denied", "{err:?}");
    assert_eq!(
        live_role(&state, &room_id, &owner).await.as_deref(),
        Some("owner")
    );
}

#[tokio::test]
async fn an_end_to_end_owner_cannot_leave_by_the_leave_door() {
    // A room is never owner-less (§ Roles and authorization, Mechanism B's
    // rule kept): the owner's exit is a transfer and then a leave, on this
    // class exactly as on the community one, read from the stored floor's
    // rank rather than the client's word.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (room_hex, room_id) = end_to_end_floor(&router, &state, &owner, &[&member], 72).await;

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&room_hex),
    )
    .await
    .expect_err("the owner transfers first");
    assert_eq!(err.code, "fauna.conversations.permission_denied", "{err:?}");
    assert_eq!(
        live_role(&state, &room_id, &owner).await.as_deref(),
        Some("owner"),
        "refused before anything moved"
    );
}

#[tokio::test]
async fn an_end_to_end_leave_needs_a_floor_to_leave() {
    // No floor, nothing to leave: the door answers "no such room" rather than
    // minting one, exactly as for a community room that was never created.
    let (router, state) = router_with_db_only().await;
    let stranger = ActorKeypair::generate();
    let room_hex = hex::encode([73u8; 32]);
    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&room_hex),
    )
    .await
    .expect_err("no floor, no departure");
    assert_eq!(err.code, "fauna.conversations.invalid_params", "{err:?}");
    assert!(
        state
            .db
            .get_room(&room_id_of(&room_hex))
            .await
            .unwrap()
            .is_none(),
        "and no room was minted by asking"
    );
}

fn role_less_report_payload(room_hex: &str, members: &[&ActorKeypair]) -> Bytes {
    let req = RoomRosterReportRequest {
        room_id: room_hex.into(),
        members: members
            .iter()
            .map(|m| RoomRosterEntryWire {
                actor: hex::encode(m.actor_id().0),
                role: None,
                extra: Default::default(),
            })
            .collect(),
        policy_version: None,
        commit_seq: None,
        extra: Default::default(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

#[tokio::test]
async fn a_role_less_report_cannot_un_govern_a_governed_floor() {
    // A report's own shape cannot tell a policy-less room from a governed one whose
    // policy the reporting device could not read — both arrive role-less — so
    // judging the REPORT would take it as policy-less and write `owner_id = NULL`,
    // and the room would then render owner-less and let its owner walk out.
    // The door judges the STORED room: a floor that names an owner refuses a
    // report that names none, role-less or not.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (room_hex, room_id) = end_to_end_floor(&router, &state, &owner, &[&member], 74).await;

    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.roster_report",
        role_less_report_payload(&room_hex, &[&owner, &member]),
    )
    .await
    .expect_err("a governed floor does not become a policy-less one by report");
    assert_eq!(err.code, "fauna.conversations.invalid_params", "{err:?}");

    let room = state.db.get_room(&room_id).await.unwrap().unwrap();
    assert_eq!(
        room.owner_id.as_deref(),
        Some(owner.actor_id().0.as_slice()),
        "the floor keeps its owner"
    );
    assert_eq!(
        live_role(&state, &room_id, &member).await.as_deref(),
        Some("member"),
        "and every rank it held"
    );
}

#[tokio::test]
async fn a_policy_less_floor_keeps_taking_role_less_reports() {
    // The stored-room check refuses only an owner-less report against a floor
    // that NAMES an owner. A policy-less room's floor names none, so its later
    // role-less reports — and the role-less birth report that minted it — are
    // admitted exactly as before (`conversation-rooms.md` § Implementation
    // status today → *The birth report*).
    let (router, state) = router_with_db_only().await;
    let a = ActorKeypair::generate();
    let b = ActorKeypair::generate();
    let c = ActorKeypair::generate();
    let room_hex = hex::encode([75u8; 32]);
    let room_id = room_id_of(&room_hex);
    seat_on_routing_roster(&state, &a, &room_id).await;

    dispatch(
        &router,
        state.clone(),
        a.actor_id().0,
        "fauna.conversations.room.roster_report",
        role_less_report_payload(&room_hex, &[&a, &b]),
    )
    .await
    .expect("the role-less birth report");
    dispatch(
        &router,
        state.clone(),
        a.actor_id().0,
        "fauna.conversations.room.roster_report",
        role_less_report_payload(&room_hex, &[&a, &b, &c]),
    )
    .await
    .expect("a policy-less room's later role-less report");
    assert!(
        state
            .db
            .is_room_member(&room_id, &c.actor_id().0)
            .await
            .unwrap()
    );
    assert_eq!(
        state.db.get_room(&room_id).await.unwrap().unwrap().owner_id,
        None
    );
}

#[test]
fn the_membership_kinds_are_user_class_only() {
    for kind in [
        "fauna.conversations.room.invite",
        "fauna.conversations.room.accept_invite",
        "fauna.conversations.room.remove",
        "fauna.conversations.room.leave",
        "fauna.conversations.room.list_invites",
        "fauna.conversations.room.revoke_invite",
    ] {
        assert!(is_permitted(CallerClass::User, kind));
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::BridgeAtprotoPds,
            CallerClass::Custodian,
            CallerClass::ContentProcessor,
        ] {
            assert!(
                !is_permitted(class, kind),
                "{class:?} holds no membership role"
            );
        }
    }
}

#[tokio::test]
async fn an_invitation_to_a_stranger_is_refused_by_their_reach_policy() {
    // "An invite is initiation, and initiation is what the recipient's inbox
    // mode mediates; the group-Welcome gate there applies unchanged"
    // (`conversation-rooms.md` § Join rules and invites). A room invitation
    // is therefore NOT a way around the reach floor: with no contact edge
    // the stored default (`allow_knock`) refuses a first-reach invitation
    // from a stranger, and the sender's path is the contact request.
    //
    // The refusal is deliberately the generic `forbidden` the supervised
    // floor uses, so an inviter cannot distinguish `contacts_only` from
    // supervision by probing rooms.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(18)).await;
    let room_id = room_id_of(&created.room_id);

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &stranger,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect_err("a room invite does not bypass the invitee's reach policy");
    assert_eq!(err.code, "fauna.conversations.forbidden");

    // Nothing was recorded — a refused invitation leaves no pending row for
    // a later accept to find.
    assert!(
        state
            .db
            .get_pending_room_invite(&room_id, &stranger.actor_id().0)
            .await
            .expect("invite read")
            .is_none(),
        "a refused invitation is not recorded"
    );

    // Once the invitee accepts contact, the same invitation goes through —
    // the gate is the recipient's policy, not a property of rooms.
    accept_contact(&state, &stranger, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &stranger,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("an accepted contact admits the invitation");
}

// ── governance — set_policy / transfer_ownership ────────────────
//
// Rule 6, read exactly: "the room policy is signed by a principal whose role
// covers the change — the owner for the owner and the admin set, owner or
// admin for the rest — and verified by members in every class; the nest
// stores it, refuses what the signature does not cover, and **cannot author
// it**."
//
// That last clause is why storing a policy also RECONCILES the floor
// roster's roles to it: the signed policy is the authority members render,
// and `room_members.role` is the projection every nest-side gate reads. If
// the two could drift, the nest would enforce a rank the policy does not
// grant.

fn policy_at(
    owner: &ActorKeypair,
    version: u64,
    admins: &[&ActorKeypair],
    name: &str,
) -> fauna_mls::room_policy::RoomPolicy {
    let mut p = fauna_mls::room_policy::RoomPolicy::initial(owner.actor_id(), Some(name.into()));
    p.version = version;
    p.set_admins(admins.iter().map(|a| a.actor_id()));
    p
}

/// `policy` signed by `signer` as every current build signs a community
/// version — both signatures, the room signature for the room `room_hex`
/// names, without which no version above 1 lands anywhere.
fn sign_for_room(
    room_hex: &str,
    signer: &ActorKeypair,
    policy: &fauna_mls::room_policy::RoomPolicy,
) -> fauna_mls::room_policy::SignedRoomPolicy {
    policy
        .sign_community(&room_id_of(room_hex), signer)
        .expect("signs")
}

/// A `set_policy` offer of `policy`, signed for the room it is offered to.
fn set_policy_payload(
    room_hex: &str,
    signer: &ActorKeypair,
    policy: &fauna_mls::room_policy::RoomPolicy,
) -> Bytes {
    signed_set_policy_payload(room_hex, &sign_for_room(room_hex, signer, policy))
}

fn signed_set_policy_payload(
    room_hex: &str,
    signed: &fauna_mls::room_policy::SignedRoomPolicy,
) -> Bytes {
    let req = RoomSetPolicyRequest {
        room_id: room_hex.into(),
        policy: encode_canonical(signed).unwrap().to_vec(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn transfer_payload(
    room_hex: &str,
    signer: &ActorKeypair,
    policy: &fauna_mls::room_policy::RoomPolicy,
) -> Bytes {
    let signed = sign_for_room(room_hex, signer, policy);
    let req = RoomTransferOwnershipRequest {
        room_id: room_hex.into(),
        policy: encode_canonical(&signed).unwrap().to_vec(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

#[tokio::test]
async fn the_owner_appoints_an_admin_and_the_floor_roster_follows_the_policy() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(20)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &owner,
            &policy_at(&owner, 2, &[&member], "the square"),
        ),
    )
    .await
    .expect("the owner appoints an admin");
    let reply: RoomSetPolicyReply = decode(&bytes).unwrap();
    assert_eq!(reply.policy_version, 2);

    // The projection, not a second source of truth.
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &member.actor_id().0)
            .await
            .expect("floor read"),
        Some("admin".to_string()),
        "the roster's role follows the signed policy"
    );

    // …and demotion travels the same way.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &owner,
            &policy_at(&owner, 3, &[], "the square"),
        ),
    )
    .await
    .expect("the owner demotes the admin");
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &member.actor_id().0)
            .await
            .expect("floor read"),
        Some("member".to_string()),
        "and a dropped admin is demoted on the floor"
    );

    // The owner's own row is never reconciled from the admin set — it is not
    // in its own admin set by construction, so doing so would demote the
    // owner on every policy change.
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &owner.actor_id().0)
            .await
            .expect("floor read"),
        Some("owner".to_string()),
    );
}

/// The floor's half of the room binding, run by the one judge a member also
/// runs (`judge_policy_step`): a version its owner genuinely signed for
/// ANOTHER room it owns is refused, and so is one carrying no room signature
/// at all — while that same version lands in the room it was signed for, and
/// the room's own lands here. Every other check passes (the appointed admin is
/// seated in both rooms), so only the binding refuses.
#[tokio::test]
async fn a_policy_version_signed_for_another_room_is_refused_by_the_floor() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();

    let room_b = create_room(&router, &state, &owner, birth_salt(31)).await;
    let room_c = create_room(&router, &state, &owner, birth_salt(32)).await;
    for room in [&room_b, &room_c] {
        seat_member(
            &router,
            &state,
            &owner,
            &room.room_id,
            &room_id_of(&room.room_id),
            &admin,
            fauna_mls::room_policy::RoomRole::Member,
        )
        .await;
    }
    let appoint = policy_at(&owner, 2, &[&admin], "the square");
    let for_c = appoint
        .sign_community(&room_id_of(&room_c.room_id), &owner)
        .unwrap();
    let mut unbound = for_c.clone();
    unbound.room_signature = None;

    for (offer, what) in [
        (&for_c, "signed for room C"),
        (&unbound, "with no room signature"),
    ] {
        let err = dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.set_policy",
            signed_set_policy_payload(&room_b.room_id, offer),
        )
        .await
        .expect_err(what);
        assert_eq!(err.code, "fauna.conversations.invalid_params", "{what}");
    }
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id_of(&room_b.room_id), &admin.actor_id().0)
            .await
            .expect("floor read"),
        Some("member".to_string()),
        "room C's appointment ranks nobody in room B"
    );

    for (room, offer) in [
        (&room_c, for_c.clone()),
        (
            &room_b,
            appoint
                .sign_community(&room_id_of(&room_b.room_id), &owner)
                .unwrap(),
        ),
    ] {
        dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.set_policy",
            signed_set_policy_payload(&room.room_id, &offer),
        )
        .await
        .expect("a version signed for its own room lands");
    }
}

#[tokio::test]
async fn an_admin_sets_the_rooms_name_but_never_its_admin_set() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let member = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(21)).await;
    let room_id = room_id_of(&created.room_id);
    for who in [&admin, &member] {
        seat_member(
            &router,
            &state,
            &owner,
            &created.room_id,
            &room_id,
            who,
            fauna_mls::room_policy::RoomRole::Member,
        )
        .await;
    }
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &owner,
            &policy_at(&owner, 2, &[&admin], "v2"),
        ),
    )
    .await
    .expect("the owner appoints the admin");

    // "owner or admin for the rest" — the admin renames the room.
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &admin,
            &policy_at(&owner, 3, &[&admin], "renamed"),
        ),
    )
    .await
    .expect("an admin renames the room");

    // "the owner for the owner and the admin set" — the admin may not
    // appoint another one, even though it may sign a policy at all.
    let err = dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &admin,
            &policy_at(&owner, 4, &[&admin, &member], "renamed"),
        ),
    )
    .await
    .expect_err("an admin does not appoint admins");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // And a plain member signs no policy at all.
    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &member,
            &policy_at(&owner, 4, &[&admin], "mine"),
        ),
    )
    .await
    .expect_err("a plain member sets no policy");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

#[tokio::test]
async fn a_policy_version_is_a_strict_ratchet_and_ownership_is_not_settable_here() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let other = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(22)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &other,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    // Replaying the founding version, and skipping ahead, are both refused:
    // exactly `stored + 1` is what makes the sequence a chain.
    for bad in [1u64, 3, 9] {
        let err = dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.set_policy",
            set_policy_payload(&created.room_id, &owner, &policy_at(&owner, bad, &[], "x")),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, "fauna.conversations.invalid_params",
            "version {bad} is not stored+1"
        );
    }

    // Ownership is its own operation — this door refuses it by name rather
    // than half-applying it (the roster row and the home would not move).
    let mut usurp = policy_at(&owner, 2, &[], "x");
    usurp.owner = other.actor_id();
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(&created.room_id, &owner, &usurp),
    )
    .await
    .expect_err("set_policy does not move ownership");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &owner.actor_id().0)
            .await
            .expect("floor read"),
        Some("owner".to_string()),
        "the room still has its original owner"
    );
}

#[tokio::test]
async fn an_admin_set_naming_a_non_member_is_refused() {
    // An admin who is not a member is a rank nobody holds, and a policy the
    // roster cannot be projected onto is a floor whose gates disagree with
    // what members render.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(23)).await;
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &owner,
            &policy_at(&owner, 2, &[&stranger], "x"),
        ),
    )
    .await
    .expect_err("an admin set names only live members");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

#[tokio::test]
async fn an_admin_invitation_needs_the_policy_to_name_the_invitee_first() {
    // The nest cannot author a policy, so seating an admin the signed policy
    // does not name would make the nest enforce a rank members do not
    // render. Admin appointment is therefore a single owner-signed act, and
    // the invitation follows it.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let guest = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(24)).await;
    let room_id = room_id_of(&created.room_id);
    accept_contact(&state, &guest, &owner).await;

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Admin,
        ),
    )
    .await
    .expect_err("an admin rank comes from the policy, not from an invitation");
    assert_eq!(err.code, "fauna.conversations.invalid_params");

    // The honest route: seat them as a member, then appoint.
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &guest,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &owner,
            &policy_at(&owner, 2, &[&guest], "x"),
        ),
    )
    .await
    .expect("the owner appoints them");
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &guest.actor_id().0)
            .await
            .expect("floor read"),
        Some("admin".to_string()),
    );
}

// ── An invitation is judged again when it is accepted ──────────
//
// `conversation-rooms.md` § Join rules and invites → *An invitation is a
// standing offer*: accepting is the moment the inviter's authority is
// exercised, so the accept door runs the invite door's own tests again,
// against the floor and the policy as they stand NOW, and an invitation that
// no longer passes lapses — row, envelope and quota charge consumed as one
// act.

/// A router that also serves the inbox plane, with quota accounting on, so a
/// test can read what an invitation left standing for its invitee.
async fn router_with_inbox_and_quotas() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    *state.enforce_tier_quotas.write().await = true;
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    fauna_nest::inbox_handlers::register_inbox_handlers(&mut b);
    (b.build(), state)
}

async fn standing_envelopes(
    router: &RpcRouter,
    state: &Arc<AppState>,
    who: &ActorKeypair,
) -> usize {
    let reply: fauna_protocol::inbox::InboxFetchReply = decode(
        &dispatch(
            router,
            state.clone(),
            who.actor_id().0,
            "fauna.inbox.fetch",
            fetch_payload(),
        )
        .await
        .expect("the invitee reads its own inbox"),
    )
    .unwrap();
    reply.items.len()
}

#[tokio::test]
async fn an_invitation_from_an_admin_since_demoted_lapses_at_accept() {
    let (router, state) = router_with_inbox_and_quotas().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let guest = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(31)).await;
    let room_id = room_id_of(&created.room_id);
    for (who, label) in [(&admin, "admin"), (&guest, "guest")] {
        state
            .db
            .create_user(&who.actor_id().0, "free", label)
            .await
            .expect("a registered tenant, so quota accounting applies");
    }
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;

    accept_contact(&state, &guest, &admin).await;
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &admin,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("an admin invites");
    assert_eq!(standing_envelopes(&router, &state, &guest).await, 1);

    // The owner demotes the admin while the invitation is still pending.
    let version = state
        .db
        .get_room(&room_id)
        .await
        .unwrap()
        .unwrap()
        .policy_version
        .unwrap();
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &owner,
            &policy_at(&owner, version + 1, &[], "the square"),
        ),
    )
    .await
    .expect("the owner demotes the admin");

    let err = dispatch(
        &router,
        state.clone(),
        guest.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(&created.room_id),
    )
    .await
    .expect_err("a plain member could not issue this invitation today");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    assert!(
        !state
            .db
            .is_room_member(&room_id, &guest.actor_id().0)
            .await
            .unwrap(),
        "a lapsed invitation seats nobody"
    );

    // Three facts, one act: the refusal consumed all of them.
    assert!(
        state
            .db
            .get_pending_room_invite(&room_id, &guest.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "the lapsed row is gone, not left for the invitee to retry against"
    );
    assert_eq!(
        standing_envelopes(&router, &state, &guest).await,
        0,
        "and so is the envelope that offered it"
    );
    assert_eq!(
        state
            .db
            .get_user(&guest.actor_id().0)
            .await
            .unwrap()
            .unwrap()
            .inbox_bytes_used,
        0,
        "with its quota charge refunded"
    );

    // A lapse is not a ban: somebody who holds the authority today invites
    // again, and that invitation is accepted.
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &guest,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;
}

#[tokio::test]
async fn an_invitation_from_a_member_who_has_left_lapses_at_accept() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let guest = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(32)).await;
    let room_id = room_id_of(&created.room_id);
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;
    accept_contact(&state, &guest, &admin).await;
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &admin,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("an admin invites");
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&created.room_id),
    )
    .await
    .expect("the admin leaves");

    let err = dispatch(
        &router,
        state.clone(),
        guest.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(&created.room_id),
    )
    .await
    .expect_err("the policy still names the departed admin, and their seat is what counts");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    assert!(
        !state
            .db
            .is_room_member(&room_id, &guest.actor_id().0)
            .await
            .unwrap()
    );
}

/// The admin half of the invite door's judgement, re-applied: an admin
/// invitation is admissible only while the signed policy names the invitee,
/// and the policy reconcile touches live seats only — so without the second
/// judgement an invitee the owner has since dropped from the admin set is
/// seated at a rank the policy members verify does not grant.
#[tokio::test]
async fn an_admin_invitation_lapses_when_the_policy_no_longer_names_the_invitee() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(33)).await;
    let room_id = room_id_of(&created.room_id);
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&created.room_id),
    )
    .await
    .expect("the admin leaves; the signed policy still names them");

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &admin,
            fauna_mls::room_policy::RoomRole::Admin,
        ),
    )
    .await
    .expect("the policy names them, so the owner may invite them back as an admin");

    let version = state
        .db
        .get_room(&room_id)
        .await
        .unwrap()
        .unwrap()
        .policy_version
        .unwrap();
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &owner,
            &policy_at(&owner, version + 1, &[], "the square"),
        ),
    )
    .await
    .expect("the owner re-signs the policy without them");

    let err = dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(&created.room_id),
    )
    .await
    .expect_err("an admin rank comes from the policy as it stands at accept");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &admin.actor_id().0)
            .await
            .unwrap(),
        None,
        "nobody is seated at a rank the signed policy does not grant"
    );
}

/// The seed-thief case. Whoever held the owner's seed issued this invitation
/// as the owner; the owner has since recovered onto a new identity. The
/// inviter is judged by its own SEAT — never through its succession line,
/// which would land on the recovered owner's seat and pass — and the ceremony
/// absorbed that seat, so the invitation lapses.
#[tokio::test]
async fn an_invitation_from_an_identity_since_succeeded_lapses_at_accept() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let recovered = ActorKeypair::generate();
    let accomplice = ActorKeypair::generate();

    state
        .db
        .create_user(&owner.actor_id().0, "free", "owner")
        .await
        .expect("an account the ceremony can carry");
    let created = create_room(&router, &state, &owner, birth_salt(34)).await;
    let room_id = room_id_of(&created.room_id);
    accept_contact(&state, &accomplice, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &accomplice,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("issued under the old identity, by whoever held its seed");

    state
        .db
        .record_succession(&owner.actor_id().0, &recovered.actor_id().0, b"s", 1)
        .await
        .expect("ceremony")
        .expect("applied");
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &recovered.actor_id().0)
            .await
            .unwrap(),
        Some("owner".to_string()),
        "the recovered identity owns the room"
    );

    let err = dispatch(
        &router,
        state.clone(),
        accomplice.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(&created.room_id),
    )
    .await
    .expect_err("the identity that issued it holds no seat any more");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    assert!(
        !state
            .db
            .is_room_member(&room_id, &accomplice.actor_id().0)
            .await
            .unwrap()
    );

    // The recovered owner's own invitations are ordinary ones.
    seat_member(
        &router,
        &state,
        &recovered,
        &created.room_id,
        &room_id,
        &accomplice,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;
}

// ── Pending invitations: seen by whoever may withdraw them ─────
//
// `conversation-rooms.md` § Join rules and invites → *Pending invitations are
// visible to whoever may withdraw them, and withdrawable*. One predicate, two
// doors: the owner and admins are served (and may withdraw) every pending
// invitation, any other seated member the ones it issued, and a caller off
// the floor is refused.

async fn listed_invitees(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: &ActorKeypair,
    room_hex: &str,
) -> Vec<RoomPendingInviteWire> {
    let reply: RoomListInvitesReply = decode(
        &dispatch(
            router,
            state.clone(),
            caller.actor_id().0,
            "fauna.conversations.room.list_invites",
            list_invites_payload(room_hex),
        )
        .await
        .expect("a seated member lists"),
    )
    .unwrap();
    reply.invites
}

#[tokio::test]
async fn the_owner_and_admins_list_every_pending_invitation_and_a_member_only_its_own() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (by_owner, by_member, stranger) = (
        ActorKeypair::generate(),
        ActorKeypair::generate(),
        ActorKeypair::generate(),
    );

    let created = create_room(&router, &state, &owner, birth_salt(35)).await;
    let room_id = room_id_of(&created.room_id);
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    // `member-invite`, so a plain member holds an invitation of its own.
    let set_join_rule = |version: u64, rule: fauna_mls::room_policy::JoinRule| {
        let mut policy = policy_at(&owner, version, &[&admin], "the square");
        policy.join_rule = rule;
        set_policy_payload(&created.room_id, &owner, &policy)
    };
    let version = state
        .db
        .get_room(&room_id)
        .await
        .unwrap()
        .unwrap()
        .policy_version
        .unwrap();
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_join_rule(version + 1, fauna_mls::room_policy::JoinRule::MemberInvite),
    )
    .await
    .expect("any member may invite now");

    for (inviter, invitee) in [(&owner, &by_owner), (&member, &by_member)] {
        accept_contact(&state, invitee, inviter).await;
        dispatch(
            &router,
            state.clone(),
            inviter.actor_id().0,
            "fauna.conversations.room.invite",
            invite_payload(
                inviter,
                &room_id,
                invitee,
                fauna_mls::room_policy::RoomRole::Member,
            ),
        )
        .await
        .expect("invited");
    }
    let hex_of = |who: &ActorKeypair| hex::encode(who.actor_id().0);

    for seer in [&owner, &admin] {
        let listed = listed_invitees(&router, &state, seer, &created.room_id).await;
        assert_eq!(
            listed.iter().map(|i| i.invitee.clone()).collect::<Vec<_>>(),
            vec![hex_of(&by_owner), hex_of(&by_member)],
            "the owner and an admin see every pending invitation, oldest first"
        );
        assert!(
            listed
                .iter()
                .all(|i| i.still_acceptable && i.role == "member")
        );
        assert_eq!(listed[1].inviter, hex_of(&member), "and who issued each");
    }
    let own = listed_invitees(&router, &state, &member, &created.room_id).await;
    assert_eq!(
        own.iter().map(|i| i.invitee.clone()).collect::<Vec<_>>(),
        vec![hex_of(&by_member)],
        "a plain member sees the invitation it issued and no other"
    );

    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.list_invites",
        list_invites_payload(&created.room_id),
    )
    .await
    .expect_err("who a room has invited is not public");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // The room goes back to `invite`: the member's invitation now lapses in
    // waiting, and the list says so with the accept door's own judgement.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_join_rule(version + 2, fauna_mls::room_policy::JoinRule::Invite),
    )
    .await
    .expect("invitations are the owner's and admins' again");
    let listed = listed_invitees(&router, &state, &owner, &created.room_id).await;
    assert_eq!(
        listed
            .iter()
            .map(|i| (i.invitee.clone(), i.still_acceptable))
            .collect::<Vec<_>>(),
        vec![(hex_of(&by_owner), true), (hex_of(&by_member), false)],
    );

    // An accepted invitation is history on the roster, never listed.
    dispatch(
        &router,
        state.clone(),
        by_owner.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(&created.room_id),
    )
    .await
    .expect("accepted");
    let listed = listed_invitees(&router, &state, &owner, &created.room_id).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].invitee, hex_of(&by_member));
}

#[tokio::test]
async fn a_withdrawn_invitation_leaves_with_its_envelope_and_cannot_be_accepted() {
    let (router, state) = router_with_inbox_and_quotas().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let guest = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(36)).await;
    let room_id = room_id_of(&created.room_id);
    for (who, label) in [(&admin, "admin"), (&member, "member"), (&guest, "guest")] {
        state
            .db
            .create_user(&who.actor_id().0, "free", label)
            .await
            .expect("a registered tenant, so quota accounting applies");
    }
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;
    accept_contact(&state, &guest, &admin).await;
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &admin,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("an admin invites");

    let revoke = |caller: &ActorKeypair| {
        dispatch(
            &router,
            state.clone(),
            caller.actor_id().0,
            "fauna.conversations.room.revoke_invite",
            revoke_invite_payload(&created.room_id, &guest),
        )
    };

    let err = revoke(&stranger)
        .await
        .expect_err("a caller off the floor withdraws nothing");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // A plain member is served only its own invitations, so somebody else's
    // is — to it — not there: answered, not refused, and nothing consumed.
    let reply: RoomRevokeInviteReply = decode(&revoke(&member).await.unwrap()).unwrap();
    assert!(!reply.revoked);
    assert_eq!(standing_envelopes(&router, &state, &guest).await, 1);

    let reply: RoomRevokeInviteReply = decode(&revoke(&owner).await.unwrap()).unwrap();
    assert!(reply.revoked, "the owner withdraws an admin's invitation");

    // Three facts, one act.
    assert!(
        state
            .db
            .get_pending_room_invite(&room_id, &guest.actor_id().0)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(standing_envelopes(&router, &state, &guest).await, 0);
    assert_eq!(
        state
            .db
            .get_user(&guest.actor_id().0)
            .await
            .unwrap()
            .unwrap()
            .inbox_bytes_used,
        0
    );

    let err = dispatch(
        &router,
        state.clone(),
        guest.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload(&created.room_id),
    )
    .await
    .expect_err("a withdrawn invitation is not pending");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // Gone is what the caller wanted: a repeat is answered, not refused.
    let reply: RoomRevokeInviteReply = decode(&revoke(&owner).await.unwrap()).unwrap();
    assert!(!reply.revoked);

    // And a withdrawal is not a ban.
    seat_member(
        &router,
        &state,
        &admin,
        &created.room_id,
        &room_id,
        &guest,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;
}

#[tokio::test]
async fn a_member_withdraws_the_invitation_it_issued() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let guest = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(37)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;
    let mut policy = policy_at(&owner, 2, &[], "the square");
    policy.join_rule = fauna_mls::room_policy::JoinRule::MemberInvite;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(&created.room_id, &owner, &policy),
    )
    .await
    .expect("any member may invite");
    accept_contact(&state, &guest, &member).await;
    dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &member,
            &room_id,
            &guest,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("a member invites");

    let reply: RoomRevokeInviteReply = decode(
        &dispatch(
            &router,
            state.clone(),
            member.actor_id().0,
            "fauna.conversations.room.revoke_invite",
            revoke_invite_payload(&created.room_id, &guest),
        )
        .await
        .expect("its own invitation"),
    )
    .unwrap();
    assert!(reply.revoked);
    assert!(
        state
            .db
            .get_pending_room_invite(&room_id, &guest.actor_id().0)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn ownership_transfers_to_a_member_and_the_outgoing_owner_becomes_one() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let heir = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(25)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &heir,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    // Signed by the OUTGOING owner: its role in the previous version is what
    // decides whether it may have changed the owner field.
    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(
            &created.room_id,
            &owner,
            &policy_at(&heir, 2, &[], "the square"),
        ),
    )
    .await
    .expect("the owner transfers ownership");
    let reply: RoomTransferOwnershipReply = decode(&bytes).unwrap();
    assert_eq!(reply.owner, hex::encode(heir.actor_id().0));
    assert_eq!(reply.policy_version, 2);

    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &heir.actor_id().0)
            .await
            .expect("floor read"),
        Some("owner".to_string()),
    );
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &owner.actor_id().0)
            .await
            .expect("floor read"),
        Some("member".to_string()),
        "the outgoing owner keeps their membership at the rank the new policy gives them"
    );
    let room = state.db.get_room(&room_id).await.unwrap().unwrap();
    assert_eq!(room.owner_id.as_deref(), Some(&heir.actor_id().0[..]));

    // The former owner can no longer govern, and — the attack the ceremony
    // must not leave open — cannot take the room back by replaying its own
    // birth record, because the stored owner no longer matches the caller.
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(&created.room_id, &owner, &policy_at(&owner, 3, &[], "back")),
    )
    .await
    .expect_err("a former owner transfers nothing");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.create",
        birth_payload(&owner, birth_salt(25), Some("the square")),
    )
    .await
    .expect_err("a former owner does not re-found the room it gave away");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    let room = state.db.get_room(&room_id).await.unwrap().unwrap();
    assert_eq!(
        room.owner_id.as_deref(),
        Some(&heir.actor_id().0[..]),
        "the heir still owns it"
    );
}

///  — `transfer_ownership` reconciles every live user member's
/// floor role to the new policy's admin set, not just the outgoing owner's:
/// a third party the new policy drops from the admin set is demoted on the
/// floor and loses the admin-gated `room.remove` door (arm 1: the
/// projection; arm 2: the effect), and one it newly names is promoted.
#[tokio::test]
async fn a_transfer_reconciles_every_members_rank_to_the_new_admin_set() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let heir = ActorKeypair::generate();
    let demoted = ActorKeypair::generate();
    let promoted = ActorKeypair::generate();
    let victim = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(90)).await;
    let room_id = room_id_of(&created.room_id);
    for who in [&heir, &demoted, &promoted, &victim] {
        seat_member(
            &router,
            &state,
            &owner,
            &created.room_id,
            &room_id,
            who,
            fauna_mls::room_policy::RoomRole::Member,
        )
        .await;
    }

    // `demoted` starts out an admin, appointed the ordinary way.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &owner,
            &policy_at(&owner, 2, &[&demoted], "the square"),
        ),
    )
    .await
    .expect("the owner appoints demoted");
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &demoted.actor_id().0)
            .await
            .expect("floor read"),
        Some("admin".to_string()),
    );

    // The owner transfers to `heir` with a new policy that drops `demoted`
    // from the admin set and names `promoted` instead.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(
            &created.room_id,
            &owner,
            &policy_at(&heir, 3, &[&promoted], "the square"),
        ),
    )
    .await
    .expect("the owner transfers ownership");

    // Arm 1 — the projection: the dropped admin reads `member`, the newly
    // named one reads `admin`, exactly as a `set_policy` reconcile would.
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &demoted.actor_id().0)
            .await
            .expect("floor read"),
        Some("member".to_string()),
        "a transfer's new admin set demotes a dropped admin just like set_policy does"
    );
    assert_eq!(
        state
            .db
            .get_room_member_role(&room_id, &promoted.actor_id().0)
            .await
            .expect("floor read"),
        Some("admin".to_string()),
        "a transfer's new admin set promotes a newly named member too"
    );

    // Arm 2 — the effect: the demoted admin no longer holds the admin-gated
    // door the new policy revoked.
    let err = dispatch(
        &router,
        state.clone(),
        demoted.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&created.room_id, &victim),
    )
    .await
    .expect_err("a transfer-demoted admin is refused the remove door");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

#[tokio::test]
async fn a_transfer_needs_a_live_user_member_and_only_the_owner_makes_it() {
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(26)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    // An owner-less room is unrepresentable, so the heir must already be
    // there.
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(&created.room_id, &owner, &policy_at(&stranger, 2, &[], "x")),
    )
    .await
    .expect_err("the incoming owner must be a live member");
    assert_eq!(err.code, "fauna.conversations.invalid_params");

    // A plain member does not transfer what it does not own.
    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(&created.room_id, &member, &policy_at(&member, 2, &[], "x")),
    )
    .await
    .expect_err("only the owner transfers ownership");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

#[tokio::test]
async fn a_room_is_never_owned_by_its_home_nest() {
    // "Don't put the home nest in a community room's key authority set. It
    // is a reader recipient; owner and admin devices mint and rotate."
    // Ownership is the authority set's root, so it never lands there.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(27)).await;
    let nest_principal = state.nest_identity.public_key_bytes();

    // The nest IS a live member of the room, so only the principal-kind
    // check stands between it and ownership.
    let mut to_nest = fauna_mls::room_policy::RoomPolicy::initial(
        fauna_core::identity::ActorId(nest_principal),
        Some("nest-owned".into()),
    );
    to_nest.version = 2;
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(&created.room_id, &owner, &to_nest),
    )
    .await
    .expect_err("a room is owned by a user principal, never by the nest that reads it");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

#[test]
fn the_governance_kinds_are_user_class_only() {
    for kind in [
        "fauna.conversations.room.set_policy",
        "fauna.conversations.room.transfer_ownership",
    ] {
        assert!(is_permitted(CallerClass::User, kind));
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::BridgeAtprotoPds,
            CallerClass::Custodian,
            CallerClass::ContentProcessor,
        ] {
            assert!(!is_permitted(class, kind), "{class:?} governs no room");
        }
    }
}

// ── the community class's floor gate on the send path ───────────

fn channel_send_payload(room_hex: &str, seed: u8) -> Bytes {
    // A well-formed, AEAD-SHAPED `ChannelEnvelope`: the ingest strict-decodes
    // the envelope and refuses a body that is not plausibly sealed (< 28
    // bytes, or carrying a plaintext magic prefix), so raw bytes never reach
    // the floor gate under test.
    let mut inner = vec![seed; 64];
    inner[0] = seed.wrapping_add(0x10);
    let envelope = fauna_mls::types::ChannelEnvelope::Application(inner);
    let req = fauna_protocol::conversations::ChannelSendRequest {
        channel_id: room_hex.into(),
        envelope: envelope.to_bytes().expect("envelope encodes"),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

#[tokio::test]
async fn a_community_rooms_send_is_verified_against_its_floor() {
    // "The home nest refuses a send ... whose signer's role does not permit
    // it, before storing anything" (`conversation-rooms.md` § The floor
    // roster → *Community rooms*).
    //
    // The routing roster self-registers on first send, so without the floor
    // gate any actor that learned the 32-byte channel id could append to a
    // community room's log — and unlike the end-to-end case those are bytes
    // the home nest holds a wrap for and will fan out and index.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let outsider = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(30)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    // Membership, not rank: a plain member sends.
    dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.channel.send",
        channel_send_payload(&created.room_id, 1),
    )
    .await
    .expect("a live member of any rank may send");

    // An outsider that knows the id is refused BEFORE anything is stored —
    // and is refused even though the routing roster would have taken its
    // self-registration.
    let err = dispatch(
        &router,
        state.clone(),
        outsider.actor_id().0,
        "fauna.conversations.channel.send",
        channel_send_payload(&created.room_id, 2),
    )
    .await
    .expect_err("a community room's floor is what decides who may send");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // …and a departed member loses it with their membership, which is the
    // whole point of the record being non-self-assertable.
    dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.leave",
        leave_payload(&created.room_id),
    )
    .await
    .expect("the member leaves");
    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.channel.send",
        channel_send_payload(&created.room_id, 3),
    )
    .await
    .expect_err("a departed member does not keep sending");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

#[tokio::test]
async fn an_end_to_end_rooms_send_is_not_floor_gated() {
    // The other half of the same rule: an end-to-end room's membership is
    // its MLS group's, which this nest cannot read, so its send stays gated
    // on the routing roster exactly as before. A stranger's envelope there
    // is spam that decrypts for nobody, not forgery — and floor-gating it
    // would break every real send, because the floor is a mirror that only
    // exists once a member has reported.
    let (router, state) = router_with_db_only().await;
    let sender = ActorKeypair::generate();
    let room_hex = hex::encode([31u8; 32]);

    dispatch(
        &router,
        state.clone(),
        sender.actor_id().0,
        "fauna.conversations.channel.send",
        channel_send_payload(&room_hex, 4),
    )
    .await
    .expect("a channel with no floor roster sends as it always did");

    // Even once a floor roster exists by REPORT, the gate stays off: the
    // report is a mirror, and a mirror is not an authority over sends.
    let room_id = room_id_of(&room_hex);
    let other = ActorKeypair::generate();
    seat_on_routing_roster(&state, &other, &room_id).await;
    dispatch(
        &router,
        state.clone(),
        other.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&other, "owner")]),
    )
    .await
    .expect("the mirror door records the roster");

    dispatch(
        &router,
        state.clone(),
        sender.actor_id().0,
        "fauna.conversations.channel.send",
        channel_send_payload(&room_hex, 5),
    )
    .await
    .expect("a reported roster does not become a send authority");
}

// ── the floor delete record ─────────────────────────────────────
//
// Authority: `docs/goal/behavior/conversation-rooms.md` § Roles and
// authorization → *Delete any message — the mechanism* → *Community rooms*.
// An owner's or admin's delete of another member's message is a signed,
// UNSEALED record the floor judges from the record alone — signature, author
// is the caller, named version is the held version, the author's seat is owner
// or admin — before storing anything, and never by opening a send.

/// A `channel.send` carrying `author`'s floor delete record for `target_seq`,
/// made under `policy_version`.
fn floor_delete_payload(
    room_hex: &str,
    author: &ActorKeypair,
    target_seq: u64,
    policy_version: u64,
) -> Bytes {
    let signed = fauna_mls::room_policy::RoomFloorDelete {
        room: room_id_of(room_hex).to_vec(),
        target_seq,
        author: author.actor_id(),
        policy_version,
    }
    .sign(author)
    .expect("the author signs its own record");
    floor_delete_send(room_hex, &signed)
}

/// A `channel.send` onto `room_hex` carrying an **already-signed** record —
/// the door [`floor_delete_payload`] takes, split out so a test can file a
/// record the honest client would never mint: one whose signature has been
/// tampered with, or one signed for a different room than the channel it is
/// filed onto.
fn floor_delete_send(
    room_hex: &str,
    signed: &fauna_mls::room_policy::SignedRoomFloorDelete,
) -> Bytes {
    let envelope = fauna_mls::types::ChannelEnvelope::RoomFloorDelete(
        signed.to_bytes().expect("record encodes"),
    );
    let req = fauna_protocol::conversations::ChannelSendRequest {
        channel_id: room_hex.into(),
        envelope: envelope.to_bytes().expect("envelope encodes"),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

async fn send_as(
    router: &RpcRouter,
    state: &Arc<AppState>,
    who: &ActorKeypair,
    payload: Bytes,
) -> Result<Bytes, fauna_protocol::RpcError> {
    dispatch(
        router,
        state.clone(),
        who.actor_id().0,
        "fauna.conversations.channel.send",
        payload,
    )
    .await
}

/// A room at policy version 2: `owner`, `admin` (named by version 2), and a
/// plain `member`, who has posted the message at seq 1 the records target.
async fn room_with_an_admin_and_a_member(
    router: &RpcRouter,
    state: &Arc<AppState>,
    salt: u8,
) -> (String, ActorKeypair, ActorKeypair, ActorKeypair) {
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let created = create_room(router, state, &owner, birth_salt(salt)).await;
    let room_id = room_id_of(&created.room_id);
    seat_admin(router, state, &owner, &created.room_id, &room_id, &admin).await;
    seat_member(
        router,
        state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;
    send_as(
        router,
        state,
        &member,
        channel_send_payload(&created.room_id, 9),
    )
    .await
    .expect("the member posts the message the records target");
    (created.room_id, owner, admin, member)
}

async fn held_version(state: &Arc<AppState>, room_hex: &str) -> u64 {
    state
        .db
        .get_room(&room_id_of(room_hex))
        .await
        .expect("room read")
        .expect("the room exists")
        .policy_version
        .expect("a ceremony-born room carries a version")
}

#[tokio::test]
async fn an_owners_and_an_admins_floor_delete_record_is_admitted_and_a_members_is_not() {
    let (router, state) = router_with_db_only().await;
    let (room, owner, admin, member) = room_with_an_admin_and_a_member(&router, &state, 40).await;
    let v = held_version(&state, &room).await;

    send_as(
        &router,
        &state,
        &owner,
        floor_delete_payload(&room, &owner, 1, v),
    )
    .await
    .expect("the owner's record is admitted");
    send_as(
        &router,
        &state,
        &admin,
        floor_delete_payload(&room, &admin, 1, v),
    )
    .await
    .expect("an admin's record is admitted");

    let err = send_as(
        &router,
        &state,
        &member,
        floor_delete_payload(&room, &member, 1, v),
    )
    .await
    .expect_err("a plain member cannot mint a floor delete record");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // Signed by an admin, filed by a member: the author is not the caller.
    let err = send_as(
        &router,
        &state,
        &member,
        floor_delete_payload(&room, &admin, 1, v),
    )
    .await
    .expect_err("nobody files a record under another principal's name");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    // …and the reverse, which only the author-is-the-caller check stops (the
    // caller's own rank would pass): the OWNER filing a record a plain member
    // signed. The log must never carry a record whose named author the floor
    // did not authenticate.
    let err = send_as(
        &router,
        &state,
        &owner,
        floor_delete_payload(&room, &member, 1, v),
    )
    .await
    .expect_err("an owner cannot file a record another principal authored");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // A non-member is refused as every send of theirs is — at membership.
    let outsider = ActorKeypair::generate();
    let err = send_as(
        &router,
        &state,
        &outsider,
        floor_delete_payload(&room, &outsider, 1, v),
    )
    .await
    .expect_err("an outsider's record is refused as today");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

#[tokio::test]
async fn a_floor_delete_record_naming_a_version_the_floor_does_not_hold_is_refused() {
    let (router, state) = router_with_db_only().await;
    let (room, owner, admin, _member) = room_with_an_admin_and_a_member(&router, &state, 41).await;
    let v = held_version(&state, &room).await;

    // The owner demotes the admin: version v + 1 names nobody.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(&room, &owner, &policy_at(&owner, v + 1, &[], "the square")),
    )
    .await
    .expect("the owner demotes the admin");

    // A record under the superseded version — the one that still named them.
    let err = send_as(
        &router,
        &state,
        &admin,
        floor_delete_payload(&room, &admin, 1, v),
    )
    .await
    .expect_err("a stale version is refused");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    // A demoted admin's NEW record, under the held version, is refused on rank.
    let err = send_as(
        &router,
        &state,
        &admin,
        floor_delete_payload(&room, &admin, 1, v + 1),
    )
    .await
    .expect_err("a demoted admin no longer deletes");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    // A version the room has not reached is no better than a stale one.
    let err = send_as(
        &router,
        &state,
        &owner,
        floor_delete_payload(&room, &owner, 1, v + 2),
    )
    .await
    .expect_err("a future version is refused");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    // The owner, under the held version, still deletes.
    send_as(
        &router,
        &state,
        &owner,
        floor_delete_payload(&room, &owner, 1, v + 1),
    )
    .await
    .expect("the owner's record under the held version is admitted");
}

#[tokio::test]
async fn a_floor_delete_record_naming_a_version_an_ownership_transfer_superseded_is_refused() {
    let (router, state) = router_with_db_only().await;
    let (room, owner, admin, _member) = room_with_an_admin_and_a_member(&router, &state, 42).await;
    let v = held_version(&state, &room).await;

    // Ownership moves to the admin; the new policy names no admin, so the
    // outgoing owner lands on `member`.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(&room, &owner, &policy_at(&admin, v + 1, &[], "the square")),
    )
    .await
    .expect("the owner hands the room over");

    let err = send_as(
        &router,
        &state,
        &owner,
        floor_delete_payload(&room, &owner, 1, v),
    )
    .await
    .expect_err("the version the transfer superseded is refused");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    let err = send_as(
        &router,
        &state,
        &owner,
        floor_delete_payload(&room, &owner, 1, v + 1),
    )
    .await
    .expect_err("the outgoing owner holds no rank under the new version");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    send_as(
        &router,
        &state,
        &admin,
        floor_delete_payload(&room, &admin, 1, v + 1),
    )
    .await
    .expect("the new owner's record is admitted");

    // Every version the room has held — the founding one, the one that named
    // the admin, the transfer-minted one — is retained and served by number.
    let room_id = room_id_of(&room);
    for version in 1..=v + 1 {
        let blob = state
            .db
            .get_room_policy_version(&room_id, version)
            .await
            .expect("history read")
            .unwrap_or_else(|| panic!("version {version} is retained"));
        let signed: fauna_mls::room_policy::SignedRoomPolicy =
            fauna_protocol::decode_strict(&blob).expect("stored as signed");
        assert_eq!(
            signed.policy.version, version,
            "served by the number it carries"
        );
    }
    assert!(
        state
            .db
            .get_room_policy_version(&room_id, v + 2)
            .await
            .expect("history read")
            .is_none()
    );
}

/// ⚠ **The floor's FIRST check — `signed.verify(room_id)` — had no witness.**
/// Every other floor-delete case files a validly signed record for the room
/// under test, so the check reddened nothing when removed: replacing it with
/// `let _ = room_id;` left all five floor/roster pins green. A pinned
/// predicate does not pin its use (`review-method.md`, ⭐ *Pinning a pure
/// predicate does not pin its use*) — `room_policy.rs` tests `verify` itself,
/// which is a different claim from "the floor calls it".
///
/// The check does two things, and this case tampers with each separately:
///
/// - **the signature verifies** — a record whose bytes the named author did
///   not sign;
/// - **for THIS room** — a record its author genuinely signed, for a room it
///   also owns, filed onto another one. The author is the caller and holds
///   rank on both floors, so every remaining check passes: only the room bind
///   stops it. Without it, an owner of any room could file tombstones onto a
///   room where they are merely a member.
///
/// Members verify independently before painting
/// (`conversation-rooms.md` § Roles and authorization → *Members verify what
/// they paint*), so what this check protects is the log: the floor must never
/// store a record no member will honour, nor serve one as though it had been
/// judged.
#[tokio::test]
async fn a_floor_delete_record_with_a_bad_signature_or_another_rooms_bind_is_refused() {
    let (router, state) = router_with_db_only().await;
    let (room, owner, _admin, _member) = room_with_an_admin_and_a_member(&router, &state, 44).await;
    let v = held_version(&state, &room).await;

    // The fixture is sound: the owner's honest record IS admitted here, so a
    // refusal below is the tampering and not the setup.
    send_as(
        &router,
        &state,
        &owner,
        floor_delete_payload(&room, &owner, 1, v),
    )
    .await
    .expect("the owner's honest record is admitted");

    // The seq probe is an ORDINARY send, deliberately, and not a second copy of
    // that record: the floor allocates a seq inside the same locked append that
    // judges, but a repeat floor delete record for a target already tombstoned
    // converges on its original position rather than taking a new one — so a
    // repeat would read as "consumed nothing" even if a refused record HAD
    // been stored. An ordinary send always appends, which is what makes two
    // consecutive seqs across the refusals mean what it says.
    let seq_of = |reply: Bytes| {
        fauna_protocol::decode_strict::<fauna_protocol::conversations::ChannelSendReply>(&reply)
            .expect("the reply decodes")
            .seq
    };
    let before = seq_of(
        send_as(&router, &state, &owner, channel_send_payload(&room, 21))
            .await
            .expect("the owner posts an ordinary record"),
    );

    // (a) The owner's own record, one signature byte flipped.
    let mut signed = fauna_mls::room_policy::RoomFloorDelete {
        room: room_id_of(&room).to_vec(),
        target_seq: 1,
        author: owner.actor_id(),
        policy_version: v,
    }
    .sign(&owner)
    .expect("the author signs its own record");
    signed.signature[0] ^= 0x01;
    let err = send_as(&router, &state, &owner, floor_delete_send(&room, &signed))
        .await
        .expect_err("a record whose signature does not verify is refused");
    assert_eq!(
        err.code, "fauna.conversations.invalid_params",
        "the refusal is the conversations family's invalid_params: {err:?}"
    );

    // (b) A record the owner genuinely signed — for a DIFFERENT room it also
    // owns — filed onto this one. Author is the caller, rank holds, version
    // matches: the room bind is the only thing left to refuse it.
    let (other_room, _, _, _) = room_with_an_admin_and_a_member(&router, &state, 45).await;
    let for_other = fauna_mls::room_policy::RoomFloorDelete {
        room: room_id_of(&other_room).to_vec(),
        target_seq: 1,
        author: owner.actor_id(),
        policy_version: v,
    }
    .sign(&owner)
    .expect("the author signs its own record");
    let err = send_as(
        &router,
        &state,
        &owner,
        floor_delete_send(&room, &for_other),
    )
    .await
    .expect_err("a record signed for another room does not land on this one");
    assert_eq!(
        err.code, "fauna.conversations.invalid_params",
        "the refusal is the conversations family's invalid_params: {err:?}"
    );

    // Nothing was stored by either refusal.
    let after = seq_of(
        send_as(&router, &state, &owner, channel_send_payload(&room, 22))
            .await
            .expect("the owner still posts"),
    );
    assert_eq!(
        after,
        before + 1,
        "the two refused records consumed no position in the log"
    );
}

#[tokio::test]
async fn a_floor_delete_record_is_refused_on_a_room_whose_authority_is_not_its_floor() {
    let (router, state) = router_with_db_only().await;
    let sender = ActorKeypair::generate();
    let room_hex = hex::encode([43u8; 32]);
    send_as(&router, &state, &sender, channel_send_payload(&room_hex, 4))
        .await
        .expect("an end-to-end channel sends as it always did");
    let err = send_as(
        &router,
        &state,
        &sender,
        floor_delete_payload(&room_hex, &sender, 1, 1),
    )
    .await
    .expect_err("the floor act exists only where the floor is the authority");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

// ── the sealing plane ───────────────────────────────────────────
//
// Authority: `docs/goal/behavior/conversation-rooms.md` § The three classes →
// *Community* (the key model), and
// `docs/goal/architecture/account-data-taxonomy.md` § The recipient-set scheme
// for the mechanics this plane reuses rather than reinvents. What is pinned
// here is the class's whole reason to exist: the room's log rests sealed under
// a generation key its members mint, the home nest is one wrap recipient, it
// opens what it is wrapped to in order to build the derived views, and
// rotating it out both stops that and deletes what it built.

use fauna_core::group_scope::RosterMember;
use fauna_mls::wrapped_blob::group_generation_wraps;

/// A nest that has booted with a deployment signing key: the key rests in
/// `nest_keypair` as a real boot leaves it, and the room-read keypair has been
/// minted from it by the same boot step `start_server` runs. That is what a
/// nest that *can* be a community room's reader looks like. `for_test` leaves
/// the key `None`, which is the honest "this nest cannot read a room" state
/// the rest of the file runs under.
async fn booted_reader_nest(db: Arc<CacheDb>) -> AppState {
    let seed = [0x5au8; 32];
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
    db.set_nest_keypair(&seed, &signing_key.verifying_key().to_bytes())
        .await
        .unwrap();
    fauna_nest::nest_kek::mint_at_boot(&db)
        .await
        .unwrap()
        .expect("a nest holding a deployment keypair runs the boot mint")
        .room_read_public_key
        .expect("and seats its room-read key at boot");
    let mut state = AppState::for_test(db);
    state.nest_signing_key = Some(signing_key);
    state
}

async fn router_with_reader_nest() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(booted_reader_nest(db).await);
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    (b.build(), state)
}

/// A member's group-reception keypair — the wrap target it hands the room at
/// the act that seats it.
fn reception() -> fauna_core::group_generation::GroupReceptionKeyRecord {
    fauna_core::group_generation::GroupReceptionKeyRecord::mint(1_700_000_000_000)
}

/// The room's live floor as the recipient-set scheme's wrap-target set —
/// exactly what an honest minter reads back before building a mint.
async fn wrap_targets(state: &Arc<AppState>, room_id: &[u8; 32]) -> Vec<RosterMember> {
    state
        .db
        .list_floor_roster(room_id)
        .await
        .expect("floor roster")
        .into_iter()
        .filter_map(|m| {
            let entry = m.entry_id?;
            let recv = m.reception_pubkey?;
            Some(RosterMember {
                entry_id: entry.try_into().ok()?,
                member_actor: ActorId(m.principal_id),
                reception_pubkey: recv,
                enrolled_at_ms: m.joined_at,
            })
        })
        .collect()
}

fn publish_payload(room_hex: &str, mint: &[u8]) -> Bytes {
    let req = RoomPublishGenerationRequest {
        room_id: room_hex.into(),
        mint: mint.to_vec(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// A distinct stamp per mint, ascending — realistic, and no longer
/// load-bearing.
///
/// A room's generations are ordered by their **parent chain**, never by
/// `minted_at_ms` (`db::rooms::order_generations_by_chain`), so the stamp
/// decides nothing in the nest and a constant would order these tests exactly
/// as this does. It stays ascending because that is what real clocks produce;
/// the tests that need a mint stamped *against* its chain — the shape two
/// admins with skewed clocks make — build it explicitly with
/// [`build_mint_with`] (`a_revoke_stamped_before_its_parent_still_ends_the_nests_read`).
///
/// History worth keeping: while the nest ordered by `(minted_at_ms,
/// generation_id)`, a constant stamp here made every twice-rotated room's tip a
/// coin-flip on the content-derived id, and this helper's ascending stamps were
/// what made the suite deterministic — which also hid the production defect,
/// since stamps that always agree with the chain can never expose an order
/// that trusts them.
fn next_mint_stamp() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 =
        std::sync::atomic::AtomicI64::new(1_700_000_000_000);
    NEXT.fetch_add(1_000, std::sync::atomic::Ordering::Relaxed)
}

/// Build one mint over `targets` naming exactly `parents`, stamped
/// `minted_at_ms` — the knobs the ordinary [`build_mint`] fixes, for a test
/// that needs a mint the helper would never make: one stamped BELOW its
/// parent, or one naming a parent that is no longer the tip.
fn build_mint_with(
    targets: &[RosterMember],
    parents: Vec<[u8; 32]>,
    minter: &ActorKeypair,
    minted_at_ms: i64,
) -> (Vec<u8>, fauna_core::crypto::GenerationKey, [u8; 32]) {
    let built = group_generation_wraps::build_group_mint(
        targets,
        parents,
        Vec::new(),
        minter.signing_key(),
        Vec::new(),
        minted_at_ms,
    )
    .expect("the mint assembles");
    (
        encode_canonical(&built.record).unwrap().to_vec(),
        built.gen_key,
        built.generation_id,
    )
}

/// Build one mint over `targets`, parented on the room's current tip.
async fn build_mint(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    minter: &ActorKeypair,
    targets: &[RosterMember],
) -> (Vec<u8>, fauna_core::crypto::GenerationKey, [u8; 32]) {
    let parents = state
        .db
        .room_generation_tip(room_id)
        .await
        .expect("tip read")
        .map(|t| vec![t.generation_id])
        .unwrap_or_default();
    let built = group_generation_wraps::build_group_mint(
        targets,
        parents,
        Vec::new(),
        minter.signing_key(),
        Vec::new(),
        next_mint_stamp(),
    )
    .expect("the mint assembles");
    (
        encode_canonical(&built.record).unwrap().to_vec(),
        built.gen_key,
        built.generation_id,
    )
}

/// Mint over the room's whole live floor and publish it — the ordinary
/// rotation an owner performs.
async fn publish_generation(
    router: &RpcRouter,
    state: &Arc<AppState>,
    room_hex: &str,
    room_id: &[u8; 32],
    minter: &ActorKeypair,
) -> (
    RoomPublishGenerationReply,
    fauna_core::crypto::GenerationKey,
    [u8; 32],
) {
    let targets = wrap_targets(state, room_id).await;
    let (mint, key, id) = build_mint(state, room_id, minter, &targets).await;
    let bytes = dispatch(
        router,
        state.clone(),
        minter.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(room_hex, &mint),
    )
    .await
    .expect("an owner's mint over the whole floor is admitted");
    (decode(&bytes).unwrap(), key, id)
}

/// A community room's message: the body sealed under the room's generation
/// key, carried in the `RoomSealed` envelope with the generation named in
/// cleartext so the reader knows which wrap to open.
fn room_send_payload(
    room_hex: &str,
    author: &ActorKeypair,
    key: &fauna_core::crypto::GenerationKey,
    generation: &[u8; 32],
    text: &str,
) -> Bytes {
    room_send_body_payload(
        room_hex,
        author,
        key,
        generation,
        fauna_mls::types::ChannelMessageBody::Text(text.to_string()),
    )
}

/// [`room_send_payload`] for any message body — an attachments message, say.
fn room_send_body_payload(
    room_hex: &str,
    author: &ActorKeypair,
    key: &fauna_core::crypto::GenerationKey,
    generation: &[u8; 32],
    body: fauna_mls::types::ChannelMessageBody,
) -> Bytes {
    // Authored, then sealed: a generation key every member holds authenticates
    // nobody, so the author's signature over `(room, generation, author, stamp,
    // body)` is what attributes the bubble (`fauna_mls::room_message`).
    let sealed = fauna_mls::room_message::seal_room_message(
        key,
        &room_id_of(room_hex),
        generation,
        author,
        1_700_000_000_000,
        body,
    )
    .expect("the author seals its own message under the generation");
    room_send_request(room_hex, generation, sealed)
}

/// A send that **lies about who wrote it**: `signer` seals the message but the
/// core is labelled `claimed_author`. Every member holds the room's generation
/// key, so the envelope this produces is perfectly well-formed and the AEAD
/// opens it — only the author's own signature stands between it and a bubble
/// in somebody else's name (`fauna_mls::room_message` module doc).
///
/// `claimed_author = None` leaves the signer's own id in place, which is how a
/// *non-member* sends honestly-signed bytes into a room it does not belong to.
fn forged_room_send_payload(
    room_hex: &str,
    signer: &ActorKeypair,
    claimed_author: Option<&ActorKeypair>,
    key: &fauna_core::crypto::GenerationKey,
    generation: &[u8; 32],
    text: &str,
) -> Bytes {
    let mut signed = fauna_mls::room_message::RoomMessageCore {
        room: room_id_of(room_hex).to_vec(),
        generation: generation.to_vec(),
        author: signer.actor_id(),
        sent_at_ms: 1_700_000_000_000,
        body: fauna_mls::types::ChannelMessageBody::Text(text.to_string()),
    }
    .sign(signer)
    .expect("the signer signs its own core");
    if let Some(claimed) = claimed_author {
        signed.core.author = claimed.actor_id();
    }
    let sealed = fauna_core::group_content::seal_group_content(
        key,
        fauna_core::group_content::ROOM_MESSAGE_CONTENT_KIND,
        generation,
        &encode_canonical(&signed).unwrap(),
    )
    .expect("a key-holder can seal whatever it likes");
    room_send_request(room_hex, generation, sealed)
}

/// The `channel.send` request carrying one already-sealed room ciphertext, with
/// the generation named in cleartext so the reader knows which wrap to open.
fn room_send_request(room_hex: &str, generation: &[u8; 32], sealed: Vec<u8>) -> Bytes {
    let envelope = fauna_mls::types::ChannelEnvelope::RoomSealed {
        generation: generation.to_vec(),
        ciphertext: sealed,
    };
    let req = fauna_protocol::conversations::ChannelSendRequest {
        channel_id: room_hex.into(),
        envelope: envelope.to_bytes().expect("envelope encodes"),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

async fn room_view_hits(state: &Arc<AppState>, room_id: &[u8; 32], term: &str) -> usize {
    let schema = CacheDb::room_view_schema(room_id);
    state
        .db
        .search_fts(term, Some(&schema), None, None, 50, 0)
        .await
        .expect("search the room's derived views")
        .len()
}

/// Found a room whose owner arrives WITH its wrap target — the community
/// class's own ceremony, since `RoomCreateRequest` carries the founder's
/// group-reception public key. Returns the reply and the owner's keypair
/// record, because a test that wants to open a wrap needs the secret half.
///
/// The shared `create_room` helper founds with no key (the end-to-end path,
/// which needs none), so the sealing tests use this rather than reaching past
/// the ceremony into the roster table.
async fn create_keyed_room(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    salt: [u8; 32],
) -> (
    RoomCreateReply,
    fauna_core::group_generation::GroupReceptionKeyRecord,
) {
    let record = reception();
    let policy =
        fauna_mls::room_policy::RoomPolicy::initial(owner.actor_id(), Some("the square".into()));
    let signed = policy
        .sign(owner)
        .expect("owner signs its own birth policy");
    let req = RoomCreateRequest {
        salt: hex::encode(salt),
        policy: encode_canonical(&signed).unwrap().to_vec(),
        reception_pubkey: record.reception_pubkey().unwrap(),
        extra: std::collections::BTreeMap::new(),
    };
    let bytes = dispatch(
        router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.create",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("the owner's own birth record is admitted");
    (decode(&bytes).unwrap(), record)
}

/// Seat a member that arrives WITH a wrap target — the community class's
/// acceptance, where the roster row and the wrap target are one fact.
async fn seat_keyed_member(
    router: &RpcRouter,
    state: &Arc<AppState>,
    inviter: &ActorKeypair,
    room_hex: &str,
    room_id: &[u8; 32],
    who: &ActorKeypair,
) -> fauna_core::group_generation::GroupReceptionKeyRecord {
    let record = reception();
    accept_contact(state, who, inviter).await;
    dispatch(
        router,
        state.clone(),
        inviter.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            inviter,
            room_id,
            who,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("the invitation is recorded");
    dispatch(
        router,
        state.clone(),
        who.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload_with_key(room_hex, record.reception_pubkey().unwrap()),
    )
    .await
    .expect("the invitee accepts with its wrap target");
    record
}

#[tokio::test]
async fn a_room_is_born_with_the_home_nest_as_a_wrap_target() {
    // Reason 1 of the ratified key model: "It is what 'the nest is a member
    // with a key' means literally" — the home nest's room-read keypair is one
    // more recipient, seated by the ceremony rather than configured.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let reply = create_room(&router, &state, &owner, birth_salt(0x11)).await;
    let room_id = room_id_of(&reply.room_id);

    let roster = state.db.list_floor_roster(&room_id).await.unwrap();
    let nest_row = roster
        .iter()
        .find(|m| m.principal_kind == "nest")
        .expect("the home nest is a founding member");
    // `conn_blocking` parks the calling thread on the DB mutex, which a tokio
    // worker may not do — the same reason every production caller of this
    // module goes through `spawn_blocking`.
    let db = state.db.clone();
    let seed = state.nest_signing_key.as_ref().unwrap().to_bytes();
    let published = tokio::task::spawn_blocking(move || {
        fauna_nest::room_read_key::public_key(&db.conn_blocking(), &seed)
    })
    .await
    .expect("the key task joins")
    .expect("the room-read key was minted at boot and reads back");
    assert_eq!(
        nest_row.reception_pubkey.as_deref(),
        Some(published.as_slice()),
        "the nest seats ITS OWN room-read key — never one the founder named"
    );
    assert!(
        nest_row.entry_id.is_some(),
        "every seating gets a roster entry — the slot a wrap is bound to"
    );
}

#[tokio::test]
async fn the_owner_mints_a_generation_and_every_live_member_gets_a_wrap() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x12)).await;
    let room_id = room_id_of(&reply.room_id);

    let (published, _key, generation_id) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    assert_eq!(published.generation_id, hex::encode(generation_id));
    assert_eq!(
        published.covered, 2,
        "coverage is the owner and the home nest — the room's whole live floor"
    );
    assert!(
        !published.nest_read_revoked,
        "a mint that wraps to the nest grants the read, it does not revoke it"
    );

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.generations",
        Bytes::from(
            encode_canonical(&RoomGenerationsRequest {
                room_id: reply.room_id.clone(),
                extra: std::collections::BTreeMap::new(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("a live member reads its generations");
    let gens: RoomGenerationsReply = decode(&bytes).unwrap();
    assert_eq!(gens.generations.len(), 1);
    assert!(gens.generations[0].is_tip);
    // The wrap really is the owner's: it opens with the owner's reception
    // secret and the recovered key matches the mint's commitment.
    let entry = room_id_of(&gens.generations[0].entry_id);
    let commitment: [u8; 32] = gens.generations[0]
        .key_commitment
        .clone()
        .try_into()
        .unwrap();
    group_generation_wraps::open_group_generation_key_as_entry(
        &gens.generations[0].wrap,
        &owner_recv.keypair().unwrap().secret,
        &generation_id,
        &entry,
        &commitment,
    )
    .expect("the member's own wrap opens with its own reception secret");
}

/// **Row 749.** An invitee's reception key of the right length but a
/// FIPS-203-invalid ML-KEM half is refused at acceptance — before the fix
/// nothing checked this door's key at all, only `set_reception_key` did.
/// Refused before anything is written: the invitation stays pending and no
/// seat is added.
#[tokio::test]
async fn accept_invite_refuses_a_fips_invalid_key() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let invitee = ActorKeypair::generate();
    let (created, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x79)).await;
    let room_id = room_id_of(&created.room_id);

    accept_contact(&state, &invitee, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.invite",
        invite_payload(
            &owner,
            &room_id,
            &invitee,
            fauna_mls::room_policy::RoomRole::Member,
        ),
    )
    .await
    .expect("the invitation is recorded");

    let err = dispatch(
        &router,
        state.clone(),
        invitee.actor_id().0,
        "fauna.conversations.room.accept_invite",
        accept_payload_with_key(
            &created.room_id,
            vec![0xFFu8; fauna_mls::wrapped_blob::XWING_ENCAPS_KEY_LEN],
        ),
    )
    .await
    .expect_err("a FIPS-203-invalid reception key is refused");
    assert_eq!(err.code, "fauna.conversations.invalid_params");

    assert!(
        state
            .db
            .get_pending_room_invite(&room_id, &invitee.actor_id().0)
            .await
            .unwrap()
            .is_some(),
        "the invitation stays pending"
    );
    assert!(
        floor_row_of(&state, &room_id, &invitee).await.is_none(),
        "and no seat was added"
    );
}

#[tokio::test]
async fn a_plain_member_does_not_mint_a_room_generation() {
    // "Owner and admin devices mint and rotate" (§ Don't do these). The
    // floor's ranks are this plane's fill of the scheme's authority seam.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x13)).await;
    let room_id = room_id_of(&reply.room_id);
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;

    let targets = wrap_targets(&state, &room_id).await;
    let (mint, _, _) = build_mint(&state, &room_id, &member, &targets).await;
    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect_err("a plain member's mint is refused");
    assert!(
        format!("{err:?}").contains("owner or admin"),
        "the refusal names the rank rule: {err:?}"
    );
}

#[tokio::test]
async fn a_mint_that_leaves_a_live_member_unwrapped_is_refused() {
    // Roster COVERAGE is the admissibility rule (`account-data-taxonomy.md`
    // § The recipient-set scheme, delta (ii)). Without it a rotation could
    // silently sever a member the room still says is a member.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x14)).await;
    let room_id = room_id_of(&reply.room_id);
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;

    let mut targets = wrap_targets(&state, &room_id).await;
    targets.retain(|t| t.member_actor.0 != member.actor_id().0);
    let (mint, _, _) = build_mint(&state, &room_id, &owner, &targets).await;
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect_err("a mint that misses a live member is refused");
    assert!(
        format!("{err:?}").contains("every live member"),
        "the refusal names coverage: {err:?}"
    );
    assert!(
        state
            .db
            .room_generation_tip(&room_id)
            .await
            .unwrap()
            .is_none(),
        "a refused mint stores nothing — the room still has no tip"
    );
}

#[tokio::test]
async fn a_mint_that_does_not_name_the_tip_as_its_parent_is_refused() {
    // The chain is a strict ratchet, so two concurrent rotations cannot both
    // land: this plane has no arbiter to resolve the fork that would leave.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x15)).await;
    let room_id = room_id_of(&reply.room_id);

    // Two mints built against the SAME (empty) parent — the concurrent case.
    let targets = wrap_targets(&state, &room_id).await;
    let (first, _, _) = build_mint(&state, &room_id, &owner, &targets).await;
    let (second, _, _) = build_mint(&state, &room_id, &owner, &targets).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &first),
    )
    .await
    .expect("the first mint lands");
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &second),
    )
    .await
    .expect_err("the second, parented on the same predecessor, is refused");
    assert!(
        format!("{err:?}").contains("parent"),
        "the refusal names the ratchet: {err:?}"
    );
}

#[tokio::test]
async fn a_mint_the_caller_did_not_sign_is_refused() {
    // The minter is bound to the authenticated caller, the way an invitation's
    // signer is: the floor judges actors, so a mint it cannot attribute to a
    // ranked actor is not one it can admit.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x16)).await;
    let room_id = room_id_of(&reply.room_id);

    let targets = wrap_targets(&state, &room_id).await;
    let (mint, _, _) = build_mint(&state, &room_id, &stranger, &targets).await;
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect_err("a mint signed by somebody else is refused");
    assert!(
        format!("{err:?}").contains("minter"),
        "the refusal names the binding: {err:?}"
    );
}

#[tokio::test]
async fn the_home_nest_opens_a_community_rooms_send_and_builds_a_derived_view() {
    // The class's whole point (§ The three classes → *Community*, and § *What
    // the home nest does with its read*): the log rests sealed, the nest holds
    // one wrap, and what it does with it is the community's search index.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x17)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            "the hedgehog convention",
        ),
    )
    .await
    .expect("a floor member's sealed send is stored");

    assert_eq!(
        room_view_hits(&state, &room_id, "hedgehog").await,
        1,
        "the home nest opened its wrap and indexed what a member wrote"
    );
}

#[tokio::test]
async fn a_send_labelled_with_another_members_name_builds_no_view() {
    // The community class's attribution property, at the nest's own reader.
    // Every member holds the room's generation key, so a member can seal a
    // well-formed envelope carrying anybody's name — the AEAD cannot tell them
    // apart, because sealing under a key you legitimately hold is not tampering.
    // Only the author's signature can (`fauna_mls::room_message`), and the nest
    // checks it before a single word reaches the corpus it derives.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let forger = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x2a)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        forged_room_send_payload(
            &reply.room_id,
            &forger,
            Some(&owner),
            &key,
            &generation,
            "the hedgehog convention",
        ),
    )
    .await
    .expect("the send is stored — the nest is not the judge of a member's bytes");

    assert_eq!(
        room_view_hits(&state, &room_id, "hedgehog").await,
        0,
        "a message signed by one actor and labelled another must build no view"
    );
}

#[tokio::test]
async fn a_non_members_honestly_signed_send_builds_no_view() {
    // A signature proves *who* wrote the bytes, never that they were entitled
    // to (`room_message` module doc → *What a verified signature does NOT
    // establish*). Here the bytes are honestly signed — by somebody who is not
    // on the floor — so the signature check passes and the **floor roster** is
    // what refuses it. The derived corpus holds exactly what the room's own
    // members wrote (`conversation-rooms.md` § The floor roster).
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let outsider = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x2b)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        forged_room_send_payload(
            &reply.room_id,
            &outsider,
            None,
            &key,
            &generation,
            "the hedgehog convention",
        ),
    )
    .await
    .expect("the send is stored — the nest is not the judge of a member's bytes");

    assert_eq!(
        room_view_hits(&state, &room_id, "hedgehog").await,
        0,
        "a non-member's message must build no view, however well it is signed"
    );
}

#[tokio::test]
async fn a_nest_with_no_wrap_reads_nothing_of_the_room() {
    // The negative of the test above, and the property that makes the grant a
    // GRANT: without a wrap minted to it, the home nest holds only ciphertext.
    // Here the room is never keyed at all — the send is stored and nothing is
    // derived from it.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let reply = create_room(&router, &state, &owner, birth_salt(0x18)).await;
    let room_id = room_id_of(&reply.room_id);
    let unheld = fauna_core::crypto::GenerationKey::from_bytes([0x99u8; 32]);

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &unheld,
            &[0x88u8; 32],
            "the hedgehog convention",
        ),
    )
    .await
    .expect("the send is a member's act whether or not the nest can read it");

    assert_eq!(
        room_view_hits(&state, &room_id, "hedgehog").await,
        0,
        "no wrap, no view — and no refusal either"
    );
}

#[tokio::test]
async fn rotating_the_nest_out_revokes_its_read_and_deletes_the_views_it_built() {
    // The materialization grant's REVOKE (`principles.md` § The user always
    // controls their data; § The three classes reason 1: "revocable by
    // rotating it out"). Three things must all hold: the views the nest
    // already built are gone, the reply says so, and nothing new is derived.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x19)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            "the hedgehog convention",
        ),
    )
    .await
    .expect("the first send is indexed");
    assert_eq!(room_view_hits(&state, &room_id, "hedgehog").await, 1);

    // Rotate to a generation the nest holds no wrap for.
    let mut targets = wrap_targets(&state, &room_id).await;
    targets.retain(|t| t.member_actor.0 == owner.actor_id().0);
    let (mint, key2, generation2) = build_mint(&state, &room_id, &owner, &targets).await;
    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect("a mint may leave the nest out — that is the revoke");
    let published: RoomPublishGenerationReply = decode(&bytes).unwrap();
    assert!(
        published.nest_read_revoked,
        "the nest reports its own read revoked rather than letting it lapse silently"
    );
    assert_eq!(
        room_view_hits(&state, &room_id, "hedgehog").await,
        0,
        "revoke DELETES the views — not at some later sweep, in the act itself"
    );

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key2,
            &generation2,
            "hedgehog again",
        ),
    )
    .await
    .expect("the room goes on without the nest reading it");
    assert_eq!(
        room_view_hits(&state, &room_id, "hedgehog").await,
        0,
        "and nothing new is derived once the members have rotated the nest out"
    );
}

/// The roster read as a member sees it, keyed by principal hex.
async fn roster_tip_wraps(
    router: &RpcRouter,
    state: &Arc<AppState>,
    reader: &ActorKeypair,
    room_hex: &str,
) -> std::collections::BTreeMap<String, Option<bool>> {
    let bytes = dispatch(
        router,
        state.clone(),
        reader.actor_id().0,
        "fauna.conversations.room.list_roster",
        Bytes::from(
            encode_canonical(&RoomListRosterRequest {
                room_id: room_hex.into(),
                at_policy_version: None,
                extra: std::collections::BTreeMap::new(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("a live member reads the floor");
    let reply: RoomListRosterReply = decode(&bytes).unwrap();
    reply
        .members
        .into_iter()
        .map(|m| (m.principal, m.tip_wrapped))
        .collect()
}

/// **The roster says who holds a wrap for the room's current generation** —
/// the one fact about the key material a member needs and the generation read
/// cannot give it (that door serves each caller its *own* wraps only).
///
/// Two consumers make it load-bearing, and this pins both answers:
/// - the **home nest's** row is whether the nest reads the room — "whether a
///   nest reads is the members' standing choice, visible on the roster"
///   (`encryption-at-rest.md` § Conversation message envelopes). Without it a
///   member could learn the grant was withdrawn only from the reply to the
///   mint that withdrew it, so every other seat's editor would show a grant
///   that no longer exists — and a later ordinary rotation, wrapping to every
///   target it can see, would hand the nest its read back unasked;
/// - a **user's** row is whether an owner or admin still owes it a key-in:
///   acceptance seats a member with no wrap, and only an owner or admin can
///   cover them, so without this an app cannot tell a newcomer who needs one
///   from a member who already has one.
#[tokio::test]
async fn the_roster_read_says_which_principals_hold_a_wrap_for_the_tip() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x1b)).await;
    let room_id = room_id_of(&reply.room_id);
    let owner_hex = hex::encode(owner.actor_id().0);
    let member_hex = hex::encode(member.actor_id().0);
    let nest_hex = hex::encode(state.nest_identity.public_key_bytes());

    // A room nobody has keyed has no tip to hold a wrap for: absent, not false.
    let unkeyed = roster_tip_wraps(&router, &state, &owner, &reply.room_id).await;
    assert_eq!(unkeyed[&owner_hex], None, "no generation, no answer");
    assert_eq!(unkeyed[&nest_hex], None, "no generation, no answer");

    publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    let keyed = roster_tip_wraps(&router, &state, &owner, &reply.room_id).await;
    assert_eq!(keyed[&owner_hex], Some(true));
    assert_eq!(
        keyed[&nest_hex],
        Some(true),
        "the founding mint wraps to the nest — the grant, visible on the roster"
    );

    // Acceptance seats a member with a wrap target and no wrap.
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;
    let seated = roster_tip_wraps(&router, &state, &owner, &reply.room_id).await;
    assert_eq!(
        seated[&member_hex],
        Some(false),
        "a newcomer the room has not keyed in yet — the row an owner's device acts on"
    );

    // Rotate the nest out: the member is covered, the nest is not.
    let mut targets = wrap_targets(&state, &room_id).await;
    targets.retain(|t| hex::encode(t.member_actor.0) != nest_hex);
    let (mint, _, _) = build_mint(&state, &room_id, &owner, &targets).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect("the revoking rotation lands");
    let revoked = roster_tip_wraps(&router, &state, &member, &reply.room_id).await;
    assert_eq!(revoked[&member_hex], Some(true));
    assert_eq!(
        revoked[&nest_hex],
        Some(false),
        "every member — not only the one that minted — can see the nest no longer reads"
    );
}

#[tokio::test]
async fn a_straggler_under_an_old_generation_does_not_resurrect_a_deleted_view() {
    // The revoke would be worthless if a message sealed under a generation the
    // nest still holds a wrap for could re-index after it. The nest reads the
    // TIP's wrap, not the envelope's.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x1a)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key1, generation1) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    let mut targets = wrap_targets(&state, &room_id).await;
    targets.retain(|t| t.member_actor.0 == owner.actor_id().0);
    let (mint, _, _) = build_mint(&state, &room_id, &owner, &targets).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect("the revoking rotation lands");

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key1,
            &generation1,
            "hedgehog straggler",
        ),
    )
    .await
    .expect("a straggler under the old generation is still a legitimate send");
    assert_eq!(
        room_view_hits(&state, &room_id, "hedgehog").await,
        0,
        "the nest holds generation 1's wrap and still builds nothing — the tip decides"
    );
}

/// **A revoke stamped EARLIER than the generation it replaces still ends the
/// nest's read** — the tip is the end of the parent chain, never the mint with
/// the greatest `minted_at_ms` (`community-rooms.md` § Implementation status
/// today → *The sealing lands*: the mint names the room's current tip as its
/// parent, a strict ratchet — that build record moved out of
/// `conversation-rooms.md` on 2026-09-10).
///
/// `minted_at_ms` is the minting device's wall clock, and a room's owner and
/// admins are different devices, so this needs no attacker: an admin whose
/// clock runs behind the owner's produces exactly this mint. Ordered by the
/// stamp, the revoke sorts BEFORE generation 1, the tip never advances to it,
/// and the straggler below re-opens under the wrap the nest still holds for
/// generation 1 — after the reply said `nest_read_revoked` and the views were
/// deleted. The revoke reports success while the read continues.
///
/// The straggler test above cannot see this: the suite's helper stamps every
/// mint ascending, so its stamps and its chain always agree.
#[tokio::test]
async fn a_revoke_stamped_before_its_parent_still_ends_the_nests_read() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x1b)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key1, generation1) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    let generation1_stamp = state
        .db
        .room_generation_tip(&room_id)
        .await
        .unwrap()
        .expect("generation 1 is the tip")
        .minted_at_ms;

    // The revoke: every live member but the home nest, parented on generation
    // 1 as admission requires — and stamped a full minute EARLIER.
    let mut targets = wrap_targets(&state, &room_id).await;
    targets.retain(|t| t.member_actor.0 == owner.actor_id().0);
    let (mint, _, revoke) = build_mint_with(
        &targets,
        vec![generation1],
        &owner,
        generation1_stamp - 60_000,
    );
    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect("a revoke naming the tip as its parent is admitted, whatever its stamp");
    let published: RoomPublishGenerationReply = decode(&bytes).unwrap();
    assert!(
        published.nest_read_revoked,
        "the reply tells the room the nest's read is withdrawn"
    );
    assert_eq!(
        state
            .db
            .room_generation_tip(&room_id)
            .await
            .unwrap()
            .map(|t| t.generation_id),
        Some(revoke),
        "the revoke is the tip: it names generation 1 as its parent, and the \
         chain — not the stamp — orders a room's generations"
    );

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key1,
            &generation1,
            "porcupine straggler",
        ),
    )
    .await
    .expect("a straggler under the old generation is still a legitimate send");
    assert_eq!(
        room_view_hits(&state, &room_id, "porcupine").await,
        0,
        "the nest still holds generation 1's wrap and must build nothing from it: \
         the revoke said the read was withdrawn, so a view here is a deleted view \
         resurrecting behind the user's back"
    );
}

/// **A mint stamped before its parent cannot move the tip backwards, and the
/// chain cannot fork through the admission door.**
///
/// Ordered by the stamp, G2 (parent G1, stamped earlier) left G1 as the tip —
/// so admission, which reads the tip through that same ordering, then ADMITTED
/// a G3 naming G1 as its parent: two children of G1, the fork the strict
/// ratchet exists to make impossible, since this plane has no arbiter to
/// resolve one. With the tip taken from the chain, G2 is the tip and the stale
/// G3 is refused like any other mint that does not name the current tip.
#[tokio::test]
async fn a_mint_stamped_before_its_parent_still_becomes_the_tip_and_the_chain_cannot_fork() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x1c)).await;
    let room_id = room_id_of(&reply.room_id);
    let targets = wrap_targets(&state, &room_id).await;

    let (g1_mint, _, g1) = build_mint_with(&targets, Vec::new(), &owner, 1_700_000_100_000);
    let (g2_mint, _, g2) = build_mint_with(&targets, vec![g1], &owner, 1_700_000_050_000);
    for mint in [&g1_mint, &g2_mint] {
        dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.publish_generation",
            publish_payload(&reply.room_id, mint),
        )
        .await
        .expect("each names the tip as it stood when published");
    }
    let order: Vec<[u8; 32]> = state
        .db
        .list_room_generations(&room_id)
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.generation_id)
        .collect();
    assert_eq!(
        order,
        vec![g1, g2],
        "oldest first, tip last — by the parent chain, though G2's stamp is earlier"
    );

    // A third mint naming G1 — the tip before G2 landed.
    let (stale, _, _) = build_mint_with(&targets, vec![g1], &owner, 1_700_000_200_000);
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &stale),
    )
    .await
    .expect_err("a second child of G1 would fork the chain");
    assert!(
        format!("{err:?}").contains("does not name the room's current generation as its parent"),
        "refused by the ratchet, and for that reason: {err:?}"
    );
    assert_eq!(
        state
            .db
            .list_room_generations(&room_id)
            .await
            .unwrap()
            .len(),
        2,
        "the refused mint stored nothing — the chain is still G1 ← G2"
    );
}

#[tokio::test]
async fn a_re_admitted_member_comes_back_on_a_fresh_roster_entry() {
    // "Re-admission is a fresh entry id, so add-wins resurrection is
    // unrepresentable" — here that means the wraps of the generations the
    // member was severed from stay bound to a slot it no longer holds.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let reply = create_room(&router, &state, &owner, birth_salt(0x1b)).await;
    let room_id = room_id_of(&reply.room_id);
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;
    let first = wrap_targets(&state, &room_id)
        .await
        .into_iter()
        .find(|t| t.member_actor.0 == member.actor_id().0)
        .expect("the member has an entry")
        .entry_id;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&reply.room_id, &member),
    )
    .await
    .expect("the owner removes the member");
    // A re-invitation and a re-acceptance is a NEW seating, and the entry id
    // derives from the seating stamp — so the two must fall in different
    // milliseconds for the test to be about two seatings at all.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await; // sleep-ok: separating two seatings in time IS the property under test, not a settle wait
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;
    let second = wrap_targets(&state, &room_id)
        .await
        .into_iter()
        .find(|t| t.member_actor.0 == member.actor_id().0)
        .expect("the member is seated again")
        .entry_id;

    assert_ne!(
        first, second,
        "a re-admission is a new roster entry, never the old seat reoccupied"
    );
}

#[tokio::test]
async fn the_home_nests_read_is_revoked_by_rotation_not_by_removal() {
    // Ruled at the sealing build: unseating the home nest would derive the
    // room back to `end_to_end` (rule 1) while its log stays sealed under the
    // recipient-set scheme — a room whose stored class names a key model it
    // does not use. Rotation revokes without touching the member set.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let reply = create_room(&router, &state, &owner, birth_salt(0x1c)).await;
    let nest_principal = state.nest_identity.public_key_bytes();

    let req = RoomRemoveRequest {
        room_id: reply.room_id.clone(),
        principal: hex::encode(nest_principal),
        extra: std::collections::BTreeMap::new(),
    };
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.remove",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("removing the home nest is refused");
    assert!(
        format!("{err:?}").contains("rotating"),
        "the refusal points at the door that DOES revoke: {err:?}"
    );
}

#[tokio::test]
async fn a_caller_off_the_floor_reads_no_generations() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x1d)).await;
    let room_id = room_id_of(&reply.room_id);
    publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.generations",
        Bytes::from(
            encode_canonical(&RoomGenerationsRequest {
                room_id: reply.room_id.clone(),
                extra: std::collections::BTreeMap::new(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect_err("a non-member is refused, not answered with an empty list");
    assert!(
        format!("{err:?}").contains("not a member"),
        "silence would read as 'this room has no generations': {err:?}"
    );
}

#[test]
fn the_sealing_kinds_are_user_class_only() {
    // Holding a room's key is an end-user act bound to the caller's own place
    // on that room's floor. `Admin` is deliberately NOT asserted against: an
    // admin is a user with a grantable extra role and inherits the whole User
    // set (`bridge_method_allowlist` § Admin ⊇ User) — and it changes nothing
    // here, because the mint door re-checks the caller's rank on the ROOM's
    // floor, which no nest-side role confers.
    for kind in [
        "fauna.conversations.room.publish_generation",
        "fauna.conversations.room.generations",
    ] {
        assert!(is_permitted(CallerClass::User, kind));
        for class in [
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::BridgeAtprotoPds,
            CallerClass::Custodian,
            CallerClass::ContentProcessor,
        ] {
            assert!(!is_permitted(class, kind), "{class:?} holds no room's keys");
        }
    }
}

// ── the retained-generation backfill an ADD wraps ───────────────
//
// `conversation-rooms.md` § History for joiners, the community arm: a
// newcomer's history is "the retained generation bundle wrapped to the
// newcomer at admission — the recipient-set scheme's archival backfill",
// and "in a community room the nest refuses to store one" the policy does
// not authorize. The scheme's own rule (`account-data-taxonomy.md` § The
// recipient-set scheme → *Mint triggers*) is that an ADD never mints: it
// wraps the retained bundle to the new member. `room.backfill_generations`
// is that act's door, and the policy is what bounds which generations it
// may cover.

/// Build one top-up wrap of `generation` to `target`'s CURRENT roster entry,
/// signed by `healer` — the scheme's own move
/// (`group_generation_wraps::build_group_topup_wrap`), which is why this
/// helper assembles a record rather than the nest minting anything.
async fn build_topup(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    target: &ActorKeypair,
    healer: &ActorKeypair,
    gen_key: &fauna_core::crypto::GenerationKey,
    generation: &[u8; 32],
) -> Vec<u8> {
    let member = wrap_targets(state, room_id)
        .await
        .into_iter()
        .find(|m| m.member_actor == target.actor_id())
        .expect("the target is a live floor member with a wrap target");
    let (_cell, record) = group_generation_wraps::build_group_topup_wrap(
        gen_key,
        &member,
        generation,
        healer.signing_key(),
        1_700_000_100_000,
    )
    .expect("the top-up assembles");
    encode_canonical(&record).unwrap().to_vec()
}

fn backfill_payload(room_hex: &str, target: &ActorKeypair, wraps: Vec<Vec<u8>>) -> Bytes {
    let req = RoomBackfillGenerationsRequest {
        room_id: room_hex.into(),
        target_actor_id: hex::encode(target.actor_id().0),
        wraps,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Set the room's history policy — an owner-signed policy change, version
/// bumped off the stored one, so the test drives the same door a client does.
async fn set_history_policy(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    room_hex: &str,
    room_id: &[u8; 32],
    history: fauna_mls::room_policy::HistoryPolicy,
) {
    let room = state
        .db
        .get_room(room_id)
        .await
        .expect("room read")
        .expect("the room record");
    let mut policy =
        fauna_mls::room_policy::RoomPolicy::initial(owner.actor_id(), Some("the square".into()));
    policy.version = room.policy_version.unwrap_or(1) + 1;
    policy.history_policy = history;
    dispatch(
        router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(room_hex, owner, &policy),
    )
    .await
    .expect("the owner sets the room's history policy");
}

/// Appoint an already-seated member as admin — an owner-signed policy naming
/// it, because an admin rank comes from the policy and never from a seating.
async fn appoint_admin(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    room_hex: &str,
    room_id: &[u8; 32],
    who: &ActorKeypair,
) {
    let room = state
        .db
        .get_room(room_id)
        .await
        .expect("room read")
        .expect("the room record");
    let mut policy =
        fauna_mls::room_policy::RoomPolicy::initial(owner.actor_id(), Some("the square".into()));
    policy.version = room.policy_version.unwrap_or(1) + 1;
    policy.set_admins(vec![who.actor_id()]);
    dispatch(
        router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(room_hex, owner, &policy),
    )
    .await
    .expect("the owner appoints the admin");
}

/// The caller's own generations, as `room.generations` serves them.
async fn read_generations(
    router: &RpcRouter,
    state: &Arc<AppState>,
    room_hex: &str,
    who: &ActorKeypair,
) -> RoomGenerationsReply {
    let bytes = dispatch(
        router,
        state.clone(),
        who.actor_id().0,
        "fauna.conversations.room.generations",
        Bytes::from(
            encode_canonical(&RoomGenerationsRequest {
                room_id: room_hex.into(),
                extra: std::collections::BTreeMap::new(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("a live member reads its generations");
    decode(&bytes).unwrap()
}

#[tokio::test]
async fn a_member_seated_after_the_tip_mint_holds_no_wrap_until_a_backfill() {
    // The gap this door closes, stated as the newcomer sees it. Coverage is
    // checked at MINT time over the floor as it stood then, so a member
    // seated afterwards is covered by nothing — not even the tip, so it
    // cannot read the room's NEW messages either. That is the
    // admission/mint race the scheme's member top-up exists to heal, and it
    // is why `history_policy: none` must still authorize the tip: refusing
    // the tip would leave a newcomer unable to read the room at all.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let newcomer = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x40)).await;
    let room_id = room_id_of(&reply.room_id);

    let (_, gen_key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    let newcomer_recv =
        seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &newcomer).await;

    let before = read_generations(&router, &state, &reply.room_id, &newcomer).await;
    assert!(
        before.generations.is_empty(),
        "a member seated after the mint holds no wrap — the state the backfill heals"
    );

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(
            &reply.room_id,
            &newcomer,
            vec![build_topup(&state, &room_id, &newcomer, &owner, &gen_key, &generation).await],
        ),
    )
    .await
    .expect("an owner backfills the tip to a newcomer under any history policy");

    let after = read_generations(&router, &state, &reply.room_id, &newcomer).await;
    assert_eq!(after.generations.len(), 1, "the tip is now readable");
    assert!(after.generations[0].is_tip);

    // And it really opens — a wrap the nest stored but the member cannot
    // open would be worse than no wrap at all.
    let entry = room_id_of(&after.generations[0].entry_id);
    let commitment: [u8; 32] = after.generations[0]
        .key_commitment
        .clone()
        .try_into()
        .expect("32-byte commitment");
    group_generation_wraps::open_group_generation_key_as_entry(
        &after.generations[0].wrap,
        &newcomer_recv.keypair().unwrap().secret,
        &generation,
        &entry,
        &commitment,
    )
    .expect("the backfilled wrap opens at the newcomer's own entry");
}

#[tokio::test]
async fn a_second_backfill_over_an_existing_wrap_leaves_the_first_in_place() {
    // `INSERT OR REPLACE` let an admin silently overwrite a wrap a member
    // already holds with arbitrary bytes — the nest cannot tell a real wrap
    // from an unopenable one. The store now only ADDS coverage
    // (`community-rooms.md` § Implementation status today → *A seat gains or
    // rotates its wrap target*, rule (b): "the wraps already stored at the
    // entry stay openable and nothing is re-sealed"), so a second backfill
    // over the same `(generation, entry)` is a no-op, never a re-key.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let newcomer = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x43)).await;
    let room_id = room_id_of(&reply.room_id);

    let (_, gen_key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    let newcomer_recv =
        seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &newcomer).await;

    let first_wrap = build_topup(&state, &room_id, &newcomer, &owner, &gen_key, &generation).await;
    let first_bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(&reply.room_id, &newcomer, vec![first_wrap.clone()]),
    )
    .await
    .expect("the first backfill stores the tip");
    let first_reply: RoomBackfillGenerationsReply = decode(&first_bytes).unwrap();
    assert_eq!(first_reply.stored, 1, "the first backfill is a fresh store");

    let entry_id = room_id_of(
        &read_generations(&router, &state, &reply.room_id, &newcomer)
            .await
            .generations[0]
            .entry_id,
    );
    let stored_after_first = state
        .db
        .get_room_generation_wrap(&room_id, &generation, &entry_id)
        .await
        .unwrap()
        .expect("the tip wrap is stored");

    // A second, independently-sealed wrap for the SAME generation and entry
    // — as an admin resupplying (or corrupting) bytes would.
    let second_wrap = build_topup(&state, &room_id, &newcomer, &owner, &gen_key, &generation).await;
    assert_ne!(
        first_wrap, second_wrap,
        "each top-up wrap is freshly sealed — a rerun is not a byte-for-byte repeat, so a \
         changed stored value could only mean the store overwrote it"
    );

    let second_bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(&reply.room_id, &newcomer, vec![second_wrap]),
    )
    .await
    .expect("a repeat backfill over existing coverage is not an error, just a no-op");
    let second_reply: RoomBackfillGenerationsReply = decode(&second_bytes).unwrap();
    assert_eq!(
        second_reply.stored, 0,
        "nothing new was stored — the entry already held a wrap for this generation"
    );

    let stored_after_second = state
        .db
        .get_room_generation_wrap(&room_id, &generation, &entry_id)
        .await
        .unwrap()
        .expect("the wrap is still there");
    assert_eq!(
        stored_after_second, stored_after_first,
        "the ORIGINAL wrap survives untouched — a backfill adds coverage, it never replaces it"
    );

    // And it still opens for the newcomer, exactly as before the repeat.
    let after = read_generations(&router, &state, &reply.room_id, &newcomer).await;
    let entry = room_id_of(&after.generations[0].entry_id);
    let commitment: [u8; 32] = after.generations[0]
        .key_commitment
        .clone()
        .try_into()
        .expect("32-byte commitment");
    group_generation_wraps::open_group_generation_key_as_entry(
        &after.generations[0].wrap,
        &newcomer_recv.keypair().unwrap().secret,
        &generation,
        &entry,
        &commitment,
    )
    .expect("the original wrap still opens after a repeat backfill was skipped");
}

#[tokio::test]
async fn a_none_history_policy_refuses_a_pre_admission_generation() {
    // "in a community room the nest refuses to store one"
    // (`conversation-rooms.md` § History for joiners). Under `none` a
    // newcomer "sees the room from their admission; nothing before", so the
    // authorized set is the TIP alone — an older, retained generation is
    // exactly the history the policy withholds.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let newcomer = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x41)).await;
    let room_id = room_id_of(&reply.room_id);

    // Two generations: the first retains content, the second is the tip.
    let (_, old_key, old_generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &newcomer).await;

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(
            &reply.room_id,
            &newcomer,
            vec![
                build_topup(
                    &state,
                    &room_id,
                    &newcomer,
                    &owner,
                    &old_key,
                    &old_generation,
                )
                .await,
            ],
        ),
    )
    .await
    .expect_err("a `none` room's history is not backfillable");
    assert!(
        format!("{err:?}").contains("history policy"),
        "the refusal names the policy that withholds it: {err:?}"
    );

    // Nothing was stored — the refusal is before the write, so a rejected
    // batch cannot leave a partial bundle behind.
    let after = read_generations(&router, &state, &reply.room_id, &newcomer).await;
    assert!(
        after.generations.is_empty(),
        "a refused backfill stores nothing at all"
    );
}

#[tokio::test]
async fn a_full_history_policy_backfills_the_whole_retained_bundle() {
    // The `full` arm: "a newcomer receives the room's history", realized as
    // "the retained generation bundle wrapped to the newcomer at admission"
    // (`conversation-rooms.md` § History for joiners) — tip AND the
    // generations still covering live content (`account-data-taxonomy.md`
    // § The recipient-set scheme → *Mint triggers*).
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let newcomer = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x42)).await;
    let room_id = room_id_of(&reply.room_id);

    set_history_policy(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &room_id,
        fauna_mls::room_policy::HistoryPolicy::Full,
    )
    .await;

    let (_, key1, generation1) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    let (_, key2, generation2) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &newcomer).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(
            &reply.room_id,
            &newcomer,
            vec![
                build_topup(&state, &room_id, &newcomer, &owner, &key1, &generation1).await,
                build_topup(&state, &room_id, &newcomer, &owner, &key2, &generation2).await,
            ],
        ),
    )
    .await
    .expect("a `full` room's whole retained bundle is backfillable");

    let after = read_generations(&router, &state, &reply.room_id, &newcomer).await;
    assert_eq!(
        after.generations.len(),
        2,
        "the newcomer holds the whole retained bundle, oldest first"
    );
    assert_eq!(after.generations[0].generation_id, hex::encode(generation1));
    assert!(!after.generations[0].is_tip);
    assert!(after.generations[1].is_tip);
}

#[tokio::test]
async fn a_plain_member_does_not_backfill() {
    // Key authority is the room's owner and admins, never a plain member and
    // never the nest (`conversation-rooms.md` § Don't do these). The
    // backfill hands out a generation key, so it takes the same rank the
    // mint does.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let newcomer = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x43)).await;
    let room_id = room_id_of(&reply.room_id);

    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;
    let (_, gen_key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &newcomer).await;

    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(
            &reply.room_id,
            &newcomer,
            vec![build_topup(&state, &room_id, &newcomer, &member, &gen_key, &generation).await],
        ),
    )
    .await
    .expect_err("a plain member holds no key authority");
    assert!(
        format!("{err:?}").contains("owner or admin"),
        "the refusal names the rank the act needs: {err:?}"
    );
}

#[tokio::test]
async fn a_backfill_whose_healer_is_not_the_caller_is_refused() {
    // The mint binds its `minter` to the authenticated caller; the backfill
    // binds its `healer` the same way. Without it an admin could relay a
    // record some other principal signed, and the floor's ranks — which
    // judge ACTORS — would be judging the wrong one.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let newcomer = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x44)).await;
    let room_id = room_id_of(&reply.room_id);

    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &admin).await;
    appoint_admin(&router, &state, &owner, &reply.room_id, &room_id, &admin).await;
    let (_, gen_key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &newcomer).await;

    // The OWNER signs the record; the admin — a rank that may backfill —
    // tries to publish it.
    let err = dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(
            &reply.room_id,
            &newcomer,
            vec![build_topup(&state, &room_id, &newcomer, &owner, &gen_key, &generation).await],
        ),
    )
    .await
    .expect_err("a record the caller did not sign is refused");
    assert!(
        format!("{err:?}").contains("healer"),
        "the refusal names the binding it broke: {err:?}"
    );
}

#[tokio::test]
async fn a_backfill_to_a_stale_roster_entry_is_refused() {
    // A re-admitted member returns on a FRESH entry id (§ Implementation
    // status today), which is what stops a wrap minted for the seat it was
    // removed from opening at the new one. A backfill naming the old entry
    // would hand that guarantee straight back.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x45)).await;
    let room_id = room_id_of(&reply.room_id);

    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;
    let (_, gen_key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    // Build the top-up against the entry the member holds NOW …
    let stale = build_topup(&state, &room_id, &member, &owner, &gen_key, &generation).await;

    // … then remove and re-admit it, so that entry is no longer the live one.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&reply.room_id, &member),
    )
    .await
    .expect("the owner removes the member");
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(&reply.room_id, &member, vec![stale]),
    )
    .await
    .expect_err("a wrap bound to a retired entry is refused");
    assert!(
        format!("{err:?}").contains("roster entry"),
        "the refusal names the entry mismatch: {err:?}"
    );
}

#[tokio::test]
async fn a_backfill_to_a_non_member_is_refused() {
    // The target must be a live floor member: backfilling a stranger would
    // hand the room's key to a principal the room does not say is a member.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x46)).await;
    let room_id = room_id_of(&reply.room_id);

    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;
    let (_, gen_key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    let wrap = build_topup(&state, &room_id, &member, &owner, &gen_key, &generation).await;
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(&reply.room_id, &stranger, vec![wrap]),
    )
    .await
    .expect_err("a stranger is not backfillable");
    assert!(format!("{err:?}").contains("not a live member"), "{err:?}");
}

#[tokio::test]
async fn the_home_nest_is_never_a_backfill_target() {
    // The nest's read is a grant the members make by wrapping to it AT A
    // MINT, and withdraw by rotating it out (§ Implementation status today,
    // the revoke). A backfill door that could re-wrap to the nest would let
    // an admin restore a read the members had just revoked, outside the one
    // act the design gives them.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x47)).await;
    let room_id = room_id_of(&reply.room_id);

    let (_, gen_key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    let nest_row = state
        .db
        .list_floor_roster(&room_id)
        .await
        .expect("roster")
        .into_iter()
        .find(|m| m.principal_kind == "nest")
        .expect("the home nest is a founding member");
    let nest_member = RosterMember {
        entry_id: nest_row.entry_id.clone().unwrap().try_into().unwrap(),
        member_actor: ActorId(nest_row.principal_id),
        reception_pubkey: nest_row.reception_pubkey.clone().unwrap(),
        enrolled_at_ms: nest_row.joined_at,
    };
    let (_cell, record) = group_generation_wraps::build_group_topup_wrap(
        &gen_key,
        &nest_member,
        &generation,
        owner.signing_key(),
        1_700_000_100_000,
    )
    .expect("the top-up assembles");

    let req = RoomBackfillGenerationsRequest {
        room_id: reply.room_id.clone(),
        target_actor_id: hex::encode(nest_row.principal_id),
        wraps: vec![encode_canonical(&record).unwrap().to_vec()],
        extra: std::collections::BTreeMap::new(),
    };
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("the home nest's read is granted at a mint, never by a backfill");
    assert!(
        format!("{err:?}").contains("home nest"),
        "the refusal names why: {err:?}"
    );
}

#[test]
fn the_backfill_kind_is_user_class_only() {
    let kind = "fauna.conversations.room.backfill_generations";
    assert!(is_permitted(CallerClass::User, kind));
    for class in [
        CallerClass::BridgeMta,
        CallerClass::BridgeMda,
        CallerClass::BridgeAtprotoPds,
        CallerClass::Custodian,
        CallerClass::ContentProcessor,
    ] {
        assert!(!is_permitted(class, kind), "{class:?} holds no room's keys");
    }
}

// ── the foreign member's generation read, relayed ───────────────
//
// `conversation-rooms.md` § The home nest: "a member on a foreign nest
// reaches the room only through their own home nest, which originates the leg
// to the room's home over the nest↔nest channel", and "a foreign member's
// home nest … never holds a wrap, so a community room's readable views exist
// on the home nest alone". These drive the room home's serving half —
// `fauna.federation.conversation.generations.fetch` — directly, with the
// verified `origin_nest_id` the federation channel would have supplied.

/// Dispatch the room home's `fauna.federation.conversation.generations.fetch`
/// exactly as a peer connection would: through the registered federation
/// serving table, with the verified `origin_nest_id` the channel supplies.
///
/// Going through the table rather than calling the handler function keeps the
/// registration itself under test — a kind that fell off the federation
/// allowlist would make these cases fail, which is the point of §4.C.
async fn dispatch_federation(
    state: &Arc<AppState>,
    origin_nest_id: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, fauna_protocol::RpcError> {
    dispatch_federation_kind(
        state,
        "fauna.federation.conversation.generations.fetch",
        origin_nest_id,
        payload,
    )
    .await
}

/// [`dispatch_federation`] for any conversations federation kind.
async fn dispatch_federation_kind(
    state: &Arc<AppState>,
    kind: &str,
    origin_nest_id: [u8; 32],
    payload: Bytes,
) -> Result<Bytes, fauna_protocol::RpcError> {
    let mut b = fauna_nest::federation_router::FederationRouter::builder();
    fauna_nest::federation_handlers::register_conversations_federation_handlers(&mut b);
    let router = b.build();
    let meta = router
        .kind_meta(kind)
        .unwrap_or_else(|| panic!("{kind} is on the federation allowlist"));
    (meta.handler)(state.clone(), origin_nest_id, payload).await
}

fn fed_generations_payload(actor: &ActorKeypair, room_hex: &str) -> Bytes {
    let req = fauna_nest::federation_handlers::FedRoomGenerationsRequest {
        requesting_actor_id: hex::encode(actor.actor_id().0),
        room_id: room_hex.into(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

#[tokio::test]
async fn a_foreign_member_reads_its_own_wraps_through_its_home_nest() {
    // The relay's whole point: a member seated on this room's floor but homed
    // on another nest can still open the room's sealed log, because its own
    // nest originates this leg for it.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let abroad = ActorKeypair::generate();
    let their_nest = [0xB7u8; 32];
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x50)).await;
    let room_id = room_id_of(&reply.room_id);

    let abroad_recv =
        seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &abroad).await;
    let (_, _key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    // The binding the room home wrote when this member's Welcome was
    // relayed: `abroad` is a foreign member of this channel, homed on
    // `their_nest`. That row is what `require_foreign_member` checks.
    state
        .db
        .register_foreign_channel_member(
            &room_id,
            &abroad.actor_id().0,
            &their_nest,
            None,
            fauna_nest::db::channels::RebindPower::InsertOnly,
        )
        .await
        .expect("the foreign membership binding is recorded");

    let bytes = dispatch_federation(
        &state,
        their_nest,
        fed_generations_payload(&abroad, &reply.room_id),
    )
    .await
    .expect("the room home serves a bound foreign member its own wraps");
    let served: fauna_nest::federation_handlers::FedRoomGenerationsReply = decode(&bytes).unwrap();

    assert_eq!(
        served.generations.len(),
        1,
        "the tip, wrapped to this member"
    );
    let entry = room_id_of(&served.generations[0].entry_id);
    let commitment: [u8; 32] = served.generations[0]
        .key_commitment
        .clone()
        .try_into()
        .expect("32-byte commitment");
    group_generation_wraps::open_group_generation_key_as_entry(
        &served.generations[0].wrap,
        &abroad_recv.keypair().unwrap().secret,
        &generation,
        &entry,
        &commitment,
    )
    .expect("the relayed wrap opens with the foreign member's own reception secret");
}

#[tokio::test]
async fn a_relaying_nest_cannot_substitute_itself_as_the_recipient() {
    // The security property the relay leg exists to preserve: "a foreign
    // member's home nest … never holds a wrap" (§ The home nest). The
    // request names an ACTOR and never a roster entry, so a hostile relay
    // cannot ask for the home nest's own room-read wrap — the one wrap on
    // this floor that would actually let it read the room.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let abroad = ActorKeypair::generate();
    let their_nest = [0xB8u8; 32];
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x51)).await;
    let room_id = room_id_of(&reply.room_id);

    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &abroad).await;
    publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    state
        .db
        .register_foreign_channel_member(
            &room_id,
            &abroad.actor_id().0,
            &their_nest,
            None,
            fauna_nest::db::channels::RebindPower::InsertOnly,
        )
        .await
        .unwrap();

    // The home nest IS a seated floor principal holding a wrap. Name it as
    // the requesting actor and the structural gate refuses: it is not a
    // recorded foreign member of this channel from `their_nest`.
    let nest_principal = state.nest_identity.public_key_bytes();
    let req = fauna_nest::federation_handlers::FedRoomGenerationsRequest {
        requesting_actor_id: hex::encode(nest_principal),
        room_id: reply.room_id.clone(),
    };
    let err = dispatch_federation(
        &state,
        their_nest,
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("a relay cannot ask for the home nest's own wrap");
    assert_eq!(err.code, "fauna.federation.forbidden");
}

#[tokio::test]
async fn a_nest_that_is_not_the_members_home_is_refused_the_relay() {
    // The `channel.fetch` gate verbatim: the requesting actor's recorded home
    // nest must be the connection's verified origin. Otherwise any nest that
    // learned a room id could pull a member's wraps and hand them on.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let abroad = ActorKeypair::generate();
    let their_nest = [0xB9u8; 32];
    let impostor = [0xBAu8; 32];
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x52)).await;
    let room_id = room_id_of(&reply.room_id);

    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &abroad).await;
    publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    state
        .db
        .register_foreign_channel_member(
            &room_id,
            &abroad.actor_id().0,
            &their_nest,
            None,
            fauna_nest::db::channels::RebindPower::InsertOnly,
        )
        .await
        .unwrap();

    let err = dispatch_federation(
        &state,
        impostor,
        fed_generations_payload(&abroad, &reply.room_id),
    )
    .await
    .expect_err("only the member's own recorded home nest relays for it");
    assert_eq!(err.code, "fauna.federation.forbidden");
}

#[tokio::test]
async fn the_relay_serves_only_the_named_members_own_wraps() {
    // The same "own wraps only" rule the same-nest door has, reached through
    // the relay: a foreign member asking for a room it is bound to does not
    // see a co-member's wrap, so the door cannot enumerate key material even
    // for a nest that hosts several of the room's members.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let abroad = ActorKeypair::generate();
    let housemate = ActorKeypair::generate();
    let their_nest = [0xBBu8; 32];
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x53)).await;
    let room_id = room_id_of(&reply.room_id);

    let abroad_recv =
        seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &abroad).await;
    let housemate_recv = seat_keyed_member(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &room_id,
        &housemate,
    )
    .await;
    let (_, _key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    for who in [&abroad, &housemate] {
        state
            .db
            .register_foreign_channel_member(
                &room_id,
                &who.actor_id().0,
                &their_nest,
                None,
                fauna_nest::db::channels::RebindPower::InsertOnly,
            )
            .await
            .unwrap();
    }

    let bytes = dispatch_federation(
        &state,
        their_nest,
        fed_generations_payload(&abroad, &reply.room_id),
    )
    .await
    .expect("the room home serves the named member");
    let served: fauna_nest::federation_handlers::FedRoomGenerationsReply = decode(&bytes).unwrap();
    assert_eq!(served.generations.len(), 1);

    let entry = room_id_of(&served.generations[0].entry_id);
    let commitment: [u8; 32] = served.generations[0]
        .key_commitment
        .clone()
        .try_into()
        .unwrap();
    // It is `abroad`'s wrap: it opens with `abroad`'s secret and NOT with the
    // housemate's, even though both members are homed on the same nest.
    group_generation_wraps::open_group_generation_key_as_entry(
        &served.generations[0].wrap,
        &abroad_recv.keypair().unwrap().secret,
        &generation,
        &entry,
        &commitment,
    )
    .expect("it is the named member's own wrap");
    // `GenerationKey` is deliberately not `Debug` — it is key material — so
    // this asserts on the discriminant rather than unwrapping the error.
    assert!(
        group_generation_wraps::open_group_generation_key_as_entry(
            &served.generations[0].wrap,
            &housemate_recv.keypair().unwrap().secret,
            &generation,
            &entry,
            &commitment,
        )
        .is_err(),
        "a co-member homed on the same nest cannot open it"
    );
}

#[test]
fn the_remote_generation_read_kind_is_user_class_only() {
    let kind = "fauna.conversations.room.generations_remote";
    assert!(is_permitted(CallerClass::User, kind));
    for class in [
        CallerClass::BridgeMta,
        CallerClass::BridgeMda,
        CallerClass::BridgeAtprotoPds,
        CallerClass::Custodian,
        CallerClass::ContentProcessor,
    ] {
        assert!(!is_permitted(class, kind), "{class:?} holds no room's keys");
    }
}

// ── fauna.conversations.room.search ────────────────────────────
//
// The read position's FIRST purpose, made reachable
// (`conversation-rooms.md` § The three classes → *What the home nest does
// with its read*). Until this door the nest indexed every community-room
// message it could open and nothing could ask the corpus a question: the
// derived view existed and served nobody.

fn room_search_payload(room_hex: &str, query: &str) -> Bytes {
    room_search_payload_with(room_hex, query, false)
}

/// A search that also asks for the room's room-restricted posts — the one flag
/// a caller that renders post hits sets (`RoomSearchRequest::include_posts`).
fn room_search_payload_with(room_hex: &str, query: &str, include_posts: bool) -> Bytes {
    let req = RoomSearchRequest {
        room_id: room_hex.into(),
        query: query.into(),
        limit: None,
        include_posts,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

#[tokio::test]
async fn a_member_searches_the_room_and_the_hit_names_the_message() {
    // What the search index is FOR. The community class is the unbounded
    // one, so searching the room is the work a member's own device cannot
    // do — it holds whatever slice of the log it fetched. The hit has to
    // name the message, or the answer is unusable.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x41)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    for text in ["the hedgehog convention", "an unrelated aardvark"] {
        dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.channel.send",
            room_send_payload(&reply.room_id, &owner, &key, &generation, text),
        )
        .await
        .expect("a floor member's sealed send is stored");
    }

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload(&reply.room_id, "hedgehog"),
    )
    .await
    .expect("a live floor member may search the room");
    let found: RoomSearchReply = decode(&bytes).unwrap();

    assert_eq!(found.hits.len(), 1, "one message matched: {found:?}");
    // Sends are appended in order, so the hedgehog message is the room's
    // first — and the hit must say so rather than handing back an opaque
    // index key nothing can fetch.
    assert_eq!(
        found.hits[0].seq,
        Some(1),
        "the hit names the log position of the message it matched"
    );
}

#[tokio::test]
async fn the_search_answers_where_and_never_what() {
    // The load-bearing rule of this door. The nest indexes under the TIP,
    // but a member holds a wrap only for the generations minted while it sat
    // on the floor — so a member seated after the current mint cannot read
    // the room's new messages at all (§ Implementation status today, the
    // admission/mint race the top-up heals). A snippet in this reply would
    // hand that member exactly the plaintext the sealing plane withholds,
    // with the nest's own read as the leak. So the reply carries positions
    // and ranks, and the message text appears nowhere in its bytes.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x42)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            "the hedgehog convention",
        ),
    )
    .await
    .expect("the send is stored and indexed");

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload(&reply.room_id, "hedgehog"),
    )
    .await
    .expect("the member searches");
    let found: RoomSearchReply = decode(&bytes).unwrap();
    assert_eq!(found.hits.len(), 1);

    // The whole encoded reply, not just the fields this build happens to
    // have: a snippet added later would fail here.
    let on_the_wire = bytes.to_vec();
    for word in ["hedgehog", "convention"] {
        assert!(
            !on_the_wire
                .windows(word.len())
                .any(|w| w == word.as_bytes()),
            "the reply must carry no plaintext of the message: {word:?} is in its bytes"
        );
    }
}

#[tokio::test]
async fn a_non_member_cannot_search_the_room() {
    // Reading the room is what every member does — and only a member. The
    // floor is the authority for a community room (§ The floor roster), and
    // the refusal is explicit rather than an empty list, for
    // `room_generations_for_principal`'s reason: silence would read as
    // "this room holds nothing".
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x43)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            "the hedgehog convention",
        ),
    )
    .await
    .expect("the send is stored and indexed");

    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload(&reply.room_id, "hedgehog"),
    )
    .await
    .expect_err("somebody off the floor cannot search the room");
    assert!(
        format!("{err:?}").contains("not a member"),
        "the refusal names the membership gate: {err:?}"
    );
}

/// One search through the real door, as `who` is served it.
async fn search_as(
    router: &RpcRouter,
    state: &Arc<AppState>,
    who: &ActorKeypair,
    room_hex: &str,
    term: &str,
) -> RoomSearchReply {
    search_as_with(router, state, who, room_hex, term, false).await
}

/// [`search_as`] for a caller that also asked for the room's posts.
async fn search_as_with(
    router: &RpcRouter,
    state: &Arc<AppState>,
    who: &ActorKeypair,
    room_hex: &str,
    term: &str,
    include_posts: bool,
) -> RoomSearchReply {
    let bytes = dispatch(
        router,
        state.clone(),
        who.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload_with(room_hex, term, include_posts),
    )
    .await
    .expect("a live floor member may search the room");
    decode(&bytes).unwrap()
}

#[tokio::test]
async fn a_hit_is_served_only_for_a_position_the_caller_could_open() {
    // The other half of *the door answers where, never what*
    // (`community-rooms.md` § The three classes): a POSITION is an answer
    // too. A member seated under `history_policy: none` "sees the room from
    // their admission; nothing before" (`conversation-rooms.md` § History for
    // joiners), and the nest indexed every earlier message under a generation
    // that member holds no wrap for. Serving it the position and the rank of
    // a pre-admission match makes the door a chosen-plaintext oracle over
    // exactly the history the policy withholds — one probe per term, answered
    // by *where*. So the rule the paragraph already gives binds the
    // positions and not only the snippet: a hit is served only for a position
    // the caller could open.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let newcomer = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x4a)).await;
    let room_id = room_id_of(&reply.room_id);

    // The history: one message, sealed and indexed under the first mint.
    let (_, first_key, first_generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &first_key,
            &first_generation,
            "the hedgehog convention",
        ),
    )
    .await
    .expect("the pre-admission send is stored and indexed");

    // The newcomer is seated afterwards — under `none`, every room's default
    // — and the next mint over the floor covers it from here on.
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &newcomer).await;
    let (_, tip_key, tip_generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &tip_key,
            &tip_generation,
            "an aardvark parade",
        ),
    )
    .await
    .expect("the post-admission send is stored and indexed");

    // The founder holds both wraps, so the corpus answers it whole.
    let founder_history = search_as(&router, &state, &owner, &reply.room_id, "hedgehog").await;
    assert_eq!(
        founder_history.hits.len(),
        1,
        "a founding member opens its own room's history: {founder_history:?}"
    );
    let founder_recent = search_as(&router, &state, &owner, &reply.room_id, "aardvark").await;
    assert_eq!(founder_recent.hits.len(), 1, "{founder_recent:?}");

    // The newcomer holds a wrap for the tip alone.
    let newcomer_history = search_as(&router, &state, &newcomer, &reply.room_id, "hedgehog").await;
    assert!(
        newcomer_history.hits.is_empty(),
        "a position the newcomer can never open is a position it is not told about: {newcomer_history:?}"
    );
    let newcomer_recent = search_as(&router, &state, &newcomer, &reply.room_id, "aardvark").await;
    assert_eq!(
        newcomer_recent.hits.len(),
        1,
        "what was said after the admission is the newcomer's to find: {newcomer_recent:?}"
    );
    assert_eq!(
        newcomer_recent.hits[0].seq, founder_recent.hits[0].seq,
        "and it names the same position the founder is served"
    );
}

#[tokio::test]
async fn a_backfilled_member_finds_the_history_it_was_given() {
    // The `full` arm of the same rule, and the reason the filter is by WRAP
    // COVERAGE and never by admission time: under `full` "a newcomer receives
    // the room's history" (`conversation-rooms.md` § History for joiners), so
    // the wraps it is given ARE the history it may search. The cheap
    // approximation — serve only `seq >= the admission seq` — would withhold
    // exactly this, and would be wrong in the other direction too: a member
    // removed and re-admitted searches only what its new entry holds wraps
    // for — its earlier tenure returns only if an existing member backfills
    // it to the new entry under `full`.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let newcomer = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x4b)).await;
    let room_id = room_id_of(&reply.room_id);
    set_history_policy(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &room_id,
        fauna_mls::room_policy::HistoryPolicy::Full,
    )
    .await;

    let (_, first_key, first_generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &first_key,
            &first_generation,
            "the hedgehog convention",
        ),
    )
    .await
    .expect("the pre-admission send is stored and indexed");
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &newcomer).await;
    publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;

    // A `full` policy does not backfill itself: until the wrap is stored the
    // newcomer is in exactly the `none` member's position.
    let before = search_as(&router, &state, &newcomer, &reply.room_id, "hedgehog").await;
    assert!(
        before.hits.is_empty(),
        "the history is not the newcomer's until it is wrapped to it: {before:?}"
    );

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(
            &reply.room_id,
            &newcomer,
            vec![
                build_topup(
                    &state,
                    &room_id,
                    &newcomer,
                    &owner,
                    &first_key,
                    &first_generation,
                )
                .await,
            ],
        ),
    )
    .await
    .expect("a `full` room's retained bundle is backfillable");

    let after = search_as(&router, &state, &newcomer, &reply.room_id, "hedgehog").await;
    assert_eq!(
        after.hits.len(),
        1,
        "the history it was given is the history it may search: {after:?}"
    );
}

#[tokio::test]
async fn a_re_admitted_member_does_not_recover_its_earlier_tenures_search() {
    // The `none` arm of the same rule, pinned on its own: a member removed
    // and re-admitted returns on a FRESH roster entry
    // (`account-data-taxonomy.md` § The recipient-set scheme — "re-admission
    // is a fresh entry id"), and the search door filters by that entry's own
    // wrap coverage, never by time. Nothing backfills the new entry, so what
    // the member's earlier tenure could open stays exactly as unreachable as
    // a newcomer's.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x4c)).await;
    let room_id = room_id_of(&reply.room_id);

    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;
    let (_, key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            "the wombat referendum",
        ),
    )
    .await
    .expect("the member's own tenure send is stored and indexed");

    // Its own tenure can find it …
    let during = search_as(&router, &state, &member, &reply.room_id, "wombat").await;
    assert_eq!(
        during.hits.len(),
        1,
        "a seated member searches the generation it holds a wrap for: {during:?}"
    );

    // … then it is removed and re-admitted, landing on a fresh entry id.
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&reply.room_id, &member),
    )
    .await
    .expect("the owner removes the member");
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;

    let after = search_as(&router, &state, &member, &reply.room_id, "wombat").await;
    assert!(
        after.hits.is_empty(),
        "the new entry holds no wrap for the tenure it replaced, and nothing backfilled it: {after:?}"
    );
}

#[tokio::test]
async fn rotating_the_nest_out_leaves_the_search_with_nothing_to_answer_from() {
    // The materialization grant's REVOKE, seen through the door that reads
    // the view (`principles.md` § The user always controls their data). The
    // members rotate the nest's wrap out; the views go in the same act, and
    // the search that could answer a moment ago answers empty — not an
    // error, because withdrawing the grant is a member's own act.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x44)).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            "the hedgehog convention",
        ),
    )
    .await
    .expect("the first send is indexed");

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload(&reply.room_id, "hedgehog"),
    )
    .await
    .expect("the member searches while the grant stands");
    assert_eq!(decode::<RoomSearchReply>(&bytes).unwrap().hits.len(), 1);

    // Rotate to a generation the nest holds no wrap for.
    let mut targets = wrap_targets(&state, &room_id).await;
    targets.retain(|t| t.member_actor.0 == owner.actor_id().0);
    let (mint, _key2, _generation2) = build_mint(&state, &room_id, &owner, &targets).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect("the members may rotate their own nest out");

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload(&reply.room_id, "hedgehog"),
    )
    .await
    .expect("the door still answers — the revoke is not an error");
    assert!(
        decode::<RoomSearchReply>(&bytes).unwrap().hits.is_empty(),
        "the revoke deleted the views the search read from"
    );
    // Both halves of the view, not just the searchable one. The seq map
    // carries no text, but it is a derived view all the same — it says which
    // positions of the log the nest was able to read — and the revoke is
    // "delete this room's derived views", not "delete the half a query would
    // have found".
    assert_eq!(
        state
            .db
            .count_room_message_views(&room_id)
            .await
            .expect("count the room's view map"),
        0,
        "the (room, seq) map goes in the same act as the FTS rows"
    );
}

#[test]
fn the_room_search_kind_is_user_class_only() {
    let kind = "fauna.conversations.room.search";
    assert!(is_permitted(CallerClass::User, kind));
    for class in [
        CallerClass::BridgeMta,
        CallerClass::BridgeMda,
        CallerClass::BridgeAtprotoPds,
        CallerClass::Custodian,
        CallerClass::ContentProcessor,
    ] {
        assert!(
            !is_permitted(class, kind),
            "{class:?} has no floor row to be gated on"
        );
    }
}

// ── room-restricted posts: the read's third purpose ────────────
//
// A post addressed to a community room is its author's ordinary post — it
// never enters the room's log — whose body only the room's floor members open
// (`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*). The
// home nest, when it STORES one, opens it under the tip's wrap in the act that
// stores it and indexes it for the room's search (`conversation-rooms.md`
// § The three classes → *What the home nest does with its read*, purpose 3).

use fauna_core::room_post::RoomPostSeal;

/// A reader nest that also stores posts: the posts handlers beside the
/// conversations ones, and a real blob store for the sealed bodies a client
/// uploads before it creates a gated post.
async fn router_with_reader_nest_and_posts() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlives the test process; never deleted under test
    let backup = Arc::new(
        fauna_nest::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
            .unwrap(),
    );
    let mut state = booted_reader_nest(db).await;
    state.backup_service = Some(backup);
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    fauna_nest::posts_handlers::register_posts_handlers(&mut b);
    (b.build(), state)
}

/// Author a room-restricted post to `room_hex` under generation `generation`,
/// upload its sealed body the way a client does, and create it through the
/// real `fauna.posts.create` door. Returns the post id and the build.
async fn create_room_post(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author: &ActorKeypair,
    room_hex: &str,
    key: &fauna_core::crypto::GenerationKey,
    generation: [u8; 32],
    text: &str,
) -> ([u8; 32], fauna_client_core::post::GatedPostBuild) {
    create_room_post_sealed(
        router,
        state,
        author,
        room_hex,
        key,
        generation,
        fauna_client_core::post::mint_seal_id().unwrap(),
        fauna_core::data::PostBody::Text {
            content: text.into(),
            facets: vec![],
        },
    )
    .await
}

/// [`create_room_post`] for a body the caller assembled itself under a
/// `seal_id` it minted first — what an author does when it attaches media,
/// since a room post's media seal under the very key its body does
/// (`../ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*,
/// ruling 4), and the sealing therefore has to happen before the build.
#[allow(clippy::too_many_arguments)] // the room, the key and the post's own seal — as `build_room_post_at` itself takes them
async fn create_room_post_sealed(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author: &ActorKeypair,
    room_hex: &str,
    key: &fauna_core::crypto::GenerationKey,
    generation: [u8; 32],
    seal_id: [u8; 32],
    body: fauna_core::data::PostBody,
) -> ([u8; 32], fauna_client_core::post::GatedPostBuild) {
    let build = fauna_client_core::post::build_room_post_at(
        author,
        "a post for the square",
        body,
        room_id_of(room_hex),
        RoomPostSeal::Community { generation },
        &fauna_core::group_content::room_post_base_key(key),
        seal_id,
        &fauna_client_core::post::PostAuthoring::now(),
    )
    .expect("a member builds a room post");
    state
        .backup_service
        .as_ref()
        .expect("blob store")
        .local_blob_store()
        .put(
            &fauna_core::data::ContentHash::from_digest_raw(build.encrypted_ref),
            &build.encrypted_blob,
        )
        .await
        .expect("the sealed body uploads first");
    dispatch(
        router,
        state.clone(),
        author.actor_id().0,
        "fauna.posts.create",
        Bytes::from(
            encode_canonical(&fauna_protocol::posts::PostCreateRequest {
                body: serde_bytes::ByteBuf::from(build.post_bytes.clone()),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("a room post is an ordinary post of its author");
    (*blake3::hash(&build.post_bytes).as_bytes(), build)
}

/// A founded, keyed room with its home nest wrapped into the tip: the reply,
/// the owner, the generation key and its id.
async fn keyed_room_with_posts(
    router: &RpcRouter,
    state: &Arc<AppState>,
    salt: [u8; 32],
) -> (
    RoomCreateReply,
    ActorKeypair,
    fauna_core::crypto::GenerationKey,
    [u8; 32],
) {
    let owner = ActorKeypair::generate();
    let (reply, _owner_recv) = create_keyed_room(router, state, &owner, salt).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(router, state, &reply.room_id, &room_id, &owner).await;
    (reply, owner, key, generation)
}

/// The author deletes `post_id` through the ordinary door — a signed
/// tombstone to `fauna.posts.delete` — the act that takes a room post's
/// derived views with it.
async fn delete_room_post(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author: &ActorKeypair,
    post_id: [u8; 32],
) {
    use fauna_core::data::{PostId, Timestamp, Tombstone};
    let tombstone = Tombstone {
        author: author.actor_id(),
        post_id: PostId::from_digest_dag_cbor(post_id),
        created_at: Timestamp::now(),
    };
    let request = fauna_protocol::posts::PostDeleteRequest {
        body: serde_bytes::ByteBuf::from(
            fauna_core::encoding::sign_and_pack(author, &tombstone).unwrap(),
        ),
        extra: std::collections::BTreeMap::new(),
    };
    dispatch(
        router,
        state.clone(),
        author.actor_id().0,
        "fauna.posts.delete",
        Bytes::from(encode_canonical(&request).unwrap().to_vec()),
    )
    .await
    .expect("the author deletes their post");
}

/// A picture sealed under a room post's own per-post key the way
/// `build_room_post_at` requires its caller to seal media —
/// `derive_post_key(room_post_base_key(tip), seal_id)` + `encrypt_content`,
/// the pair `fauna_media::seal`'s Group arm runs — and uploaded first, as a
/// client's upload does. Returns the item a body names it by and the
/// plaintext, so a caller can assert that no read carries the latter. The
/// bytes carry both fixtures' markers (`meow`, `cat`).
async fn upload_sealed_room_picture(
    state: &Arc<AppState>,
    key: &fauna_core::crypto::GenerationKey,
    seal_id: [u8; 32],
) -> (fauna_core::data::MediaItem, Vec<u8>) {
    let plaintext = b"\x89PNG\r\n\x1a\n....meow....cat....IEND".to_vec();
    let per_post = fauna_core::subscription::crypto::derive_post_key(
        &fauna_core::group_content::room_post_base_key(key),
        &fauna_core::data::ContentHash::from_digest_raw(seal_id),
    );
    let sealed = fauna_core::subscription::crypto::encrypt_content(&per_post, &plaintext);
    let blob_hash = fauna_core::encoding::content_hash(&sealed);
    state
        .backup_service
        .as_ref()
        .expect("blob store")
        .local_blob_store()
        .put(&blob_hash, &sealed)
        .await
        .expect("the sealed picture uploads first, as a client's does");
    let item = fauna_core::data::MediaItem {
        blob_hash,
        media_type: "image/png".into(),
        size_bytes: plaintext.len() as u64,
        dimensions: None,
        thumbnail: None,
        ..Default::default()
    };
    (item, plaintext)
}

#[tokio::test]
async fn a_members_room_post_is_indexed_and_the_search_names_it_by_id() {
    let (router, state) = router_with_reader_nest_and_posts().await;
    let (reply, owner, key, generation) =
        keyed_room_with_posts(&router, &state, birth_salt(0x61)).await;
    let room_id = room_id_of(&reply.room_id);
    let (post_id, build) = create_room_post(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        "the hedgehog manifesto",
    )
    .await;

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload_with(&reply.room_id, "hedgehog", true),
    )
    .await
    .expect("a live floor member searches, posts included");
    let found: RoomSearchReply = decode(&bytes).unwrap();
    assert_eq!(found.hits.len(), 1, "{found:?}");
    let hit = &found.hits[0];
    assert_eq!(
        hit.kind,
        fauna_protocol::conversations::RoomSearchHitKind::Post
    );
    assert_eq!(hit.post_id.as_deref(), Some(hex::encode(post_id).as_str()));
    assert_eq!(hit.seq, None, "a post is never in the room's log");
    assert_eq!(state.db.count_room_post_views(&room_id).await.unwrap(), 1);

    // A member opens the stored body with the room's key; anybody holding
    // another room's key opens nothing.
    let seal_id = fauna_core::data::ContentHash::from_digest_raw(build.seal_id);
    let open = |base: [u8; 32]| {
        fauna_core::subscription::crypto::decrypt_content(
            &fauna_core::subscription::crypto::derive_post_key(&base, &seal_id),
            &build.encrypted_blob,
        )
    };
    assert!(open(fauna_core::group_content::room_post_base_key(&key)).is_ok());
    assert!(
        open(fauna_core::group_content::room_post_base_key(
            &fauna_core::crypto::GenerationKey::from_bytes([0x77; 32])
        ))
        .is_err(),
        "a non-member's key opens nothing"
    );
}

#[tokio::test]
async fn a_post_hit_is_served_only_for_a_post_the_caller_could_open() {
    // The post arm of the same rule. A room-restricted post seals
    // under the room's generation exactly as a message does
    // (`../../docs/goal/ui/feed.md` § Encryption at rest → *Room-restricted —
    // the ruling*), so a member that holds no wrap for the generation a post
    // was indexed under cannot open it — and a post id served for it is the
    // same *where*-oracle over the same withheld history.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let (reply, owner, key, generation) =
        keyed_room_with_posts(&router, &state, birth_salt(0x6a)).await;
    let room_id = room_id_of(&reply.room_id);
    let newcomer = ActorKeypair::generate();

    let (before_admission, _) = create_room_post(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        "the hedgehog manifesto",
    )
    .await;

    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &newcomer).await;
    let (_, tip_key, tip_generation) =
        publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    let (after_admission, _) = create_room_post(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &tip_key,
        tip_generation,
        "an aardvark parade",
    )
    .await;

    let founder = search_as_with(&router, &state, &owner, &reply.room_id, "hedgehog", true).await;
    assert_eq!(founder.hits.len(), 1, "{founder:?}");
    assert_eq!(
        founder.hits[0].post_id.as_deref(),
        Some(hex::encode(before_admission).as_str()),
        "the founder is served the post it can open"
    );

    let withheld =
        search_as_with(&router, &state, &newcomer, &reply.room_id, "hedgehog", true).await;
    assert!(
        withheld.hits.is_empty(),
        "a post the newcomer can never open is a post it is not told about: {withheld:?}"
    );

    let served = search_as_with(&router, &state, &newcomer, &reply.room_id, "aardvark", true).await;
    assert_eq!(
        served.hits.len(),
        1,
        "what was posted after the admission is the newcomer's to find: {served:?}"
    );
    assert_eq!(
        served.hits[0].post_id.as_deref(),
        Some(hex::encode(after_admission).as_str())
    );
}
#[tokio::test]
async fn the_post_search_answers_where_and_never_what() {
    let (router, state) = router_with_reader_nest_and_posts().await;
    let (reply, owner, key, generation) =
        keyed_room_with_posts(&router, &state, birth_salt(0x62)).await;
    create_room_post(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        "the hedgehog manifesto",
    )
    .await;
    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload_with(&reply.room_id, "hedgehog", true),
    )
    .await
    .expect("the member searches");
    assert_eq!(decode::<RoomSearchReply>(&bytes).unwrap().hits.len(), 1);
    let on_the_wire = bytes.to_vec();
    for word in ["hedgehog", "manifesto"] {
        assert!(
            !on_the_wire
                .windows(word.len())
                .any(|w| w == word.as_bytes()),
            "the reply must carry no plaintext of the post: {word:?} is in its bytes"
        );
    }
}

#[tokio::test]
async fn a_caller_that_did_not_ask_for_posts_gets_exactly_the_message_page() {
    // An app built before post hits existed requires every hit's `seq`, so it
    // must never meet one — and its page must not be shortened by posts it
    // was never going to be shown.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let (reply, owner, key, generation) =
        keyed_room_with_posts(&router, &state, birth_salt(0x63)).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            "a hedgehog message",
        ),
    )
    .await
    .expect("the message is stored and indexed");
    create_room_post(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        "a hedgehog post",
    )
    .await;

    let older = |include_posts| {
        dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.search",
            room_search_payload_with(&reply.room_id, "hedgehog", include_posts),
        )
    };
    let found: RoomSearchReply = decode(&older(false).await.unwrap()).unwrap();
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert_eq!(found.hits[0].seq, Some(1));
    assert_eq!(
        found.hits[0].kind,
        fauna_protocol::conversations::RoomSearchHitKind::Message
    );

    let both: RoomSearchReply = decode(&older(true).await.unwrap()).unwrap();
    assert_eq!(both.hits.len(), 2, "message and post, one corpus: {both:?}");
}

#[tokio::test]
async fn a_room_post_by_somebody_off_the_floor_builds_no_view() {
    // The derived corpus holds what the room's own members wrote. A stranger
    // who somehow holds the tip's key authors a perfectly well-formed post —
    // it is stored (it is their post), and it is indexed nowhere.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let (reply, _owner, key, generation) =
        keyed_room_with_posts(&router, &state, birth_salt(0x64)).await;
    let stranger = ActorKeypair::generate();
    create_room_post(
        &router,
        &state,
        &stranger,
        &reply.room_id,
        &key,
        generation,
        "the hedgehog manifesto",
    )
    .await;
    assert_eq!(
        state
            .db
            .count_room_post_views(&room_id_of(&reply.room_id))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn a_room_post_under_a_superseded_generation_builds_no_view() {
    // Keyed on the TIP, like a message: a post sealed under a generation the
    // room has rotated past is one the rotation meant to withhold from
    // somebody, and the nest does not index around it.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let (reply, owner, key, generation) =
        keyed_room_with_posts(&router, &state, birth_salt(0x65)).await;
    let room_id = room_id_of(&reply.room_id);
    publish_generation(&router, &state, &reply.room_id, &room_id, &owner).await;
    create_room_post(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        "the hedgehog manifesto",
    )
    .await;
    assert_eq!(state.db.count_room_post_views(&room_id).await.unwrap(), 0);
}

#[tokio::test]
async fn rotating_the_nest_out_purges_the_rooms_post_views_too() {
    // The materialization grant's revoke is "delete this room's derived
    // views" — the post half as much as the message half. Counted, not
    // queried: a search that answers empty cannot tell whether the map
    // behind it survived.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let (reply, owner, key, generation) =
        keyed_room_with_posts(&router, &state, birth_salt(0x66)).await;
    let room_id = room_id_of(&reply.room_id);
    create_room_post(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        "the hedgehog manifesto",
    )
    .await;
    assert_eq!(state.db.count_room_post_views(&room_id).await.unwrap(), 1);

    let mut targets = wrap_targets(&state, &room_id).await;
    targets.retain(|t| t.member_actor.0 == owner.actor_id().0);
    let (mint, _key2, _generation2) = build_mint(&state, &room_id, &owner, &targets).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect("the members may rotate their own nest out");

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload_with(&reply.room_id, "hedgehog", true),
    )
    .await
    .expect("the door still answers");
    assert!(decode::<RoomSearchReply>(&bytes).unwrap().hits.is_empty());
    assert_eq!(
        state.db.count_room_post_views(&room_id).await.unwrap(),
        0,
        "the (room, post) map goes in the same act as the FTS rows"
    );
    assert_eq!(
        room_view_hits(&state, &room_id, "hedgehog").await,
        0,
        "and the message class stayed empty throughout"
    );
}

#[tokio::test]
async fn deleting_a_room_post_withdraws_its_view() {
    // A deleted post must not outlive its derivations: a search hit naming a
    // post that no longer exists is exactly such a derivation.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let (reply, owner, key, generation) =
        keyed_room_with_posts(&router, &state, birth_salt(0x67)).await;
    let room_id = room_id_of(&reply.room_id);
    let (post_id, _) = create_room_post(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        "the hedgehog manifesto",
    )
    .await;
    assert_eq!(state.db.count_room_post_views(&room_id).await.unwrap(), 1);

    delete_room_post(&router, &state, &owner, post_id).await;

    assert_eq!(state.db.count_room_post_views(&room_id).await.unwrap(), 0);
    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload_with(&reply.room_id, "hedgehog", true),
    )
    .await
    .expect("the member searches");
    assert!(decode::<RoomSearchReply>(&bytes).unwrap().hits.is_empty());
}

// ── room-restricted post labels ───────────────────────────────────
//
// Purpose 3's label half (`conversation-rooms.md` § The three classes → *What
// the home nest does with its read*; `ui/feed.md` § Encryption at rest →
// *Room-restricted — the ruling*, ruling 7): the pass that indexes a room
// post also runs the room's named labelers, and the verdicts are served to a
// live floor member through `fauna.posts.room_labels` — never on the envelope
// reads every follower makes.

fn room_labels_payload(post_ids: &[[u8; 32]]) -> Bytes {
    Bytes::from(
        encode_canonical(&fauna_protocol::posts::PostRoomLabelsRequest {
            post_ids: post_ids.iter().map(hex::encode).collect(),
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    )
}

async fn read_room_labels(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: &ActorKeypair,
    post_ids: &[[u8; 32]],
) -> (Bytes, fauna_protocol::posts::PostRoomLabelsReply) {
    let bytes = dispatch(
        router,
        state.clone(),
        caller.actor_id().0,
        "fauna.posts.room_labels",
        room_labels_payload(post_ids),
    )
    .await
    .expect("the verdict read answers any caller");
    let reply = decode(&bytes).unwrap();
    (bytes, reply)
}

/// A keyed, labelled room on a nest that also serves posts, with one room
/// post by its owner: the room reply, the owner, the room id, the post id.
async fn labelled_room_with_a_post(
    router: &RpcRouter,
    state: &Arc<AppState>,
    salt: [u8; 32],
    labelers: &[ActorId],
    text: &str,
) -> (
    RoomCreateReply,
    ActorKeypair,
    [u8; 32],
    fauna_core::crypto::GenerationKey,
    [u8; 32],
    [u8; 32],
) {
    let owner = ActorKeypair::generate();
    let (reply, room_id, key, generation) =
        labelled_room(router, state, &owner, salt, Some(labelers)).await;
    let (post_id, _) = create_room_post(
        router,
        state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        text,
    )
    .await;
    (reply, owner, room_id, key, generation, post_id)
}

#[tokio::test]
async fn a_room_that_names_labelers_labels_a_members_post_at_reception() {
    // The message case, applied to a post: the room names a wasm and a
    // text-model labeler, a member creates a room post, and the member's
    // very first verdict read already carries both planes — scored in the
    // act that indexed the post.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let wasm = publish_wasm_room_labeler(&state).await;
    let model = publish_text_model_room_labeler(&state).await;
    let text = "my cat is dozing";
    let (_reply, owner, _room_id, _key, _generation, post_id) =
        labelled_room_with_a_post(&router, &state, birth_salt(0x71), &[wasm, model], text).await;

    let (bytes, read) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert_eq!(read.posts.len(), 1, "{read:?}");
    let entry = &read.posts[0];
    assert_eq!(entry.post_id, hex::encode(post_id));
    assert_eq!(
        entry.labels,
        vec![fauna_core::content_category::ContentLabelEntry {
            category: "spam".into(),
            confidence_per_mille: 900,
        }],
        "the wasm labeler's canonical verdict, and only it"
    );
    let expected_model =
        fauna_text_model::publish::PublishedTextModel::new(6, 6, cat_text_model_vocabulary())
            .damped_score(text);
    let mut got: Vec<(String, i64, u8)> = entry
        .scores
        .iter()
        .map(|s| (s.factor.clone(), s.score, s.tier))
        .collect();
    got.sort();
    let mut expected = vec![
        (
            fauna_core::scoring::labeler_factor(&wasm),
            900,
            fauna_core::scoring::TIER_COMMUNITY,
        ),
        (
            fauna_core::scoring::labeler_factor(&model),
            expected_model,
            fauna_core::scoring::TIER_COMMUNITY,
        ),
    ];
    expected.sort();
    assert_eq!(got, expected, "one tier-3 factor row per labeler that ran");

    // Metadata only: the verdict read carries none of the post's plaintext.
    for word in ["dozing", "my cat"] {
        assert!(
            !bytes.windows(word.len()).any(|w| w == word.as_bytes()),
            "the read must carry no plaintext of the post: {word:?} is in its bytes"
        );
    }

    // A post nobody labelled — or one that does not exist — has no entry,
    // and asking for it beside a labelled one costs nothing.
    let (_, read) = read_room_labels(&router, &state, &owner, &[[0x99u8; 32], post_id]).await;
    assert_eq!(read.posts.len(), 1);
}

#[tokio::test]
async fn a_taken_down_room_posts_views_are_withheld_and_an_overturn_re_serves_them() {
    // Path 4 of the owner-scoped withhold ruling
    // (`../../docs/goal/behavior/moderation.md` § Legal takedown → *The
    // blob-serve door* → *What the withhold binds on owner- and admin-scoped
    // routes*): the reception pass's store read is not gated — it runs in the
    // act that stores the post — but what it derives IS a serve path for the
    // body. A search hit is an existence oracle over the withheld text (search
    // for a rare word, get the post id back) and a verdict was scored from it,
    // so both are withheld at their serve paths for exactly as long as the flag
    // stands; the view rows stay (tombstone-not-delete), and the overturn
    // re-serves them with nothing re-derived.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let wasm = publish_wasm_room_labeler(&state).await;
    let (reply, owner, room_id, _key, _generation, post_id) = labelled_room_with_a_post(
        &router,
        &state,
        birth_salt(0x73),
        &[wasm],
        "the hedgehog manifesto",
    )
    .await;

    // Before: the member's search names the post, and its verdict reads back.
    let found = search_as_with(&router, &state, &owner, &reply.room_id, "hedgehog", true).await;
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert_eq!(
        found.hits[0].post_id.as_deref(),
        Some(hex::encode(post_id).as_str())
    );
    let (_, labels) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert_eq!(labels.posts.len(), 1, "{labels:?}");

    // The takedown: the same atomic flag write the admin verb performs
    // (`post_legal_takedown_txn`), on the post's own `content_meta` row.
    let content_id_hex = hex::encode(post_id);
    let admin = [0x0au8; 32];
    state
        .db
        .post_legal_takedown_txn(
            &post_id,
            &content_id_hex,
            Some("EU-DSA-2024/12345"),
            &owner.actor_id().0,
            admin.as_slice(),
            "admin=test reference=EU-DSA-2024/12345",
            1_000_000,
        )
        .await
        .expect("the flag lands on the room post's own content_meta row");

    // Withheld at both serve paths — and the rows are still there.
    let found = search_as_with(&router, &state, &owner, &reply.room_id, "hedgehog", true).await;
    assert!(
        found.hits.is_empty(),
        "a taken-down room post must yield no search hit: {found:?}"
    );
    let (_, labels) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert!(
        labels.posts.is_empty(),
        "a taken-down room post must serve no verdict: {labels:?}"
    );
    assert_eq!(
        state.db.count_room_post_views(&room_id).await.unwrap(),
        1,
        "the view row stays for the overturn — withheld, not purged"
    );

    // The overturn clears the flag; nothing is re-derived and both re-serve.
    state
        .db
        .post_legal_takedown_txn(
            &post_id,
            &content_id_hex,
            None,
            &owner.actor_id().0,
            admin.as_slice(),
            "admin=test reason=overturned",
            2_000_000,
        )
        .await
        .expect("the overturn clears the flag");
    let found = search_as_with(&router, &state, &owner, &reply.room_id, "hedgehog", true).await;
    assert_eq!(
        found.hits.len(),
        1,
        "the overturn re-serves the hit: {found:?}"
    );
    let (_, labels) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert_eq!(
        labels.posts.len(),
        1,
        "the overturn re-serves the verdict: {labels:?}"
    );
}

#[tokio::test]
async fn a_follower_off_the_floor_reads_the_post_and_none_of_its_verdicts() {
    // The envelope reads stay un-floor-gated — a room post is its author's
    // ordinary post and every follower gets the sealed bytes. A verdict was
    // derived from the plaintext, so it reaches a live floor member of any
    // rank and nobody else.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let wasm = publish_wasm_room_labeler(&state).await;
    let (reply, owner, room_id, _key, _generation, post_id) =
        labelled_room_with_a_post(&router, &state, birth_salt(0x72), &[wasm], "a cat").await;
    let member = ActorKeypair::generate();
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;

    let (_, seen_by_member) = read_room_labels(&router, &state, &member, &[post_id]).await;
    assert_eq!(
        seen_by_member.posts.len(),
        1,
        "a live member of any rank reads the verdicts"
    );
    assert_eq!(seen_by_member.posts[0].labels.len(), 1);

    // A follower who is not on the floor: the post itself is served …
    let stranger = ActorKeypair::generate();
    let got = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.posts.get",
        Bytes::from(
            encode_canonical(&fauna_protocol::posts::PostGetRequest {
                post_id: hex::encode(post_id),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("the sealed envelope is every follower's to read");
    let got: fauna_protocol::posts::PostGetReply = decode(&got).unwrap();
    assert!(!got.body.is_empty());
    // … and the verdicts are not.
    let (_, seen_by_stranger) = read_room_labels(&router, &state, &stranger, &[post_id]).await;
    assert!(
        seen_by_stranger.posts.is_empty(),
        "a caller off the floor reads no verdicts: {seen_by_stranger:?}"
    );

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&reply.room_id, &member),
    )
    .await
    .expect("the owner removes the member");
    let (_, seen_after_removal) = read_room_labels(&router, &state, &member, &[post_id]).await;
    assert!(
        seen_after_removal.posts.is_empty(),
        "the floor is live: a removed member reads no more verdicts"
    );
}

#[tokio::test]
async fn rotating_the_nest_out_deletes_the_post_verdicts_it_derived() {
    // The revoke covers the post half of the label plane in the SAME act as
    // the search index — counted through the store, because a read after a
    // revoke answers empty whether or not the rows survived it.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let wasm = publish_wasm_room_labeler(&state).await;
    let model = publish_text_model_room_labeler(&state).await;
    let (reply, owner, room_id, _key, _generation, post_id) = labelled_room_with_a_post(
        &router,
        &state,
        birth_salt(0x73),
        &[wasm, model],
        "a cat dozing",
    )
    .await;
    assert_eq!(
        state.db.count_room_post_bus_rows(&room_id).await.unwrap(),
        3,
        "one category row and two factor rows"
    );

    let mut targets = wrap_targets(&state, &room_id).await;
    targets.retain(|t| t.member_actor.0 == owner.actor_id().0);
    let (mint, key2, generation2) = build_mint(&state, &room_id, &owner, &targets).await;
    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect("a mint may leave the nest out — that is the revoke");
    let published: RoomPublishGenerationReply = decode(&bytes).unwrap();
    assert!(published.nest_read_revoked);
    assert_eq!(
        state.db.count_room_post_bus_rows(&room_id).await.unwrap(),
        0,
        "the revoke deletes the post verdicts, not only the searchable half"
    );
    let (_, read) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert!(read.posts.is_empty());

    // A post sealed under the new generation: the nest holds no wrap for it
    // and derives nothing — no view, no verdict.
    create_room_post(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key2,
        generation2,
        "a cat again",
    )
    .await;
    assert_eq!(
        state.db.count_room_post_bus_rows(&room_id).await.unwrap(),
        0,
        "and a nest the members rotated out labels nothing new"
    );
}

#[tokio::test]
async fn deleting_a_room_post_deletes_its_verdicts() {
    // A deleted post must not outlive its derivations — its verdicts as much
    // as its search rows (`ui/feed.md` § Post deletion → *Propagation*).
    let (router, state) = router_with_reader_nest_and_posts().await;
    let wasm = publish_wasm_room_labeler(&state).await;
    let (_reply, owner, room_id, _key, _generation, post_id) =
        labelled_room_with_a_post(&router, &state, birth_salt(0x74), &[wasm], "a cat").await;
    assert_eq!(
        state.db.count_room_post_bus_rows(&room_id).await.unwrap(),
        2,
        "one category row and one factor row"
    );

    delete_room_post(&router, &state, &owner, post_id).await;

    assert_eq!(
        state.db.count_room_post_bus_rows(&room_id).await.unwrap(),
        0,
        "the post's verdicts go with the post"
    );
    let (_, read) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert!(read.posts.is_empty());
}

// ── the roster read serves the wrap targets ────────────────────

/// **A minter can build its mint from the roster READ alone** — the roster
/// read carries each member's `entry_id` and `reception_pubkey`, the two
/// fields `build_group_mint` addresses a wrap to.
///
/// Why this is the roster's own content rather than a new disclosure: the
/// recipient-set scheme defines a roster entry's value as "the member's actor
/// key, **the reception pubkey observed at add**, `Enrolled | Removed`,
/// stamps" (`account-data-taxonomy.md` § The recipient-set scheme → *The
/// roster kind*), and admissibility is **roster coverage** — "a mint is
/// admissible when its wrap set + merged top-ups cover the merged Enrolled
/// roster" (same §). A minter that cannot see the wrap targets cannot satisfy
/// a rule stated over them, and a member that cannot see them cannot check
/// that a mint covered it.
///
/// ⚠ **What this test exists to prevent is a nest-only capability.** Every
/// mint in this file until now was built by `wrap_targets`, which reads
/// `list_floor_roster` **straight out of the database** — a door only code
/// running inside the nest process has. So the whole community class could
/// pass its conformance suite while remaining unmintable by any client, which
/// is exactly the state the class was in: nest-complete and reachable from no
/// app. The assertion is therefore deliberately shaped as *the wire read
/// reproduces what the DB helper produces*, not as "the fields are present".
#[tokio::test]
async fn the_roster_read_serves_the_wrap_targets_a_minter_must_cover() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();

    let (created, _owner_key) = create_keyed_room(&router, &state, &owner, birth_salt(0x5a)).await;
    let room_id = room_id_of(&created.room_id);
    seat_keyed_member(&router, &state, &owner, &created.room_id, &room_id, &member).await;

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.list_roster",
        Bytes::from(
            encode_canonical(&RoomListRosterRequest {
                room_id: created.room_id.clone(),
                at_policy_version: None,
                extra: std::collections::BTreeMap::new(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("the owner reads its own room's floor");
    let reply: RoomListRosterReply = decode(&bytes).unwrap();

    // Owner, member, and the home nest — all three are wrap targets.
    assert_eq!(reply.members.len(), 3);

    // Project the WIRE reply onto the scheme's wrap-target shape, exactly as
    // a client minter would have to.
    let mut from_wire: Vec<RosterMember> = reply
        .members
        .iter()
        .filter_map(|m| {
            let entry = hex::decode(m.entry_id.as_deref()?).ok()?;
            let recv = m.reception_pubkey.clone()?;
            Some(RosterMember {
                entry_id: entry.try_into().ok()?,
                member_actor: ActorId(hex::decode(&m.principal).ok()?.try_into().ok()?),
                reception_pubkey: recv,
                enrolled_at_ms: m.joined_at,
            })
        })
        .collect();

    let mut from_db = wrap_targets(&state, &room_id).await;
    assert_eq!(
        from_db.len(),
        3,
        "the fixture must seat three keyed principals, else this compares two empty sets"
    );

    let key = |r: &RosterMember| r.entry_id;
    from_wire.sort_by_key(key);
    from_db.sort_by_key(key);
    assert_eq!(
        from_wire.len(),
        from_db.len(),
        "the roster read must expose every wrap target the coverage check will \
         demand — a member missing here is a mint the nest refuses for a reason \
         the minter could not have seen"
    );
    for (wire, db) in from_wire.iter().zip(from_db.iter()) {
        assert_eq!(wire.entry_id, db.entry_id);
        assert_eq!(wire.member_actor.0, db.member_actor.0);
        assert_eq!(
            wire.reception_pubkey, db.reception_pubkey,
            "the wire must carry the reception key the coverage check reads, byte for byte"
        );
        assert!(
            !wire.reception_pubkey.is_empty(),
            "an empty reception key is 'unkeyable', not a wrap target"
        );
    }
}

// ── the home nest's labels (purpose 2 of the read) ──────────────
//
// `conversation-rooms.md` § The three classes → *What the home nest does with
// its read*, purpose 2: the home nest applies the transparent labelers the
// room's owner or admins name, in the act that indexes each message, and
// serves what they derive as metadata beside the message — to live floor
// members only, and deleted with every other derived view when the members
// rotate the nest out.

/// A room labeler fixture that speaks the real `label()` ABI: on any input
/// containing `cat` it emits two labels — `spam` at 0.9, one of the canonical
/// five, and `cat` at 0.8, an off-list category no labeler may put on the
/// category plane (`moderation.md` § Categories & enforcement) — and on
/// anything else it emits nothing. wasmi parses WAT text, so these bytes ARE
/// the module and the registry's `wasm_hash` binds to exactly them.
///
/// The hit blob at offset 1024 is `[len = 28 LE][28 BARE bytes]`:
/// `02` (two labels) · `04 "spam"` · `cd cc cc cc cc cc ec 3f` (f64 0.9) ·
/// `01` (`LabelSource::TextAnalysis`) · `03 "cat"` ·
/// `9a 99 99 99 99 99 e9 3f` (f64 0.8) · `01` — pinned by
/// `the_room_labeler_fixture_speaks_the_label_abi`.
const ROOM_LABELER_WAT: &str = r#"
(module
  (memory (export "memory") 2)
  (global $heap_top (mut i32) (i32.const 65536))
  (data (i32.const 1024) "\1c\00\00\00\02\04\73\70\61\6d\cd\cc\cc\cc\cc\cc\ec\3f\01\03\63\61\74\9a\99\99\99\99\99\e9\3f\01")
  (func (export "alloc") (param $size i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $heap_top))
    (global.set $heap_top (i32.add (global.get $heap_top) (local.get $size)))
    (local.get $ptr))
  (func $scan_for_cat (param $ptr i32) (param $len i32) (result i32)
    (local $i i32)
    (local $end i32)
    (local.set $end (i32.sub (i32.add (local.get $ptr) (local.get $len)) (i32.const 2)))
    (local.set $i (local.get $ptr))
    (block $break
      (loop $loop
        (br_if $break (i32.ge_s (local.get $i) (local.get $end)))
        (if (i32.eq (i32.load8_u (local.get $i)) (i32.const 0x63))
          (then
            (if (i32.eq (i32.load8_u (i32.add (local.get $i) (i32.const 1))) (i32.const 0x61))
              (then
                (if (i32.eq (i32.load8_u (i32.add (local.get $i) (i32.const 2))) (i32.const 0x74))
                  (then (return (i32.const 1))))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)))
    (i32.const 0))
  (func (export "label") (param $ptr i32) (param $len i32) (result i32)
    (local $out i32)
    (if (i32.eqz (call $scan_for_cat (local.get $ptr) (local.get $len)))
      (then
        (local.set $out (global.get $heap_top))
        (global.set $heap_top (i32.add (global.get $heap_top) (i32.const 4)))
        (i32.store (local.get $out) (i32.const 0))
        (return (local.get $out))))
    (i32.const 1024))
)
"#;

/// The authenticated caller every labeler publish here runs as — distinct from
/// each labeler's own signing keypair, as in production.
const LABELER_PUBLISH_CALLER: [u8; 32] = [0xC1u8; 32];

/// The text-model fixture's vocabulary — what a publisher's scrub over public
/// cat/finance examples would leave (counts clear the prune floor).
fn cat_text_model_vocabulary() -> Vec<(String, u32, u32)> {
    vec![
        ("cat".to_string(), 6, 0),
        ("kitten".to_string(), 5, 0),
        ("dozing".to_string(), 4, 0),
        ("earnings".to_string(), 0, 6),
        ("quarterly".to_string(), 0, 5),
        ("guidance".to_string(), 0, 4),
    ]
}

/// A room labeler fixture that declares `needs_attachment_bytes` and labels a
/// message **by its attachment bytes** — `nsfw` at 0.7 on any input carrying
/// `meow`, which the tests put inside an attachment's plaintext and never in
/// a message's text (the shared fixture, pinned by
/// `libs/fauna-labeler/tests/attachment_facet.rs`).
const MEOW_BYTES_LABELER_WAT: &str =
    include_str!("../../../tests/e2e-unified/fixtures/labeler/meow_bytes_labeler.wat");

/// A signed `AlgorithmLabeler` over `artifact`, published by `publisher`.
fn signed_room_labeler_metadata(publisher: &ActorKeypair, artifact: &[u8]) -> Vec<u8> {
    signed_room_labeler_metadata_declaring(publisher, artifact, false)
}

/// [`signed_room_labeler_metadata`] with the module's `needs_attachment_bytes`
/// declaration chosen by the caller.
fn signed_room_labeler_metadata_declaring(
    publisher: &ActorKeypair,
    artifact: &[u8],
    needs_attachment_bytes: bool,
) -> Vec<u8> {
    let meta = fauna_core::scoring::AlgorithmLabeler {
        algorithm_id: publisher.actor_id(),
        version: 1,
        wasm_hash: fauna_core::encoding::content_hash(artifact),
        wasm_size: artifact.len() as u64,
        input_schema: fauna_core::scoring::LabelerInput {
            needs_text: true,
            needs_hashtags: false,
            needs_media_metadata: false,
            needs_author: false,
            needs_attachment_bytes,
        },
        output_schema: fauna_core::scoring::LabelerOutput::default(),
        resource_limits: fauna_core::scoring::ScorerLimits {
            max_memory_bytes: 16 * 1024 * 1024,
            max_cpu_microseconds: 100_000,
        },
        updated_at: fauna_core::data::Timestamp(1_700_000_000_000_000),
        signature: Vec::new(),
    };
    let signed = fauna_core::scoring::sign_labeler_metadata(publisher.signing_key(), meta)
        .expect("the publisher signs its own labeler");
    fauna_core::encoding::canonical_encode(&signed).expect("labeler metadata encodes")
}

/// Publish one artifact into the home nest's labeler registry — the catalog a
/// room names its labelers from — and return the labeler's id.
async fn publish_room_labeler(
    state: &Arc<AppState>,
    artifact: &[u8],
    artifact_kind: &str,
) -> ActorId {
    publish_room_labeler_declaring(state, artifact, artifact_kind, false).await
}

/// [`publish_room_labeler`] with the module's `needs_attachment_bytes`
/// declaration chosen by the caller.
async fn publish_room_labeler_declaring(
    state: &Arc<AppState>,
    artifact: &[u8],
    artifact_kind: &str,
    needs_attachment_bytes: bool,
) -> ActorId {
    let publisher = ActorKeypair::generate();
    fauna_nest::labeler_handlers::publish_labeler_core(
        &state.db,
        &LABELER_PUBLISH_CALLER,
        &signed_room_labeler_metadata_declaring(&publisher, artifact, needs_attachment_bytes),
        artifact,
        "post",
        artifact_kind,
    )
    .await
    .expect("the registry admits the artifact");
    publisher.actor_id()
}

async fn publish_wasm_room_labeler(state: &Arc<AppState>) -> ActorId {
    publish_room_labeler(state, ROOM_LABELER_WAT.as_bytes(), "wasm").await
}

/// The bytes-reading fixture, published with its `needs_attachment_bytes`
/// declaration set.
async fn publish_bytes_room_labeler(state: &Arc<AppState>) -> ActorId {
    publish_room_labeler_declaring(state, MEOW_BYTES_LABELER_WAT.as_bytes(), "wasm", true).await
}

async fn publish_text_model_room_labeler(state: &Arc<AppState>) -> ActorId {
    let artifact = fauna_core::scoring::build_text_model_artifact(
        Some("Small orange cats"),
        6,
        6,
        cat_text_model_vocabulary(),
    )
    .expect("the fixture model is in contract");
    publish_room_labeler(state, &artifact, "text-model").await
}

fn set_labelers_payload(
    room_hex: &str,
    signer: &ActorKeypair,
    version: u64,
    labelers: &[ActorId],
) -> Bytes {
    let signed = fauna_mls::room_policy::RoomLabelers::new(
        room_id_of(room_hex),
        version,
        labelers.iter().copied(),
    )
    .sign(signer)
    .expect("the signer signs a well-formed set");
    Bytes::from(
        encode_canonical(&fauna_protocol::conversations::RoomSetLabelersRequest {
            room_id: room_hex.into(),
            labelers: encode_canonical(&signed).unwrap().to_vec(),
            extra: std::collections::BTreeMap::new(),
        })
        .unwrap()
        .to_vec(),
    )
}

/// The whole channel, read as `caller` — the encoded reply and its decode.
async fn fetch_room_log(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: &ActorKeypair,
    room_hex: &str,
) -> (Bytes, fauna_protocol::conversations::ChannelFetchReply) {
    let req = fauna_protocol::conversations::ChannelFetchRequest {
        channel_id: room_hex.into(),
        after: 0,
        limit: 0,
        nest_url: None,
        extra: std::collections::BTreeMap::new(),
    };
    let bytes = dispatch(
        router,
        state.clone(),
        caller.actor_id().0,
        "fauna.conversations.channel.fetch",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("the channel read is served");
    let reply = decode(&bytes).unwrap();
    (bytes, reply)
}

/// A keyed room, its first generation minted, and — when `labelers` is
/// `Some` — the owner's first labeler set stored.
async fn labelled_room(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    salt: [u8; 32],
    labelers: Option<&[ActorId]>,
) -> (
    RoomCreateReply,
    [u8; 32],
    fauna_core::crypto::GenerationKey,
    [u8; 32],
) {
    let (reply, _owner_recv) = create_keyed_room(router, state, owner, salt).await;
    let room_id = room_id_of(&reply.room_id);
    let (_, key, generation) =
        publish_generation(router, state, &reply.room_id, &room_id, owner).await;
    if let Some(labelers) = labelers {
        let bytes = dispatch(
            router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.set_labelers",
            set_labelers_payload(&reply.room_id, owner, 1, labelers),
        )
        .await
        .expect("the owner names the room's labelers");
        let stored: fauna_protocol::conversations::RoomSetLabelersReply = decode(&bytes).unwrap();
        assert_eq!(stored.labelers_version, 1);
    }
    (reply, room_id, key, generation)
}

async fn send_room_text(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author: &ActorKeypair,
    room_hex: &str,
    key: &fauna_core::crypto::GenerationKey,
    generation: &[u8; 32],
    text: &str,
) {
    dispatch(
        router,
        state.clone(),
        author.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_payload(room_hex, author, key, generation, text),
    )
    .await
    .expect("a floor member's sealed send is stored");
}

#[test]
fn the_room_labeler_fixture_speaks_the_label_abi() {
    // The hand-written BARE bytes in the fixture are a claim; this is the
    // check, through the one execution boundary every position shares.
    let publisher = ActorKeypair::generate();
    let wasm = ROOM_LABELER_WAT.as_bytes();
    let metadata = signed_room_labeler_metadata(&publisher, wasm);
    let input = |text: &str| fauna_core::scoring::LabelerPostInput {
        text: Some(text.into()),
        hashtags: Vec::new(),
        has_media: false,
        media_type: None,
        duration_ms: None,
        author: ActorId([0u8; 32]),
    };
    let hit = fauna_labeler::run_published_labeler(
        &metadata,
        wasm,
        &publisher.actor_id(),
        &input("a cat"),
    )
    .unwrap();
    let got: Vec<(String, u32)> = hit
        .iter()
        .map(|l| (l.category.clone(), (l.confidence * 1000.0).round() as u32))
        .collect();
    assert_eq!(got, vec![("spam".into(), 900), ("cat".into(), 800)]);
    assert!(
        fauna_labeler::run_published_labeler(
            &metadata,
            wasm,
            &publisher.actor_id(),
            &input("the weather")
        )
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn a_room_that_names_a_wasm_and_a_text_model_labeler_labels_each_message_at_send() {
    // Purpose 2, end to end: the room's owner names two transparent labelers,
    // a member sends, and the very first read after the send already carries
    // what they derived — scored before visible (`content-scoring.md`
    // § Timing), because the label pass runs in the act that indexes the
    // message, before the fan-out.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let wasm = publish_wasm_room_labeler(&state).await;
    let model = publish_text_model_room_labeler(&state).await;
    let (reply, _room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x61),
        Some(&[wasm, model]),
    )
    .await;

    let text = "my cat is dozing";
    send_room_text(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        &generation,
        text,
    )
    .await;

    let (bytes, fetched) = fetch_room_log(&router, &state, &owner, &reply.room_id).await;
    assert_eq!(fetched.messages.len(), 1);
    let entry = &fetched.messages[0];

    // The category plane: the wasm labeler's canonical verdict, and only it —
    // its off-list `cat` label is dropped, never rendered as a grey badge.
    assert_eq!(
        entry.labels,
        vec![fauna_core::content_category::ContentLabelEntry {
            category: "spam".into(),
            confidence_per_mille: 900,
        }],
    );

    // The factor plane: one tier-3 row per labeler that ran. The text-model
    // row is the same number the subscriber's client would compute, because
    // it is the same pure scorer (`content-scoring.md` § Don't do these:
    // never fork a scorer by execution position).
    let expected_model =
        fauna_text_model::publish::PublishedTextModel::new(6, 6, cat_text_model_vocabulary())
            .damped_score(text);
    assert!(
        expected_model > 500,
        "the fixture model must read this message as on-topic, or the row proves nothing \
         about the text reaching it (got {expected_model})"
    );
    let mut got: Vec<(String, i64, u8)> = entry
        .scores
        .iter()
        .map(|s| (s.factor.clone(), s.score, s.tier))
        .collect();
    got.sort();
    let mut expected = vec![
        (
            fauna_core::scoring::labeler_factor(&wasm),
            900,
            fauna_core::scoring::TIER_COMMUNITY,
        ),
        (
            fauna_core::scoring::labeler_factor(&model),
            expected_model,
            fauna_core::scoring::TIER_COMMUNITY,
        ),
    ];
    expected.sort();
    assert_eq!(got, expected);

    // Metadata only. The whole encoded reply, so a snippet added later fails
    // here — the envelope beside the labels is ciphertext.
    for word in ["dozing", "my cat"] {
        assert!(
            !bytes.windows(word.len()).any(|w| w == word.as_bytes()),
            "the read must carry no plaintext of the message: {word:?} is in its bytes"
        );
    }
}

#[tokio::test]
async fn a_room_labeler_that_asks_for_bytes_labels_an_attachment_only_message_by_them() {
    // The bytes half of *What the read covers* (`conversation-rooms.md` § The
    // three classes: "labels see the whole message, attachment bytes
    // included"): a labeler that declared `needs_attachment_bytes` receives an
    // attachment's opened plaintext in the act that indexes the message, and
    // an attachment-only message — no caption, nothing a text labeler could
    // read — is labelled by what its picture holds. Beside it, the unflagged
    // cat labeler (which scans its whole input) is handed no bytes at all,
    // though the same picture carries its marker too: a module reads only
    // what it declared (`content-moderation-and-ranking.md` § Tier-3 → *The
    // attachment facet*).
    let (router, state) = router_with_reader_nest_and_posts().await;
    let owner = ActorKeypair::generate();
    let bytes_labeler = publish_bytes_room_labeler(&state).await;
    let text_labeler = publish_wasm_room_labeler(&state).await;
    let (reply, _room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x62),
        Some(&[bytes_labeler, text_labeler]),
    )
    .await;

    // The picture: its plaintext carries both fixtures' markers; the message
    // text carries neither (it is empty — an attachment-only message).
    let plaintext = b"\x89PNG\r\n\x1a\n....meow....cat....IEND".to_vec();
    let sealed = fauna_mls::room_message::seal_room_attachment(&key, &generation, &plaintext)
        .expect("a member seals its attachment under the generation");
    let sealed_cid = fauna_core::encoding::content_hash(&sealed);
    state
        .backup_service
        .as_ref()
        .expect("blob store")
        .local_blob_store()
        .put(&sealed_cid, &sealed)
        .await
        .expect("the sealed attachment uploads first, as a client's does");
    let attachment = fauna_mls::types::ChannelAttachment {
        blob_hash: fauna_core::encoding::content_hash(&plaintext),
        sealed_cid,
        filename: "cat.png".into(),
        mime_type: "image/png".into(),
        size_bytes: plaintext.len() as u64,
        is_image: true,
        epoch: 0,
    };
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_body_payload(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            fauna_mls::types::ChannelMessageBody::Attachments {
                body: String::new(),
                attachments: vec![attachment],
            },
        ),
    )
    .await
    .expect("a floor member's sealed attachments send is stored");

    let (bytes, fetched) = fetch_room_log(&router, &state, &owner, &reply.room_id).await;
    assert_eq!(fetched.messages.len(), 1);
    let entry = &fetched.messages[0];

    // The category plane: the bytes labeler's verdict on the picture, and
    // only it — the text labeler, handed no bytes, saw no `cat`.
    assert_eq!(
        entry.labels,
        vec![fauna_core::content_category::ContentLabelEntry {
            category: "nsfw".into(),
            confidence_per_mille: 700,
        }],
        "the bytes labeler must label the attachment-only message by its picture"
    );

    // The factor plane: both ran (one row each); the bytes one scored the
    // picture, the text one scored nothing.
    let mut got: Vec<(String, i64, u8)> = entry
        .scores
        .iter()
        .map(|s| (s.factor.clone(), s.score, s.tier))
        .collect();
    got.sort();
    let mut expected = vec![
        (
            fauna_core::scoring::labeler_factor(&bytes_labeler),
            700,
            fauna_core::scoring::TIER_COMMUNITY,
        ),
        (
            fauna_core::scoring::labeler_factor(&text_labeler),
            0,
            fauna_core::scoring::TIER_COMMUNITY,
        ),
    ];
    expected.sort();
    assert_eq!(got, expected);

    // No derived view carries bytes (`conversation-rooms.md` § *Forbidden*):
    // the read serves the labels beside the sealed envelope, and neither the
    // picture's bytes nor its marker are anywhere in it.
    for marker in [&b"meow"[..], &b"\x89PNG"[..]] {
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker),
            "the read must carry no attachment plaintext: {marker:?} is in its bytes"
        );
    }
}

/// [`room_send_body_payload`] whose request also carries the plaintext
/// `attachment_refs` a real sender lists beside the envelope — the record's
/// own statement about which sealed blobs it pins
/// (`ChannelSendRequest::attachment_refs`, recorded in
/// `conv_attachment_refs`). Every other room fixture here sends an EMPTY
/// list, which is exactly what a current sender sends for an attachment-less
/// message and the shape the cross-check must not punish.
fn room_send_body_payload_with_refs(
    room_hex: &str,
    author: &ActorKeypair,
    key: &fauna_core::crypto::GenerationKey,
    generation: &[u8; 32],
    body: fauna_mls::types::ChannelMessageBody,
    refs: Vec<String>,
) -> Bytes {
    let sealed = fauna_mls::room_message::seal_room_message(
        key,
        &room_id_of(room_hex),
        generation,
        author,
        1_700_000_000_000,
        body,
    )
    .expect("the author seals its own message under the generation");
    let envelope = fauna_mls::types::ChannelEnvelope::RoomSealed {
        generation: generation.to_vec(),
        ciphertext: sealed,
    };
    let req = fauna_protocol::conversations::ChannelSendRequest {
        channel_id: room_hex.into(),
        envelope: envelope.to_bytes().expect("envelope encodes"),
        expect_no_commit_since: None,
        attachment_refs: refs,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Seal one picture under the room's generation, upload it the way a client
/// does, and return the `ChannelAttachment` that names it. The plaintext
/// carries the bytes-labeler fixture's `cat` marker, so whether the facet
/// served it is readable off the message's labels.
async fn uploaded_room_picture(
    state: &Arc<AppState>,
    key: &fauna_core::crypto::GenerationKey,
    generation: &[u8; 32],
) -> fauna_mls::types::ChannelAttachment {
    let plaintext = b"\x89PNG\r\n\x1a\n....meow....cat....IEND".to_vec();
    let sealed = fauna_mls::room_message::seal_room_attachment(key, generation, &plaintext)
        .expect("a member seals its attachment under the generation");
    let sealed_cid = fauna_core::encoding::content_hash(&sealed);
    state
        .backup_service
        .as_ref()
        .expect("blob store")
        .local_blob_store()
        .put(&sealed_cid, &sealed)
        .await
        .expect("the sealed attachment uploads first, as a client's does");
    fauna_mls::types::ChannelAttachment {
        blob_hash: fauna_core::encoding::content_hash(&plaintext),
        sealed_cid,
        filename: "cat.png".into(),
        mime_type: "image/png".into(),
        size_bytes: plaintext.len() as u64,
        is_image: true,
        epoch: 0,
    }
}

/// Did the bytes labeler see this room's one message's picture? Its verdict is
/// the only thing that can put `nsfw` on the message, and it can only reach it
/// through the attachment facet.
async fn the_picture_reached_the_labeler(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: &ActorKeypair,
    room_hex: &str,
) -> bool {
    let (_bytes, fetched) = fetch_room_log(router, state, owner, room_hex).await;
    fetched
        .messages
        .iter()
        .any(|m| m.labels.iter().any(|l| l.category == "nsfw"))
}

#[tokio::test]
async fn an_attachment_the_record_never_pinned_is_not_read() {
    // The residual row 696 left: the inner attachment list lives inside the SEALED body,
    // so nothing at send time bounds it, and every ceiling the facet applies
    // keys on bytes handed over — which a blob that never opens never costs.
    // The record's own `conv_attachment_refs` is the one plaintext statement
    // this nest made about which blobs the message pins, so an entry outside
    // it is not read at all. The blob here is stored AND opens: the only
    // thing withholding it is the cross-check.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let owner = ActorKeypair::generate();
    let bytes_labeler = publish_bytes_room_labeler(&state).await;
    let (reply, _room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x71),
        Some(&[bytes_labeler]),
    )
    .await;
    let attachment = uploaded_room_picture(&state, &key, &generation).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_body_payload_with_refs(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            fauna_mls::types::ChannelMessageBody::Attachments {
                body: String::new(),
                attachments: vec![attachment],
            },
            // The record pins a DIFFERENT blob than the sealed body names.
            vec![hex::encode([0x99u8; 32])],
        ),
    )
    .await
    .expect("the send is stored either way — a mismatch is not a refusal");

    assert!(
        !the_picture_reached_the_labeler(&router, &state, &owner, &reply.room_id).await,
        "an attachment this nest never recorded the message as pinning must not be opened \
         for the facet"
    );
}

/// A second sealed picture, stored and pinnable exactly as
/// [`uploaded_room_picture`]'s is, but with the nest's blob-metadata row
/// recording a **size over the per-attachment ceiling** while the bytes behind
/// it stay small.
///
/// That combination is what makes the probe observable from outside: the real
/// blob is a valid sealed picture carrying `meow`, so a host that *reads* it
/// admits it and the labeler says `nsfw`. Only a host that asks the nest how
/// big it is first, and believes the answer, withholds it. The production
/// upload route writes this row itself from the bytes it stored
/// (`blob_routes.rs`, which fails an upload rather than ACK one with no
/// durable trace), so a real over-ceiling blob carries a real over-ceiling
/// row; the fixture writes the row by hand only to keep the test's I/O small.
async fn uploaded_room_picture_recorded_oversize(
    state: &Arc<AppState>,
    key: &fauna_core::crypto::GenerationKey,
    generation: &[u8; 32],
) -> fauna_mls::types::ChannelAttachment {
    let plaintext = b"\x89PNG\r\n\x1a\n....meow....oversize....IEND".to_vec();
    let sealed = fauna_mls::room_message::seal_room_attachment(key, generation, &plaintext)
        .expect("a member seals its attachment under the generation");
    let sealed_cid = fauna_core::encoding::content_hash(&sealed);
    state
        .backup_service
        .as_ref()
        .expect("blob store")
        .local_blob_store()
        .put(&sealed_cid, &sealed)
        .await
        .expect("the sealed attachment uploads first, as a client's does");
    state
        .db
        .put_blob_metadata(
            &sealed_cid.digest(),
            // The first value the pre-read cap refuses: a seal is its
            // plaintext plus framing, so anything at or under
            // `MAX + SEALED_FRAMING_ALLOWANCE` could still open to a
            // plaintext within the ceiling and must be read.
            (fauna_labeler::LABELER_ATTACHMENT_BYTES_MAX
                + fauna_labeler::SEALED_FRAMING_ALLOWANCE
                + 1) as i64,
            "image/png",
            None,
            None,
        )
        .await
        .expect("the upload route records what it stored");
    fauna_mls::types::ChannelAttachment {
        blob_hash: fauna_core::encoding::content_hash(&plaintext),
        sealed_cid,
        filename: "oversize.png".into(),
        mime_type: "image/png".into(),
        // The author declares a few bytes — the cheap pre-filter must not be
        // what withholds this one, or the test would prove nothing about the
        // probe.
        size_bytes: plaintext.len() as u64,
        is_image: true,
        epoch: 0,
    }
}

#[tokio::test]
async fn an_attachment_the_nest_recorded_as_oversize_is_never_read() {
    // Rule (4)'s "a candidate that cannot be admitted is never fetched and
    // never decrypted", over the real `RoomFacetSource`. Until the sealed-length
    // probe landed, the sealed-length refusals ran only AFTER `store.get` had
    // read the whole blob, so an author naming distinct pinned uploads that
    // each declare a few bytes over blobs at the upload ceiling was charged a
    // full read for every one of them, inline in the send path.
    //
    // Discriminating by construction: this blob is stored, it is pinned by the
    // record, and it opens — its plaintext carries `meow`, the one thing that
    // makes this labeler say `nsfw`. A host that reads it therefore LABELS the
    // message. The label's absence is only reachable by asking the nest for
    // the stored size and believing it.
    //
    // So this is the conformance witness that removing the probe reddens —
    // measured, not inferred: with the loop's probe made never to answer, this
    // case fails while its two siblings stay green. What a label cannot see is a host that reads the blob
    // and THEN refuses it from the size row; the counted tests beside
    // `RoomFacetSource` (`src/conversations_handlers.rs`) pin that shape.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let owner = ActorKeypair::generate();
    let bytes_labeler = publish_bytes_room_labeler(&state).await;
    let (reply, _room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x73),
        Some(&[bytes_labeler]),
    )
    .await;
    let oversize = uploaded_room_picture_recorded_oversize(&state, &key, &generation).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_body_payload_with_refs(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            fauna_mls::types::ChannelMessageBody::Attachments {
                body: String::new(),
                attachments: vec![oversize.clone()],
            },
            // Pinned, so the record cross-check admits it: the probe is the
            // only bound left standing between it and a read.
            vec![hex::encode(oversize.sealed_cid.digest())],
        ),
    )
    .await
    .expect("the send is stored either way — an oversize attachment is not a refusal");

    assert!(
        !the_picture_reached_the_labeler(&router, &state, &owner, &reply.room_id).await,
        "a blob the nest itself recorded as past the per-attachment ceiling must not be \
         read for the facet — and had it been read, its `meow` would have labelled this \
         message"
    );
}

#[tokio::test]
async fn a_many_entry_sealed_body_still_serves_the_attachment_the_nest_never_measured() {
    // The many-entry case over the real `RoomFacetSource` — the conformance
    // shape that never existed, every earlier case here carrying exactly one
    // attachment.
    //
    // It pins the probe's ⚠ rule end to end: **an absent answer means "no
    // answer", never "refuse".** This fixture stores its picture straight into
    // the blob store, as a relay or backup write path does and as every
    // attachment case here has always done, so the nest holds NO metadata row
    // for it — and the honest facet must survive that, at a candidate count
    // past the per-item ceiling. Read the other way: had absence been taken
    // for a refusal, this assertion and every sibling attachment case would
    // have gone dark together.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let owner = ActorKeypair::generate();
    let bytes_labeler = publish_bytes_room_labeler(&state).await;
    let (reply, _room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x74),
        Some(&[bytes_labeler]),
    )
    .await;
    let picture = uploaded_room_picture(&state, &key, &generation).await;
    assert!(
        state
            .db
            .get_blob_sizes(&[picture.sealed_cid.digest()])
            .await
            .expect("the size query runs")
            .is_empty(),
        "fixture: this blob has no metadata row, so the probe has no answer for it"
    );

    // The picture first, then junk past the per-item candidate ceiling: the
    // author writes this list inside the sealed body, so nothing at send time
    // bounds its length.
    let mut attachments = vec![picture.clone()];
    for i in 0..(fauna_labeler::LABELER_ATTACHMENT_FACET_MAX_CANDIDATES + 8) {
        let filler = format!("never-stored-{i}").into_bytes();
        attachments.push(fauna_mls::types::ChannelAttachment {
            blob_hash: fauna_core::encoding::content_hash(&filler),
            sealed_cid: fauna_core::encoding::content_hash(&filler),
            filename: format!("junk-{i}.png"),
            mime_type: "image/png".into(),
            size_bytes: filler.len() as u64,
            is_image: true,
            epoch: 0,
        });
    }
    // The sealed body is unbounded; the RECORD's refs list is not — the nest
    // caps it at `MAX_ATTACHMENT_REFS_PER_RECORD`, pinned equal to the
    // candidate ceiling by a static assertion. So a record cannot even name
    // the tail, which is the reason that ceiling is the number it is.
    let refs: Vec<String> = attachments
        .iter()
        .take(fauna_labeler::LABELER_ATTACHMENT_FACET_MAX_CANDIDATES)
        .map(|a| hex::encode(a.sealed_cid.digest()))
        .collect();

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_body_payload_with_refs(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            fauna_mls::types::ChannelMessageBody::Attachments {
                body: String::new(),
                attachments: attachments.clone(),
            },
            refs,
        ),
    )
    .await
    .expect("a long attachment list is not a refusal");

    assert!(
        the_picture_reached_the_labeler(&router, &state, &owner, &reply.room_id).await,
        "the one real picture still reaches the labeler from a body naming {} \
         attachments — an unmeasured blob is read, and the ceiling withholds only \
         the tail",
        attachments.len()
    );
}

#[tokio::test]
async fn an_attachment_the_record_pinned_is_read_as_before() {
    // The control the test above needs: the same message, the same picture,
    // the same labeler — and the record pinning the address the body names.
    // Without this pair, an implementation that withheld everything would
    // pass the test above.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let owner = ActorKeypair::generate();
    let bytes_labeler = publish_bytes_room_labeler(&state).await;
    let (reply, _room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x72),
        Some(&[bytes_labeler]),
    )
    .await;
    let attachment = uploaded_room_picture(&state, &key, &generation).await;
    let pinned = hex::encode(attachment.sealed_cid.digest());

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_body_payload_with_refs(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            fauna_mls::types::ChannelMessageBody::Attachments {
                body: String::new(),
                attachments: vec![attachment],
            },
            vec![pinned],
        ),
    )
    .await
    .expect("a floor member's sealed attachments send is stored");

    assert!(
        the_picture_reached_the_labeler(&router, &state, &owner, &reply.room_id).await,
        "a pinned attachment must reach a declaring labeler exactly as before"
    );
}

#[tokio::test]
async fn a_record_that_pinned_nothing_is_bounded_by_the_ceilings_alone() {
    // ⚠ The rule that keeps the cross-check from breaking the honest case:
    // `attachment_refs` is an ADDITIVE wire field a sender may omit, and the nest
    // cannot see inside the envelope, so an attachment-less message records no
    // refs at all. An empty
    // set means "no record to check against", never "nothing was pinned" —
    // read the other way, every such message would silently lose its bytes,
    // which is the feature's own most common shape today (every other room
    // fixture in this file sends an empty list).
    let (router, state) = router_with_reader_nest_and_posts().await;
    let owner = ActorKeypair::generate();
    let bytes_labeler = publish_bytes_room_labeler(&state).await;
    let (reply, _room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x73),
        Some(&[bytes_labeler]),
    )
    .await;
    let attachment = uploaded_room_picture(&state, &key, &generation).await;

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.channel.send",
        room_send_body_payload_with_refs(
            &reply.room_id,
            &owner,
            &key,
            &generation,
            fauna_mls::types::ChannelMessageBody::Attachments {
                body: String::new(),
                attachments: vec![attachment],
            },
            Vec::new(),
        ),
    )
    .await
    .expect("a send with no refs is an ordinary send");

    assert!(
        the_picture_reached_the_labeler(&router, &state, &owner, &reply.room_id).await,
        "a message whose record pinned nothing must keep its facet — the ceilings are its \
         only bound"
    );
}

#[tokio::test]
async fn a_room_labeler_that_asks_for_bytes_labels_a_room_posts_picture() {
    // The message case's twin on the post rail. Purpose 3 derives "the same
    // kinds of view as for a message … under the same rule"
    // (`community-rooms.md` § The three classes → *What the home nest does
    // with its read*), and *What the read covers* has labels see the whole
    // item, attachment bytes included — but a room post's media seal under the
    // post's OWN per-post key rather than as room attachments, so the pass
    // opens them with the key it already holds. One bounding loop
    // (`fauna_labeler::build_attachment_facet`), two key models: a declaring
    // labeler reads a post's picture exactly as it reads a message's, and the
    // unflagged cat labeler beside it is handed no bytes at all, though the
    // same picture carries its marker too.
    //
    // The caption carries neither fixture's marker, so every verdict below
    // comes from the picture. The text-less shapes — a picture with no
    // caption at all, a picture with alt text — are the two cases after this.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let owner = ActorKeypair::generate();
    let bytes_labeler = publish_bytes_room_labeler(&state).await;
    let text_labeler = publish_wasm_room_labeler(&state).await;
    let (reply, room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x72),
        Some(&[bytes_labeler, text_labeler]),
    )
    .await;

    let seal_id = fauna_client_core::post::mint_seal_id().unwrap();
    let (item, _plaintext) = upload_sealed_room_picture(&state, &key, seal_id).await;
    let (post_id, _) = create_room_post_sealed(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        seal_id,
        fauna_core::data::PostBody::TextWithMedia {
            content: "look at this".into(),
            facets: vec![],
            items: vec![item],
        },
    )
    .await;

    let (bytes, read) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert_eq!(read.posts.len(), 1, "the post was indexed and labelled");
    let entry = &read.posts[0];

    // The category plane: the bytes labeler's verdict on the picture, and only
    // it — the text labeler, handed no bytes, saw no `cat` in the caption.
    assert_eq!(
        entry.labels,
        vec![fauna_core::content_category::ContentLabelEntry {
            category: "nsfw".into(),
            confidence_per_mille: 700,
        }],
        "the declaring labeler must label the post by its picture"
    );

    // The factor plane: both ran (one row each); the bytes one scored the
    // picture, the text one scored nothing.
    let mut got: Vec<(String, i64, u8)> = entry
        .scores
        .iter()
        .map(|s| (s.factor.clone(), s.score, s.tier))
        .collect();
    got.sort();
    let mut expected = vec![
        (
            fauna_core::scoring::labeler_factor(&bytes_labeler),
            700,
            fauna_core::scoring::TIER_COMMUNITY,
        ),
        (
            fauna_core::scoring::labeler_factor(&text_labeler),
            0,
            fauna_core::scoring::TIER_COMMUNITY,
        ),
    ];
    expected.sort();
    assert_eq!(got, expected);

    // No derived view carries bytes (`community-rooms.md` § *Forbidden*): the
    // verdict read carries neither the picture's marker nor its header, and
    // the room's post corpus holds only what the member typed.
    for marker in [&b"meow"[..], &b"\x89PNG"[..]] {
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker),
            "the read must carry no attachment plaintext: {marker:?} is in its bytes"
        );
    }
    assert_eq!(
        state.db.count_room_post_views(&room_id).await.unwrap(),
        1,
        "one post view, as for any captioned room post"
    );
}

#[tokio::test]
async fn a_room_labeler_that_asks_for_bytes_labels_a_media_only_room_post_by_its_picture() {
    // The case the captioned twin above cannot cover: a post whose body is
    // ONLY a picture — no caption, no alt text — carries nothing search may
    // index (*What the read covers*: never from attachment bytes), yet
    // purpose 3 derives "the same kinds of view as for a message", and a
    // picture-only MESSAGE is labelled by its bytes. Ruled: the `(room, post)` map row records that the nest derived a
    // view of the post — ANY view — so it is written whether or not an index
    // row is, and the two consumers keyed on it, the floor-gated verdict read
    // and the delete purge, find a text-less post exactly as a captioned one.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let owner = ActorKeypair::generate();
    let bytes_labeler = publish_bytes_room_labeler(&state).await;
    let text_labeler = publish_wasm_room_labeler(&state).await;
    let (reply, room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x73),
        Some(&[bytes_labeler, text_labeler]),
    )
    .await;
    let seal_id = fauna_client_core::post::mint_seal_id().unwrap();
    let (item, _plaintext) = upload_sealed_room_picture(&state, &key, seal_id).await;
    let (post_id, _) = create_room_post_sealed(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        seal_id,
        fauna_core::data::PostBody::Media {
            items: vec![item],
            alt_text: None,
        },
    )
    .await;

    // Viewed: one map row, though the corpus holds nothing for it. The ruled
    // shape is a map row with no document, and the search half's rule — a
    // hit the map cannot resolve is silently dropped — is never reached: no
    // token was indexed, so no query produces a hit to resolve.
    assert_eq!(
        state.db.count_room_post_views(&room_id).await.unwrap(),
        1,
        "a text-less post is a viewed post"
    );
    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload_with(&reply.room_id, "meow", true),
    )
    .await
    .expect("a live floor member searches, posts included");
    let found: RoomSearchReply = decode(&bytes).unwrap();
    assert!(
        found.hits.is_empty(),
        "nothing typed, nothing searchable: {found:?}"
    );

    // Labelled, by the picture alone: the declaring labeler's verdict, and the
    // text labeler beside it — handed no bytes and no text — saw nothing.
    let (_, read) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert_eq!(
        read.posts.len(),
        1,
        "the media-only post's verdicts are served to a floor member"
    );
    assert_eq!(
        read.posts[0].labels,
        vec![fauna_core::content_category::ContentLabelEntry {
            category: "nsfw".into(),
            confidence_per_mille: 700,
        }],
        "the declaring labeler must label the post by its picture"
    );
    assert_eq!(
        state.db.count_room_post_bus_rows(&room_id).await.unwrap(),
        3,
        "one category row and two factor rows"
    );

    // And purged with the post, through the same map row (the consumer the
    // ruling is about): the verdicts, the map row, and the served read.
    delete_room_post(&router, &state, &owner, post_id).await;
    assert_eq!(
        state.db.count_room_post_bus_rows(&room_id).await.unwrap(),
        0,
        "the post's verdicts go with the post"
    );
    assert_eq!(state.db.count_room_post_views(&room_id).await.unwrap(), 0);
    let (_, read) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert!(read.posts.is_empty());
}

#[tokio::test]
async fn a_media_only_room_posts_alt_text_is_the_text_the_member_typed() {
    // The other media-only shape: a picture with alt text. Alt text is what
    // the member typed — `PostBody::text` is the one home for which variants
    // carry typed text, and it names a media post's alt text and a structured
    // post's content beside a caption — so it is indexed like a caption
    // (*What the read covers*: what a member typed, never the bytes), and the
    // post is labelled by its picture like any other.
    let (router, state) = router_with_reader_nest_and_posts().await;
    let owner = ActorKeypair::generate();
    let bytes_labeler = publish_bytes_room_labeler(&state).await;
    let (reply, room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x75),
        Some(&[bytes_labeler]),
    )
    .await;
    let seal_id = fauna_client_core::post::mint_seal_id().unwrap();
    let (item, _plaintext) = upload_sealed_room_picture(&state, &key, seal_id).await;
    let (post_id, _) = create_room_post_sealed(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        generation,
        seal_id,
        fauna_core::data::PostBody::Media {
            items: vec![item],
            alt_text: Some("a hedgehog on the porch".into()),
        },
    )
    .await;

    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.search",
        room_search_payload_with(&reply.room_id, "hedgehog", true),
    )
    .await
    .expect("a live floor member searches, posts included");
    let found: RoomSearchReply = decode(&bytes).unwrap();
    assert_eq!(found.hits.len(), 1, "the alt text is searchable: {found:?}");
    assert_eq!(
        found.hits[0].kind,
        fauna_protocol::conversations::RoomSearchHitKind::Post
    );
    assert_eq!(
        found.hits[0].post_id.as_deref(),
        Some(hex::encode(post_id).as_str())
    );
    assert_eq!(state.db.count_room_post_views(&room_id).await.unwrap(), 1);

    let (_, read) = read_room_labels(&router, &state, &owner, &[post_id]).await;
    assert_eq!(read.posts.len(), 1);
    assert_eq!(
        read.posts[0].labels,
        vec![fauna_core::content_category::ContentLabelEntry {
            category: "nsfw".into(),
            confidence_per_mille: 700,
        }],
        "the declaring labeler labels the post by its picture, alt text or not"
    );
}

#[tokio::test]
async fn a_room_that_names_no_labeler_derives_no_labels() {
    // "No entry, no labels": a labeler being published on this nest is not a
    // licence to run it over a room — only the room's own signed choice is.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let wasm = publish_wasm_room_labeler(&state).await;
    let _model = publish_text_model_room_labeler(&state).await;
    let (reply, room_id, key, generation) =
        labelled_room(&router, &state, &owner, birth_salt(0x62), None).await;

    send_room_text(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        &generation,
        "a cat",
    )
    .await;
    let (_, fetched) = fetch_room_log(&router, &state, &owner, &reply.room_id).await;
    assert!(fetched.messages[0].labels.is_empty());
    assert!(fetched.messages[0].scores.is_empty());
    assert_eq!(
        state
            .db
            .count_room_message_bus_rows(&room_id)
            .await
            .unwrap(),
        0,
        "the search index is built; the label plane is not"
    );
    assert_eq!(
        room_view_hits(&state, &room_id, "cat").await,
        1,
        "labels are a purpose of their own — search runs either way"
    );

    // An empty set is a real one: named, then withdrawn, labels nothing new.
    for (version, set) in [(1u64, vec![wasm]), (2, Vec::new())] {
        dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.set_labelers",
            set_labelers_payload(&reply.room_id, &owner, version, &set),
        )
        .await
        .expect("the owner changes the room's set");
    }
    send_room_text(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        &generation,
        "another cat",
    )
    .await;
    assert_eq!(
        state
            .db
            .count_room_message_bus_rows(&room_id)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn a_labeler_set_naming_anything_the_nest_would_not_run_is_refused() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let wasm = publish_wasm_room_labeler(&state).await;
    let list = publish_room_labeler(
        &state,
        &fauna_core::scoring::build_list_artifact(None, vec![([7u8; 32], 900)]).unwrap(),
        "list",
    )
    .await;
    let (reply, room_id, _, _) =
        labelled_room(&router, &state, &owner, birth_salt(0x63), None).await;

    for (labelers, why) in [
        // A list is keyed by post id: it has nothing to say about a message.
        (vec![list], "a list labeler scores no message"),
        // A user's own model is sealed under its owner's key and never
        // published, so nothing it could be named by resolves to a
        // transparent artifact.
        (
            vec![owner.actor_id()],
            "an unpublished model is not transparent",
        ),
        (
            vec![wasm, owner.actor_id()],
            "one bad id refuses the whole set",
        ),
    ] {
        let err = dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.set_labelers",
            set_labelers_payload(&reply.room_id, &owner, 1, &labelers),
        )
        .await
        .expect_err(why);
        assert_eq!(err.code, "fauna.conversations.invalid_params", "{why}");
    }

    // Rule 6: the owner or an admin — a plain member does not choose what
    // reads the room.
    let member = ActorKeypair::generate();
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;
    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.set_labelers",
        set_labelers_payload(&reply.room_id, &member, 1, &[wasm]),
    )
    .await
    .expect_err("a plain member names no labeler");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // Signed by one principal, sent by another: the nest stores what the
    // caller signed or nothing.
    let err = dispatch(
        &router,
        state.clone(),
        member.actor_id().0,
        "fauna.conversations.room.set_labelers",
        set_labelers_payload(&reply.room_id, &owner, 1, &[wasm]),
    )
    .await
    .expect_err("a set is signed by the principal making it");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    // The strict ratchet: the first set is version 1, then exactly +1.
    for bad in [2u64, 5] {
        let err = dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.set_labelers",
            set_labelers_payload(&reply.room_id, &owner, bad, &[wasm]),
        )
        .await
        .expect_err("a set is exactly one version on");
        assert_eq!(err.code, "fauna.conversations.invalid_params");
    }

    // And an admin may.
    let admin = ActorKeypair::generate();
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &admin).await;
    appoint_admin(&router, &state, &owner, &reply.room_id, &room_id, &admin).await;
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.set_labelers",
        set_labelers_payload(&reply.room_id, &admin, 1, &[wasm]),
    )
    .await
    .expect("an admin names the room's labelers");
}

#[tokio::test]
async fn a_labeler_set_is_refused_on_a_room_whose_authority_is_its_mls_group() {
    // An end-to-end room's nest reads nothing, so it has nothing to label; a
    // set stored there would be a promise the class cannot keep.
    let (router, state) = router_with_db_only().await;
    let owner = ActorKeypair::generate();
    let room_id = [0x64u8; 32];
    let room_hex = hex::encode(room_id);
    seat_on_routing_roster(&state, &owner, &room_id).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&room_hex, vec![entry(&owner, "owner")]),
    )
    .await
    .expect("a member-reported roster");
    let wasm = publish_wasm_room_labeler(&state).await;
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_labelers",
        set_labelers_payload(&room_hex, &owner, 1, &[wasm]),
    )
    .await
    .expect_err("no labeler set on a room the nest cannot read");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

#[tokio::test]
async fn rotating_the_nest_out_deletes_every_label_row_it_derived() {
    // The revoke covers the label plane in the SAME act as the search index —
    // "every table of it" (§ *What the home nest does with its read*). Counted
    // through the store rather than through a read, because a read after a
    // revoke answers empty whether or not the rows survived it.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let wasm = publish_wasm_room_labeler(&state).await;
    let model = publish_text_model_room_labeler(&state).await;
    let (reply, room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x65),
        Some(&[wasm, model]),
    )
    .await;
    send_room_text(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        &generation,
        "a cat dozing",
    )
    .await;
    assert_eq!(
        state
            .db
            .count_room_message_bus_rows(&room_id)
            .await
            .unwrap(),
        3,
        "one category row and two factor rows"
    );

    let mut targets = wrap_targets(&state, &room_id).await;
    targets.retain(|t| t.member_actor.0 == owner.actor_id().0);
    let (mint, key2, generation2) = build_mint(&state, &room_id, &owner, &targets).await;
    let bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&reply.room_id, &mint),
    )
    .await
    .expect("a mint may leave the nest out — that is the revoke");
    let published: RoomPublishGenerationReply = decode(&bytes).unwrap();
    assert!(published.nest_read_revoked);
    assert_eq!(
        state
            .db
            .count_room_message_bus_rows(&room_id)
            .await
            .unwrap(),
        0,
        "the revoke deletes the label rows, not only the searchable half"
    );

    send_room_text(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key2,
        &generation2,
        "a cat again",
    )
    .await;
    assert_eq!(
        state
            .db
            .count_room_message_bus_rows(&room_id)
            .await
            .unwrap(),
        0,
        "and a nest the members rotated out labels nothing new"
    );
    let (_, fetched) = fetch_room_log(&router, &state, &owner, &reply.room_id).await;
    assert!(
        fetched
            .messages
            .iter()
            .all(|m| m.labels.is_empty() && m.scores.is_empty())
    );
}

#[tokio::test]
async fn a_caller_off_the_floor_reads_the_envelopes_and_no_labels() {
    // The channel read is deliberately not floor-gated — the envelope is
    // sealed. A verdict is not: it was derived from the plaintext, so it
    // reaches a live floor member and nobody else.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let wasm = publish_wasm_room_labeler(&state).await;
    let (reply, room_id, key, generation) =
        labelled_room(&router, &state, &owner, birth_salt(0x66), Some(&[wasm])).await;
    let member = ActorKeypair::generate();
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &member).await;
    send_room_text(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        &generation,
        "a cat",
    )
    .await;

    let (_, seen_by_member) = fetch_room_log(&router, &state, &member, &reply.room_id).await;
    assert_eq!(
        seen_by_member.messages[0].labels.len(),
        1,
        "a live member of any rank reads the labels"
    );

    let stranger = ActorKeypair::generate();
    let (_, seen_by_stranger) = fetch_room_log(&router, &state, &stranger, &reply.room_id).await;
    assert_eq!(
        seen_by_stranger.messages.len(),
        1,
        "the sealed envelope is served"
    );
    assert!(seen_by_stranger.messages[0].labels.is_empty());
    assert!(seen_by_stranger.messages[0].scores.is_empty());

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&reply.room_id, &member),
    )
    .await
    .expect("the owner removes the member");
    let (_, seen_after_removal) = fetch_room_log(&router, &state, &member, &reply.room_id).await;
    assert!(
        seen_after_removal.messages[0].labels.is_empty(),
        "the floor is live: a removed member reads no more labels"
    );
}

// ── the verdicts on a foreign member's relayed read ─────────────
//
// `community-rooms.md` § The three classes → *What the home nest does with
// its read*, purpose 2, serves the result beside the message to live floor
// members — wherever homed: `conversation-rooms.md` § The home nest has a
// member on a foreign nest reach the room only through its own nest, which
// relays the drain as `fauna.federation.channel.fetch`. So that reply
// carries the verdicts too, gated on the actor the relay names — never on
// the relaying nest. These drive the room home's serving half with the
// verified `origin_nest_id` the federation channel supplies; the two-nest
// journey through the relaying nest and the client seam is
// `conformance_cross_nest_conversations_client.rs`.

/// A member's drain as its own nest relays it: the room home's
/// `fauna.federation.channel.fetch`, dispatched through the registered
/// federation table — the encoded reply and its decode.
async fn fetch_room_log_relayed(
    state: &Arc<AppState>,
    origin_nest_id: [u8; 32],
    reader: &ActorKeypair,
    room_hex: &str,
) -> (Bytes, fauna_nest::federation_handlers::FedChannelFetchReply) {
    let req = fauna_nest::federation_handlers::FedChannelFetchRequest {
        requesting_actor_id: hex::encode(reader.actor_id().0),
        channel_id: room_hex.into(),
        after: 0,
        limit: 0,
        requesting_handle: None,
        requesting_domain: None,
    };
    let bytes = dispatch_federation_kind(
        state,
        "fauna.federation.channel.fetch",
        origin_nest_id,
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("the room home serves a bound foreign actor's drain");
    let reply = decode(&bytes).unwrap();
    (bytes, reply)
}

/// The refusal half of [`fetch_room_log_relayed`] — the same relayed drain,
/// for a reader the room home does NOT admit. Answers the wire error, whose
/// `code` is the contractual half (the rendered sentence is localized and
/// carries no wire vocabulary by design).
async fn fetch_room_log_relayed_err(
    state: &Arc<AppState>,
    origin_nest_id: [u8; 32],
    reader: &ActorKeypair,
    room_hex: &str,
) -> fauna_protocol::RpcError {
    let req = fauna_nest::federation_handlers::FedChannelFetchRequest {
        requesting_actor_id: hex::encode(reader.actor_id().0),
        channel_id: room_hex.into(),
        after: 0,
        limit: 0,
        requesting_handle: None,
        requesting_domain: None,
    };
    dispatch_federation_kind(
        state,
        "fauna.federation.channel.fetch",
        origin_nest_id,
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("the room home refuses a drain it does not admit")
}

/// Record `who` as a foreign member of `room_id`'s channel homed on
/// `their_nest` — the row the room home writes when it relays that member's
/// Welcome, and the one `require_foreign_member` checks. A binding is not a
/// seat: it admits the drain, and the floor alone decides the verdicts.
async fn bind_foreign_member(
    state: &Arc<AppState>,
    room_id: &[u8; 32],
    who: &ActorKeypair,
    their_nest: [u8; 32],
) {
    state
        .db
        .register_foreign_channel_member(
            room_id,
            &who.actor_id().0,
            &their_nest,
            None,
            fauna_nest::db::channels::RebindPower::InsertOnly,
        )
        .await
        .expect("the foreign membership binding is recorded");
}

/// A page's factor rows as sortable tuples, for comparing two reads.
fn score_rows(scores: &[fauna_core::scoring::ScoreEntry]) -> Vec<(String, i64, u8)> {
    let mut rows: Vec<(String, i64, u8)> = scores
        .iter()
        .map(|s| (s.factor.clone(), s.score, s.tier))
        .collect();
    rows.sort();
    rows
}

#[tokio::test]
async fn a_foreign_member_on_the_floor_reads_the_verdicts_through_its_own_nests_relay() {
    // Purpose 2 reaches a member wherever it is homed: the drain its own nest
    // relays carries both planes the same-nest read serves a member homed
    // here — the same verdicts, and, as there, metadata only.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let wasm = publish_wasm_room_labeler(&state).await;
    let model = publish_text_model_room_labeler(&state).await;
    let (reply, room_id, key, generation) = labelled_room(
        &router,
        &state,
        &owner,
        birth_salt(0x67),
        Some(&[wasm, model]),
    )
    .await;
    let abroad = ActorKeypair::generate();
    let their_nest = [0xC7u8; 32];
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &abroad).await;
    bind_foreign_member(&state, &room_id, &abroad, their_nest).await;

    let text = "my cat is dozing";
    send_room_text(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        &generation,
        text,
    )
    .await;

    let (_, at_home) = fetch_room_log(&router, &state, &owner, &reply.room_id).await;
    let (bytes, relayed) =
        fetch_room_log_relayed(&state, their_nest, &abroad, &reply.room_id).await;
    assert_eq!(relayed.messages.len(), 1);
    let entry = &relayed.messages[0];

    assert_eq!(
        entry.labels,
        vec![fauna_core::content_category::ContentLabelEntry {
            category: "spam".into(),
            confidence_per_mille: 900,
        }],
        "the category plane crosses to the member's own nest"
    );
    assert_eq!(
        entry.labels, at_home.messages[0].labels,
        "the same verdicts a member homed here reads"
    );
    assert_eq!(
        score_rows(&entry.scores).len(),
        2,
        "one factor row per labeler that ran — the text model's reaches a \
         member nowhere else"
    );
    assert_eq!(
        score_rows(&entry.scores),
        score_rows(&at_home.messages[0].scores),
        "and the same factor rows"
    );

    // Metadata only: the relaying nest holds ciphertext and verdicts, never
    // the text they were derived from.
    for word in ["dozing", "my cat"] {
        assert!(
            !bytes.windows(word.len()).any(|w| w == word.as_bytes()),
            "the relayed read must carry no plaintext of the message: {word:?} is in its bytes"
        );
    }
}

#[tokio::test]
async fn a_bound_actor_off_the_floor_drains_the_envelopes_and_no_verdicts() {
    // The relay's verdict gate is the floor, keyed on the actor it names. A
    // channel binding is not a seat: an actor can hold one it was never seated
    // for, and its drain still serves the sealed envelope — as the same-nest
    // read serves any routing-roster caller — with no verdict.
    //
    // A REMOVAL is the other case, and it is not that one: since the S8 purge
    // (`conversations_handlers::room_remove_handler`) a removal ends the
    // binding along with the seat, so the removed member's drain is REFUSED
    // rather than served-without-verdicts. Both halves are pinned below,
    // because it is the contrast that carries the meaning: an unseated
    // stranger reads ciphertext, a removed member reads nothing.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let wasm = publish_wasm_room_labeler(&state).await;
    let (reply, room_id, key, generation) =
        labelled_room(&router, &state, &owner, birth_salt(0x68), Some(&[wasm])).await;
    let their_nest = [0xC8u8; 32];
    let abroad = ActorKeypair::generate();
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &abroad).await;
    bind_foreign_member(&state, &room_id, &abroad, their_nest).await;
    let never_seated = ActorKeypair::generate();
    bind_foreign_member(&state, &room_id, &never_seated, their_nest).await;
    send_room_text(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        &generation,
        "a cat",
    )
    .await;

    let (_, seated) = fetch_room_log_relayed(&state, their_nest, &abroad, &reply.room_id).await;
    assert_eq!(
        seated.messages[0].labels.len(),
        1,
        "a seated member's relayed read carries the verdict — the contrast \
         the two refusals below are measured against"
    );

    let (_, unseated) =
        fetch_room_log_relayed(&state, their_nest, &never_seated, &reply.room_id).await;
    assert_eq!(unseated.messages.len(), 1, "the sealed envelope is served");
    assert!(!unseated.messages[0].envelope.is_empty());
    assert!(
        unseated.messages[0].labels.is_empty() && unseated.messages[0].scores.is_empty(),
        "a binding with no seat reads no verdict"
    );

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.remove",
        remove_payload(&reply.room_id, &abroad),
    )
    .await
    .expect("the owner removes the member");
    let refused = fetch_room_log_relayed_err(&state, their_nest, &abroad, &reply.room_id).await;
    assert_eq!(
        refused.code, "fauna.federation.forbidden",
        "the removal ended the BINDING too (S8: the fetch authorization dies \
         with the membership), so the drain is refused outright — where the \
         never-seated stranger above, whose binding nobody revoked, is still \
         served the ciphertext"
    );
    assert!(
        state
            .db
            .foreign_member_binding(&room_id, &abroad.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "and the row itself is gone, so every door gated on it alone — the \
         roster read, the write-token mint, `channel.actors` — is shut with it"
    );
}

#[tokio::test]
async fn no_verdict_crosses_the_relay_beside_a_withheld_envelope() {
    // A verdict is derived from the content, so it goes where the content
    // goes: a message taken down under a legal obligation reaches a member
    // homed elsewhere as the same tombstone a member homed here reads, and
    // its verdicts stay behind on both doors.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let wasm = publish_wasm_room_labeler(&state).await;
    let (reply, room_id, key, generation) =
        labelled_room(&router, &state, &owner, birth_salt(0x69), Some(&[wasm])).await;
    let their_nest = [0xC9u8; 32];
    let abroad = ActorKeypair::generate();
    seat_keyed_member(&router, &state, &owner, &reply.room_id, &room_id, &abroad).await;
    bind_foreign_member(&state, &room_id, &abroad, their_nest).await;
    send_room_text(
        &router,
        &state,
        &owner,
        &reply.room_id,
        &key,
        &generation,
        "a cat",
    )
    .await;

    let (_, before) = fetch_room_log_relayed(&state, their_nest, &abroad, &reply.room_id).await;
    assert_eq!(
        before.messages[0].labels.len(),
        1,
        "before the takedown the relayed read carries the verdict"
    );

    let stored = before.messages[0].envelope.clone();
    let cid = fauna_mls::segments::derive_record_cid(&stored).expect("derive conv record cid");
    let n = state
        .db
        .set_conv_legal_takedown(&cid, Some("EU-DSA-2024/686"))
        .await
        .expect("set takedown");
    assert_eq!(
        n, 1,
        "precondition: exactly the room's one message is taken down"
    );

    let (_, after) = fetch_room_log_relayed(&state, their_nest, &abroad, &reply.room_id).await;
    let entry = &after.messages[0];
    assert!(entry.envelope.is_empty(), "the sealed body is withheld");
    assert_eq!(
        entry.legal_takedown.as_ref().map(|t| t.reference.as_str()),
        Some("EU-DSA-2024/686"),
        "and the tombstone reference is served in its place"
    );
    assert!(
        entry.labels.is_empty() && entry.scores.is_empty(),
        "no verdict travels beside a withheld envelope"
    );

    let (_, at_home) = fetch_room_log(&router, &state, &owner, &reply.room_id).await;
    assert!(
        at_home.messages[0].labels.is_empty() && at_home.messages[0].scores.is_empty(),
        "the same-nest door withholds them identically"
    );
}

#[tokio::test]
async fn the_roster_read_serves_the_signed_labeler_set() {
    // What members verify and render — the consent surface must say what
    // reads the room — and what the next change is authored from.
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let wasm = publish_wasm_room_labeler(&state).await;
    let (labelled, _, _, _) =
        labelled_room(&router, &state, &owner, birth_salt(0x67), Some(&[wasm])).await;
    let (bare, _, _, _) = labelled_room(&router, &state, &owner, birth_salt(0x68), None).await;

    let mut served = Vec::new();
    for room_hex in [&labelled.room_id, &bare.room_id] {
        let bytes = dispatch(
            &router,
            state.clone(),
            owner.actor_id().0,
            "fauna.conversations.room.list_roster",
            Bytes::from(
                encode_canonical(&RoomListRosterRequest {
                    room_id: room_hex.clone(),
                    at_policy_version: None,
                    extra: std::collections::BTreeMap::new(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await
        .expect("the owner reads its room's floor");
        served.push(decode::<RoomListRosterReply>(&bytes).unwrap().labelers);
    }

    let set = served[0]
        .as_ref()
        .expect("a room that named its labelers serves the signed set");
    let signed: fauna_mls::room_policy::SignedRoomLabelers =
        fauna_protocol::decode_strict(set).unwrap();
    signed
        .verify_signature()
        .expect("exactly the bytes its author signed");
    assert_eq!(signed.signer, owner.actor_id());
    assert_eq!(signed.labelers.version, 1);
    assert_eq!(signed.labelers.labelers, vec![wasm]);
    assert!(served[1].is_none(), "a room that named none serves none");
}

#[test]
fn the_labeler_set_kind_is_user_class_only() {
    let kind = "fauna.conversations.room.set_labelers";
    assert!(is_permitted(CallerClass::User, kind));
    for class in [
        CallerClass::BridgeMta,
        CallerClass::BridgeMda,
        CallerClass::BridgeAtprotoPds,
        CallerClass::Custodian,
        CallerClass::ContentProcessor,
    ] {
        assert!(
            !is_permitted(class, kind),
            "{class:?} names no room's labelers"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Identity succession and account deletion on a community room's floor
// ═════════════════════════════════════════════════════════════════════════════
//
// `conversation-rooms.md` § The home nest → *Transfer by succession*: the
// successor joins every room as a new member — "the recipient-set roster's
// fresh entry with the predecessor `Removed`" — and the ownership re-point
// carries the owner role. A community room's floor IS its membership
// authority and no member report will ever arrive for it, so the ceremony has
// to seat the successor itself; and because the nest cannot rewrite the
// room's signed policy, the names in it resolve through the succession chain
// wherever the floor compares them to a seat (§ Roles and authorization →
// *Community rooms — enforced at the floor*).
//
// Every succession below is a REAL one — a RecoveryKey registered over the
// identity's own connection, then the statement submitted pre-identity.

async fn router_with_recovery_and_account() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    recovery_handlers::register_recovery_handlers(&mut b);
    account_handlers::register_account_user_handlers(&mut b);
    (b.build(), state)
}

/// Succeed `who` for real and hand back the successor's keypair.
async fn succeed(
    router: &RpcRouter,
    state: &Arc<AppState>,
    who: &ActorKeypair,
    recovery_seed: u8,
) -> ActorKeypair {
    let recovery = RecoveryKey::from_bytes([recovery_seed; 32]);
    common::register_recovery_key(
        router,
        state,
        who.signing_key(),
        who.actor_id().0,
        &recovery,
        None,
        1,
    )
    .await;
    let successor = ActorKeypair::generate();
    common::submit_succession(
        router,
        state,
        common::succession_bytes(
            &recovery,
            who.actor_id().0,
            successor.signing_key(),
            None,
            2,
        ),
    )
    .await
    .expect("the succession lands");
    successor
}

/// A principal's floor row — live or `Removed`-absorbed — as
/// `(role, removed_at, entry_id, reception_pubkey)`.
async fn floor_row_of(
    state: &AppState,
    room_id: &[u8; 32],
    who: &ActorKeypair,
) -> Option<(
    Option<String>,
    Option<i64>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
)> {
    state
        .db
        .list_floor_roster_including_removed(room_id)
        .await
        .expect("floor history")
        .into_iter()
        .find(|m| m.principal_id == who.actor_id().0)
        .map(|m| (m.role, m.removed_at, m.entry_id, m.reception_pubkey))
}

fn account_delete_payload() -> Bytes {
    common::encode(&AccountDeleteRequest {
        extra: Default::default(),
    })
}

#[tokio::test]
async fn an_owners_succession_hands_the_owner_seat_to_the_successor() {
    let (router, state) = router_with_recovery_and_account().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let member = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(60)).await;
    let room_id = room_id_of(&created.room_id);
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    let successor = succeed(&router, &state, &owner, 0x61).await;

    // The floor: the successor holds the owner seat at a fresh roster entry
    // with no wrap target — the predecessor's reception key derives from the
    // seed the ceremony exists to retire.
    assert_eq!(
        live_role(&state, &room_id, &successor).await.as_deref(),
        Some("owner"),
        "the owner's successor holds the owner seat on the floor"
    );
    let (old_role, old_removed, old_entry, _) = floor_row_of(&state, &room_id, &owner)
        .await
        .expect("the predecessor's row stays as history");
    assert_eq!(old_role.as_deref(), Some("owner"), "history keeps its rank");
    assert!(
        old_removed.is_some(),
        "the predecessor's seat is Removed-absorbed"
    );
    let (_, _, entry, key) = floor_row_of(&state, &room_id, &successor)
        .await
        .expect("the successor is seated");
    assert!(
        entry.is_some() && entry != old_entry,
        "a fresh roster entry"
    );
    assert_eq!(key, None, "never the predecessor's wrap target");
    assert_eq!(
        state.db.get_room(&room_id).await.unwrap().unwrap().owner_id,
        Some(successor.actor_id().0.to_vec()),
        "the room record agrees with the floor"
    );

    // Appoint. The stored policy still names the predecessor — the nest cannot
    // rewrite a signed blob — and the successor's first re-sign names itself:
    // not an ownership change, because both names stand for the same seat.
    dispatch(
        &router,
        state.clone(),
        successor.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &successor,
            &policy_at(&successor, 3, &[&admin, &member], "the square"),
        ),
    )
    .await
    .expect("the owner's successor appoints an admin");
    assert_eq!(
        live_role(&state, &room_id, &member).await.as_deref(),
        Some("admin")
    );
    assert_eq!(
        live_role(&state, &room_id, &successor).await.as_deref(),
        Some("owner")
    );

    // Transfer — the owner-only door the stranded room had lost for good.
    dispatch(
        &router,
        state.clone(),
        successor.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(
            &created.room_id,
            &successor,
            &policy_at(&admin, 4, &[&successor, &member], "the square"),
        ),
    )
    .await
    .expect("the owner's successor transfers ownership");
    assert_eq!(
        live_role(&state, &room_id, &admin).await.as_deref(),
        Some("owner")
    );
    assert_eq!(
        live_role(&state, &room_id, &successor).await.as_deref(),
        Some("admin")
    );
}

#[tokio::test]
async fn an_admins_succession_hands_the_admin_seat_to_the_successor() {
    let (router, state) = router_with_recovery_and_account().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let member = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(62)).await;
    let room_id = room_id_of(&created.room_id);
    seat_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    let successor = succeed(&router, &state, &admin, 0x63).await;

    assert_eq!(
        live_role(&state, &room_id, &successor).await.as_deref(),
        Some("admin"),
        "an admin's successor holds the admin seat on the floor"
    );
    assert!(
        floor_row_of(&state, &room_id, &admin)
            .await
            .expect("history")
            .1
            .is_some(),
        "the predecessor's seat is Removed-absorbed"
    );

    // A rename carrying the admin set verbatim — still naming the predecessor.
    // No owner re-sign is owed first: the name resolves to the successor's seat.
    dispatch(
        &router,
        state.clone(),
        successor.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &successor,
            &policy_at(&owner, 3, &[&admin], "renamed by the successor"),
        ),
    )
    .await
    .expect("the admin's successor renames the room");
    assert_eq!(
        live_role(&state, &room_id, &successor).await.as_deref(),
        Some("admin"),
        "the carried name keeps the successor's rank through the reconcile"
    );

    // Naming itself instead is the same admin set on the floor.
    dispatch(
        &router,
        state.clone(),
        successor.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &successor,
            &policy_at(&owner, 4, &[&successor], "renamed again"),
        ),
    )
    .await
    .expect("re-naming itself is still an admin's edit");

    // A genuine admin-set change stays the owner's alone.
    let err = dispatch(
        &router,
        state.clone(),
        successor.actor_id().0,
        "fauna.conversations.room.set_policy",
        set_policy_payload(
            &created.room_id,
            &successor,
            &policy_at(&owner, 5, &[&successor, &member], "renamed again"),
        ),
    )
    .await
    .expect_err("an admin's successor appoints nobody");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
    assert_eq!(
        live_role(&state, &room_id, &member).await.as_deref(),
        Some("member")
    );
}

// ── A seat's own wrap target: `room.set_reception_key` ─────────────────────
//
// `community-rooms.md` § Implementation status today, *A seat gains or rotates
// its wrap target*. The ceremony above seats the successor KEYLESS — the
// predecessor's key belongs to the material it retires — and every other
// writer of the column is a seating act, so until this door the successor
// governed a room it could not read (`conversation-rooms.md` § Implementation
// status today, the succession-axis bullet's declared bound (2), retired).

async fn router_with_recovery_and_reader_nest() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(booted_reader_nest(db).await);
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    recovery_handlers::register_recovery_handlers(&mut b);
    account_handlers::register_account_user_handlers(&mut b);
    (b.build(), state)
}

fn set_reception_key_payload(room_hex: &str, reception_pubkey: Vec<u8>) -> Bytes {
    let req = RoomSetReceptionKeyRequest {
        room_id: room_hex.into(),
        reception_pubkey,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// `who` binds `record`'s public half as the wrap target of the seat it holds.
async fn set_reception_key(
    router: &RpcRouter,
    state: &Arc<AppState>,
    room_hex: &str,
    who: &ActorKeypair,
    record: &fauna_core::group_generation::GroupReceptionKeyRecord,
) -> RoomSetReceptionKeyReply {
    let bytes = dispatch(
        router,
        state.clone(),
        who.actor_id().0,
        "fauna.conversations.room.set_reception_key",
        set_reception_key_payload(room_hex, record.reception_pubkey().unwrap()),
    )
    .await
    .expect("a live member binds its own wrap target");
    decode(&bytes).unwrap()
}

/// Whether the wrap `wire` carries opens under `record`'s secret at the entry
/// the nest says it is bound to — `false` for any other key, because nothing
/// is ever re-sealed.
fn opens_with(
    wire: &fauna_protocol::conversations::RoomGenerationWire,
    record: &fauna_core::group_generation::GroupReceptionKeyRecord,
) -> bool {
    let entry = room_id_of(&wire.entry_id);
    let generation = room_id_of(&wire.generation_id);
    let commitment: [u8; 32] = wire
        .key_commitment
        .clone()
        .try_into()
        .expect("32-byte commitment");
    group_generation_wraps::open_group_generation_key_as_entry(
        &wire.wrap,
        &record.keypair().unwrap().secret,
        &generation,
        &entry,
        &commitment,
    )
    .is_ok()
}

/// The predecessor's roster entry — the seat the ceremony `Removed`-absorbed.
async fn retired_entry_of(state: &AppState, room_id: &[u8; 32], who: &ActorKeypair) -> [u8; 32] {
    let (_, removed, entry, _) = floor_row_of(state, room_id, who)
        .await
        .expect("the predecessor's row stays as history");
    assert!(removed.is_some(), "and it is absorbed, not live");
    entry
        .expect("at the entry it was seated at")
        .try_into()
        .expect("32-byte entry")
}

/// **The whole gap, closed end to end over a REAL succession**: the successor
/// is seated keyless and opens nothing; it supplies its own key through the
/// door, on the entry the ceremony derived; an admin's ordinary top-up covers
/// the tip; and a generation minted after the ceremony wraps to the successor
/// — which opens it with its own key — and not to the predecessor's seat.
#[tokio::test]
async fn a_successor_supplies_its_wrap_target_and_reads_a_generation_minted_after_the_ceremony() {
    let (router, state) = router_with_recovery_and_reader_nest().await;
    let owner = ActorKeypair::generate();
    let admin = ActorKeypair::generate();
    let (created, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x70)).await;
    let room_id = room_id_of(&created.room_id);
    let _admin_recv =
        seat_keyed_member(&router, &state, &owner, &created.room_id, &room_id, &admin).await;
    appoint_admin(&router, &state, &owner, &created.room_id, &room_id, &admin).await;
    let (_, gen_key0, gen0) =
        publish_generation(&router, &state, &created.room_id, &room_id, &owner).await;

    let successor = succeed(&router, &state, &owner, 0x72).await;
    let (_, _, entry, key) = floor_row_of(&state, &room_id, &successor)
        .await
        .expect("the ceremony seated the successor");
    let entry = entry.expect("at a fresh roster entry");
    assert_eq!(
        key, None,
        "keyless — the predecessor's key belongs to the retired material"
    );
    assert!(
        read_generations(&router, &state, &created.room_id, &successor)
            .await
            .generations
            .is_empty(),
        "so it opens nothing: the state this door exists to heal"
    );

    let successor_recv = reception();
    let reply = set_reception_key(
        &router,
        &state,
        &created.room_id,
        &successor,
        &successor_recv,
    )
    .await;
    assert_eq!(
        room_id_of(&reply.entry_id).as_slice(),
        entry.as_slice(),
        "the seat keeps the entry the ceremony derived"
    );
    assert!(!reply.rotated, "a first key is not a rotation");
    assert_eq!(
        reply.uncovered_tip.as_deref(),
        Some(hex::encode(gen0).as_str()),
        "and the door names the tip the seat holds no wrap for"
    );

    // An admin's top-up covers the tip — the ordinary key-in, now possible.
    dispatch(
        &router,
        state.clone(),
        admin.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(
            &created.room_id,
            &successor,
            vec![build_topup(&state, &room_id, &successor, &admin, &gen_key0, &gen0).await],
        ),
    )
    .await
    .expect("an admin backfills the tip to the successor's keyed seat");

    // A generation minted AFTER the ceremony wraps to the successor…
    let (_, _, gen1) =
        publish_generation(&router, &state, &created.room_id, &room_id, &admin).await;
    let gens = read_generations(&router, &state, &created.room_id, &successor)
        .await
        .generations;
    assert_eq!(
        gens.len(),
        2,
        "the backfilled tip and the generation minted after the ceremony"
    );
    let tip = gens.iter().find(|g| g.is_tip).expect("a tip");
    assert_eq!(room_id_of(&tip.generation_id), gen1);
    assert!(
        opens_with(tip, &successor_recv),
        "and the successor opens it with its own key"
    );
    // …and not to the predecessor's seat: severed, as a `Removed` row must be.
    let retired = retired_entry_of(&state, &room_id, &owner).await;
    assert!(
        state
            .db
            .get_room_generation_wrap(&room_id, &gen1, &retired)
            .await
            .unwrap()
            .is_none(),
        "the retired seat holds no wrap for a generation minted after the ceremony"
    );
}

/// **A successor that is the room's only key authority mints itself back in.**
/// Nobody else can back it in — a top-up is an owner's or admin's act and the
/// predecessor was the only one — so its way back is a fresh generation
/// parented on the tip the door named, which the mint door admits from a
/// minter that holds no key for the generation it replaces: the parent check
/// is by name. The same mint is the severance the predecessor's `Removed` row
/// calls for, and it covers the plain member and the nest as the floor stands.
#[tokio::test]
async fn a_successor_that_is_the_rooms_only_key_authority_mints_itself_back_in() {
    let (router, state) = router_with_recovery_and_reader_nest().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let (created, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x73)).await;
    let room_id = room_id_of(&created.room_id);
    let member_recv =
        seat_keyed_member(&router, &state, &owner, &created.room_id, &room_id, &member).await;
    let (_, _, gen0) =
        publish_generation(&router, &state, &created.room_id, &room_id, &owner).await;

    let successor = succeed(&router, &state, &owner, 0x74).await;
    let successor_recv = reception();
    let reply = set_reception_key(
        &router,
        &state,
        &created.room_id,
        &successor,
        &successor_recv,
    )
    .await;
    let parent = room_id_of(
        reply
            .uncovered_tip
            .as_deref()
            .expect("the successor holds no wrap for the tip"),
    );
    assert_eq!(parent, gen0);

    let targets = wrap_targets(&state, &room_id).await;
    assert!(
        targets
            .iter()
            .any(|t| t.member_actor == successor.actor_id()),
        "the keyed seat is now a wrap target the floor serves"
    );
    let (mint, _key1, gen1) =
        build_mint_with(&targets, vec![parent], &successor, next_mint_stamp());
    let bytes = dispatch(
        &router,
        state.clone(),
        successor.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&created.room_id, &mint),
    )
    .await
    .expect("the owner's successor mints without holding the generation it replaces");
    let published: RoomPublishGenerationReply = decode(&bytes).unwrap();
    assert_eq!(room_id_of(&published.generation_id), gen1);

    let gens = read_generations(&router, &state, &created.room_id, &successor)
        .await
        .generations;
    assert_eq!(
        gens.len(),
        1,
        "the generation it minted, and nothing before it"
    );
    assert!(gens[0].is_tip);
    assert!(opens_with(&gens[0], &successor_recv), "and it opens it");

    let member_gens = read_generations(&router, &state, &created.room_id, &member)
        .await
        .generations;
    assert!(
        member_gens
            .iter()
            .any(|g| room_id_of(&g.generation_id) == gen1 && opens_with(g, &member_recv)),
        "the plain member is covered by the same mint"
    );
    let retired = retired_entry_of(&state, &room_id, &owner).await;
    assert!(
        state
            .db
            .get_room_generation_wrap(&room_id, &gen1, &retired)
            .await
            .unwrap()
            .is_none(),
        "and the retired seat is severed from it"
    );
}

/// **A keyed seat rotates on the same entry, and its earlier wrap stays
/// served under the key it was sealed to.** The scheme's wrap target is the
/// member's *current* key (`account-data-taxonomy.md` § The recipient-set
/// scheme → *Severance, per axis*), so a key already set is replaced; the
/// entry is not re-derived — that is the re-admission rule, for a `Removed`
/// member — so nothing already sealed is lost, and the next mint seals to the
/// new key. Naming the bound key again is answered, not refused.
#[tokio::test]
async fn a_keyed_seat_rotates_on_the_same_entry_and_its_earlier_wrap_stays_served() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (created, owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x75)).await;
    let room_id = room_id_of(&created.room_id);
    publish_generation(&router, &state, &created.room_id, &room_id, &owner).await;
    let (_, _, entry_before, _) = floor_row_of(&state, &room_id, &owner)
        .await
        .expect("seated");

    let rotated_recv = reception();
    let reply = set_reception_key(&router, &state, &created.room_id, &owner, &rotated_recv).await;
    assert!(
        reply.rotated,
        "a different key on a keyed seat is a rotation"
    );
    assert_eq!(
        Some(room_id_of(&reply.entry_id).to_vec()),
        entry_before,
        "the entry survives the rotation"
    );
    assert_eq!(
        reply.uncovered_tip, None,
        "the seat still holds the tip's wrap — sealed to the key it was minted under"
    );

    let gens = read_generations(&router, &state, &created.room_id, &owner)
        .await
        .generations;
    assert_eq!(gens.len(), 1);
    assert!(
        opens_with(&gens[0], &owner_recv),
        "the earlier wrap opens with the retained key"
    );
    assert!(
        !opens_with(&gens[0], &rotated_recv),
        "and not with the new one — nothing was re-sealed"
    );

    // The next mint seals to the rotated key.
    let (_, _, gen1) =
        publish_generation(&router, &state, &created.room_id, &room_id, &owner).await;
    let gens = read_generations(&router, &state, &created.room_id, &owner)
        .await
        .generations;
    assert_eq!(gens.len(), 2);
    let tip = gens.iter().find(|g| g.is_tip).expect("a tip");
    assert_eq!(room_id_of(&tip.generation_id), gen1);
    assert!(opens_with(tip, &rotated_recv));
    assert!(!opens_with(tip, &owner_recv));

    let again = set_reception_key(&router, &state, &created.room_id, &owner, &rotated_recv).await;
    assert!(!again.rotated, "the bound key again is a no-op");
    assert_eq!(again.uncovered_tip, None);
}

/// **The door binds only a live user seat's own key, and only a key of the
/// one shape every wrap seals to.** A stranger is refused as a non-member; a
/// malformed key is refused before anything is written — bound, it would make
/// the seat coverable and every mint over it unopenable; and a mirror room,
/// whose membership authority is its MLS group, has no wrap targets to bind.
#[tokio::test]
async fn the_wrap_target_door_binds_only_a_live_user_seats_own_well_formed_key() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let stranger = ActorKeypair::generate();
    let (created, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x76)).await;
    let room_id = room_id_of(&created.room_id);

    let err = dispatch(
        &router,
        state.clone(),
        stranger.actor_id().0,
        "fauna.conversations.room.set_reception_key",
        set_reception_key_payload(&created.room_id, reception().reception_pubkey().unwrap()),
    )
    .await
    .expect_err("a stranger holds no seat to key");
    assert_eq!(err.code, "fauna.conversations.permission_denied");

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_reception_key",
        set_reception_key_payload(&created.room_id, vec![0x11u8; 5]),
    )
    .await
    .expect_err("a key of the wrong shape is refused");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
    let (_, _, _, key) = floor_row_of(&state, &room_id, &owner)
        .await
        .expect("seated");
    assert_ne!(
        key.as_deref(),
        Some(&[0x11u8; 5][..]),
        "and nothing was written"
    );

    // A mirror room: reported, never founded.
    let mirror_hex = hex::encode([0x77u8; 32]);
    let mirror_id = room_id_of(&mirror_hex);
    seat_on_routing_roster(&state, &owner, &mirror_id).await;
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.roster_report",
        report_payload(&mirror_hex, vec![entry(&owner, "owner")]),
    )
    .await
    .expect("the member's report is stored");
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_reception_key",
        set_reception_key_payload(&mirror_hex, reception().reception_pubkey().unwrap()),
    )
    .await
    .expect_err("an end-to-end room's floor is a mirror with no wrap targets");
    assert_eq!(err.code, "fauna.conversations.permission_denied");
}

/// **Row 749.** The right length (1216 bytes) but a FIPS-203-invalid ML-KEM
/// half — before the fix, the length-only check let exactly this key
/// through, binding a seat that would later freeze every mint over the room
/// the moment a member's own key held no coefficient below the modulus.
/// Refused before anything is written, exactly like the wrong-shape key
/// above.
#[tokio::test]
async fn set_reception_key_refuses_a_fips_invalid_key() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let (created, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x7A)).await;
    let room_id = room_id_of(&created.room_id);

    let poisoned = vec![0xFFu8; fauna_mls::wrapped_blob::XWING_ENCAPS_KEY_LEN];
    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.set_reception_key",
        set_reception_key_payload(&created.room_id, poisoned.clone()),
    )
    .await
    .expect_err("a FIPS-203-invalid key is refused");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
    let (_, _, _, key) = floor_row_of(&state, &room_id, &owner)
        .await
        .expect("seated");
    assert_ne!(
        key.as_deref(),
        Some(poisoned.as_slice()),
        "and nothing was written"
    );
}

/// The recovery arm of the FIPS-203 reception-key fix:
/// a seat bound BEFORE the fix could hold a key none of the three doors
/// accept anymore, so healing it needs a write straight into the column they
/// refuse — [`CacheDb::set_room_member_reception_key`], which does no
/// validation. This test seeds exactly that seat and pins
/// `usable_reception_key` (`conversations_handlers.rs:3174-3177`) at every
/// door that reads it: the poisoned seat is reported absent on the roster,
/// skipped rather than frozen at mint coverage, refused as unusable by
/// backfill, and heals the moment its own owner re-keys.
///
/// Reverting the predicate to `!bytes.is_empty()` (the pre-fix reading) must
/// red at least the roster and coverage assertions below.
#[tokio::test]
async fn a_poisoned_seat_is_skipped_not_frozen_and_heals_on_its_own_rekey() {
    let (router, state) = router_with_reader_nest().await;
    let owner = ActorKeypair::generate();
    let poisoned_member = ActorKeypair::generate();
    let (created, _owner_recv) = create_keyed_room(&router, &state, &owner, birth_salt(0x71)).await;
    let room_id = room_id_of(&created.room_id);

    seat_keyed_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &poisoned_member,
    )
    .await;

    // Seated the ordinary way, THEN poisoned straight through the db — the
    // one writer that does no validation, and the only way a pre-fix room
    // could ever have ended up holding one.
    let poisoned = vec![0xFFu8; fauna_mls::wrapped_blob::XWING_ENCAPS_KEY_LEN];
    state
        .db
        .set_room_member_reception_key(&room_id, &poisoned_member.actor_id().0, &poisoned)
        .await
        .expect("db write")
        .expect("the seated member's own row is touched");

    // 1. The roster reports the seat's key absent — never the poisoned bytes.
    let roster_bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.list_roster",
        Bytes::from(
            encode_canonical(&RoomListRosterRequest {
                room_id: created.room_id.clone(),
                at_policy_version: None,
                extra: std::collections::BTreeMap::new(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("the owner reads its own room's floor");
    let roster: RoomListRosterReply = decode(&roster_bytes).unwrap();
    let seat = roster
        .members
        .iter()
        .find(|m| m.principal == hex::encode(poisoned_member.actor_id().0))
        .expect("the poisoned seat still renders on the floor");
    assert!(
        seat.reception_pubkey.is_none(),
        "an unusable stored key reports as no key, never as the poisoned bytes"
    );

    // 2. The owner's mint over the REST of the floor is admitted — coverage
    // skips the poisoned seat instead of refusing "does not wrap to every
    // live member". Built from `wrap_targets` filtered to leave the seat
    // out: the unfiltered targets `publish_generation` uses would hand the
    // poisoned bytes to `build_group_mint`, which refuses before sealing.
    let live_targets: Vec<RosterMember> = wrap_targets(&state, &room_id)
        .await
        .into_iter()
        .filter(|m| m.member_actor != poisoned_member.actor_id())
        .collect();
    let (mint, _key, _gen) = build_mint(&state, &room_id, &owner, &live_targets).await;
    let publish_bytes = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.publish_generation",
        publish_payload(&created.room_id, &mint),
    )
    .await
    .expect("the owner's mint over the floor minus the poisoned seat is admitted");
    let publish_reply: RoomPublishGenerationReply = decode(&publish_bytes).unwrap();

    // 3. The backfill door refuses the poisoned seat exactly as it refuses a
    // keyless one — and the seat really does hold an entry id, so this
    // exercises the usable-key check (`:5549`), not the earlier no-entry-id
    // check (`:5543`), which gives the identical refusal text.
    let (_, _, entry_id, _) = floor_row_of(&state, &room_id, &poisoned_member)
        .await
        .expect("the poisoned seat is still on the floor");
    assert!(
        entry_id.is_some(),
        "the seat keeps its real entry id — this must exercise the key check, not the entry check"
    );
    let backfill_err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.backfill_generations",
        backfill_payload(&created.room_id, &poisoned_member, Vec::new()),
    )
    .await
    .expect_err("a poisoned seat holds no usable wrap target");
    assert_eq!(backfill_err.code, "fauna.conversations.invalid_params");
    assert_eq!(
        backfill_err.detail_or_code(),
        "the backfill target holds no wrap target — it cannot be covered yet",
        "must be the key-validity refusal at :5549, not the unrelated empty-wraps refusal at :5567 \
         (same code, different door)"
    );

    // 4. The seat's own re-key heals it: `rotated: true` (the db compares
    // stored bytes at `db/rooms.rs:1070-1072`), and the very next mint covers
    // it again.
    let healed = reception();
    let bound_reply =
        set_reception_key(&router, &state, &created.room_id, &poisoned_member, &healed).await;
    assert!(
        bound_reply.rotated,
        "a valid key replacing a poisoned one is a rotation"
    );

    let (final_reply, _key2, _gen2) =
        publish_generation(&router, &state, &created.room_id, &room_id, &owner).await;
    assert_eq!(
        final_reply.covered,
        publish_reply.covered + 1,
        "the healed seat is covered by the very next mint"
    );
}

/// `conversation-rooms.md` § Roles and authorization — the owner rule's
/// deletion half. A self-deletion is refused while a live user member homed
/// here could take the room over, at the scheduling door and again at the
/// executor (a persisted deletion can outlive the door that would have refused
/// it); transferring first lifts both.
#[tokio::test]
async fn an_owner_cannot_delete_their_account_while_a_member_could_take_the_room_over() {
    let (router, state) = router_with_recovery_and_account().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(64)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    let err = dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.account.delete",
        account_delete_payload(),
    )
    .await
    .expect_err("scheduling the owner's deletion is refused while a member could take over");
    assert_eq!(
        err.code, "fauna.account.room_ownership_held",
        "got: {err:?}"
    );

    pending_actions::finalize_self_deletion(&state, &owner.actor_id().0)
        .await
        .expect_err("the executor refuses a governed room's owner until transferred");
    assert!(
        state
            .db
            .get_user(&owner.actor_id().0)
            .await
            .unwrap()
            .is_some(),
        "the refused finalize leaves the account intact"
    );
    assert_eq!(
        live_role(&state, &room_id, &owner).await.as_deref(),
        Some("owner"),
        "and the owner seat with it"
    );

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(
            &created.room_id,
            &owner,
            &policy_at(&member, 2, &[], "the square"),
        ),
    )
    .await
    .expect("the owner hands the room on");
    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.account.delete",
        account_delete_payload(),
    )
    .await
    .expect("with the room handed on, the owner may schedule their deletion");
    pending_actions::finalize_self_deletion(&state, &owner.actor_id().0)
        .await
        .expect("and the executor completes it");
    assert_eq!(
        live_role(&state, &room_id, &member).await.as_deref(),
        Some("owner")
    );
}

/// The witness above calls `pending_actions::finalize_self_deletion` directly
/// — one level below the arm `execute_action` actually dispatches to
/// (`pending_actions.rs:378-393`). Nothing else drove a *persisted* action
/// through the executor, so an arm swapped back to `finalize_user_deletion`
/// would red nothing (). This one enters through the tick instead, with the member seated
/// *after* scheduling — the door has nobody to refuse yet, so the case this
/// pins is exactly the doc comment's "a member joins between scheduling and
/// execution".
#[tokio::test]
async fn a_persisted_self_deletion_is_refused_by_the_executor_tick_too() {
    let (router, state) = router_with_recovery_and_account().await;
    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();

    let created = create_room(&router, &state, &owner, birth_salt(68)).await;
    let room_id = room_id_of(&created.room_id);

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.account.delete",
        account_delete_payload(),
    )
    .await
    .expect("the owner is still alone, so the door has nothing to refuse");

    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;

    let action = state
        .db
        .list_pending_actions_for_actor(&owner.actor_id().0)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.action_type == "account.delete")
        .expect("the scheduled deletion is on file");
    state.db.test_set_execute_after(action.id, 1).await.unwrap();

    let executed = pending_actions::execute_ready_actions(&state)
        .await
        .unwrap();
    assert_eq!(
        executed, 0,
        "the executor tick must refuse — a member could still take the room over"
    );
    assert_eq!(
        state
            .db
            .get_pending_action(action.id)
            .await
            .unwrap()
            .expect("the row survives a refused tick")
            .status,
        "pending",
        "a refused action is not marked executed, so the tick retries it"
    );
    assert!(
        state
            .db
            .get_user(&owner.actor_id().0)
            .await
            .unwrap()
            .is_some(),
        "the refused tick leaves the account intact"
    );
    assert_eq!(
        live_role(&state, &room_id, &owner).await.as_deref(),
        Some("owner"),
        "and the owner seat with it"
    );

    dispatch(
        &router,
        state.clone(),
        owner.actor_id().0,
        "fauna.conversations.room.transfer_ownership",
        transfer_payload(
            &created.room_id,
            &owner,
            &policy_at(&member, 2, &[], "the square"),
        ),
    )
    .await
    .expect("the owner hands the room on");

    let executed = pending_actions::execute_ready_actions(&state)
        .await
        .unwrap();
    assert_eq!(
        executed, 1,
        "with the room handed on, the retried tick completes the deletion"
    );
    assert!(
        state
            .db
            .get_user(&owner.actor_id().0)
            .await
            .unwrap()
            .is_none(),
        "the account is gone"
    );
    assert_eq!(
        live_role(&state, &room_id, &member).await.as_deref(),
        Some("owner")
    );
}

/// The refusal above must never be permanent (`nest/common.md` § Client-state
/// recoverability). An owner with nobody who could take the room over is let
/// go — the room keeps its members without an owner seat. And an ADMIN's
/// deletion of a room owner is never refused: the admin holds no door that
/// could transfer the room, so a refusal would let any user make themselves
/// undeletable by seating one other account.
#[tokio::test]
async fn a_deletion_with_nobody_to_take_the_room_over_is_never_held_hostage() {
    let (router, state) = router_with_recovery_and_account().await;
    let alone = ActorKeypair::generate();
    let lonely = create_room(&router, &state, &alone, birth_salt(66)).await;
    let lonely_id = room_id_of(&lonely.room_id);

    dispatch(
        &router,
        state.clone(),
        alone.actor_id().0,
        "fauna.account.delete",
        account_delete_payload(),
    )
    .await
    .expect("an owner beside only the home nest has nobody to transfer to");
    pending_actions::finalize_self_deletion(&state, &alone.actor_id().0)
        .await
        .expect("so the executor lets the account go");
    assert!(
        state.db.get_room(&lonely_id).await.unwrap().is_some(),
        "the room itself is not destroyed"
    );

    let owner = ActorKeypair::generate();
    let member = ActorKeypair::generate();
    let created = create_room(&router, &state, &owner, birth_salt(67)).await;
    let room_id = room_id_of(&created.room_id);
    seat_member(
        &router,
        &state,
        &owner,
        &created.room_id,
        &room_id,
        &member,
        fauna_mls::room_policy::RoomRole::Member,
    )
    .await;
    pending_actions::finalize_user_deletion(&state, &owner.actor_id().0)
        .await
        .expect("an admin's deletion of a room owner is never refused for the room");
    assert_eq!(
        live_role(&state, &room_id, &member).await.as_deref(),
        Some("member"),
        "the room keeps its members"
    );
}
