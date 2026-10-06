//! **Room-restricted posts** — the vocabulary every reader of one shares
//! (`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*,
//! 2026-09-10).
//!
//! A room-restricted post is an ordinary post of its author whose body only a
//! room's floor members open. Three readers meet one: the author's device
//! sealing it, a member's device opening it, and — for a community room — the
//! room's home nest opening it at reception. They agree on *which* key through
//! [`RoomPostSeal`], read off the post's own
//! [`KeyAccess::Room`](crate::subscription::types::KeyAccess::Room) arm by
//! [`room_post_of`], so no reader re-derives the class from anything else.
//!
//! [`RoomPostKeys`] is the device-side seam: the room's keys live in the
//! conversations plane (the MLS engine for an end-to-end room, the member's
//! generation wraps for a community one), which the feed does not hold, so the
//! feed asks through this trait rather than growing a dependency on it.

use crate::maybe_send::MaybeSendSync;
use crate::subscription::types::KeyAccess;

/// Which of a room's two member-keyed classes seals a room-restricted post —
/// the half of [`KeyAccess::Room`] that says which base key opens it.
///
/// The per-post key is `derive_post_key(base, seal_id)` either way — the Posts
/// row's own seal (`conversation-rooms.md` § The group plane's fate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomPostSeal {
    /// An **end-to-end** room: the base is the MLS epoch secret exported at
    /// `epoch`, so only a member whose group is at that epoch derives it.
    EndToEnd { epoch: u64 },
    /// A **community** room: the base is
    /// [`room_post_base_key`](crate::group_content::room_post_base_key) over
    /// the generation key `generation` names, so every holder of that
    /// generation's wrap derives it — the room's home nest included.
    Community { generation: [u8; 32] },
}

/// The room a room-restricted post addresses (its 32-byte channel id) and
/// which of the room's keys seals it — or `None` when `access` is not a room
/// arm this build can resolve.
///
/// `None` covers two honest cases a caller must not tell apart by guessing: a
/// different arm (`Broadcast`, an unknown future one), and a room arm whose
/// `group_id` is not a 32-byte channel id — the arm's never-authored
/// subscriber-tier reading, which names an MLS group rather than a room and so
/// has no room for any reader to resolve.
#[must_use]
pub fn room_post_of(access: &KeyAccess) -> Option<([u8; 32], RoomPostSeal)> {
    let KeyAccess::Room {
        group_id,
        epoch,
        generation,
    } = access
    else {
        return None;
    };
    let room = <[u8; 32]>::try_from(group_id.0.as_slice()).ok()?;
    let seal = match generation {
        Some(generation) => RoomPostSeal::Community {
            generation: *generation,
        },
        None => RoomPostSeal::EndToEnd { epoch: *epoch },
    };
    Some((room, seal))
}

/// One room a new post can be addressed to right now: its 32-byte channel id,
/// and the label the author's own conversation list reads it by — never a
/// label an app re-derives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomPostRoom {
    pub room: [u8; 32],
    pub label: String,
}

/// The **room-post key** seam: the base key a room-restricted post opens
/// under, resolved by whatever on this device holds the room's keys.
///
/// Implemented by the conversations plane — it alone holds an end-to-end
/// room's MLS group and a community member's generation wraps — and consumed by
/// the feed's unlock, which holds neither. An **unset** seam leaves every room
/// post locked, which is the honest state for a device with no conversations
/// plane: it could not open one anyway.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait RoomPostKeys: MaybeSendSync {
    /// The base key a **new** post addressed to `room` seals under right now,
    /// and the [`RoomPostSeal`] that names it on the post — the room's tip
    /// generation for a community room, its current epoch for an end-to-end
    /// one. Always resolved now, never cached: sealing under a generation the
    /// room has rotated past would hand the post to a member the rotation
    /// removed.
    async fn room_post_seal_key(
        &self,
        room: [u8; 32],
    ) -> Result<(RoomPostSeal, zeroize::Zeroizing<[u8; 32]>), String>;

    /// The base key for a post room `room` sealed as `seal`.
    ///
    /// `Err` carries why this device cannot open it — not a member, not keyed
    /// into that generation, an end-to-end group no longer at that epoch, a
    /// read that failed — as a reason for the reader, never a key-shaped
    /// fallback. The caller keeps the post locked either way.
    async fn room_post_base_key(
        &self,
        room: [u8; 32],
        seal: RoomPostSeal,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, String>;

    /// The rooms a **new** post can be addressed to from this device right
    /// now — every bound room of a member-keyed class (end-to-end or
    /// community) its user sits on the floor of, labelled as their own
    /// conversation list reads it. The composer's audience options
    /// (`ui/feed.md` § User actions, `compose-gate-tier-select`).
    ///
    /// Defaults to none: a seam that only opens offers nothing to post to.
    async fn room_post_rooms(&self) -> Vec<RoomPostRoom> {
        Vec::new()
    }

    /// The nest `room`'s canonical plane lives on, when that is **not** this
    /// device's own nest — `None` for a same-nest room.
    ///
    /// The feed holds no channel routing, so it asks here for the same reason
    /// it asks for keys: the conversations plane records each channel's home
    /// when it joins one, and the feed's reads of a room's *derived views*
    /// must follow the log. Today that is the room-post verdict read, which
    /// only the room's home nest can answer — its reception pass is what
    /// derived the verdicts, and a member's own nest indexes no post of a room
    /// it does not home (`ui/feed.md` § Encryption at rest →
    /// *Room-restricted — the ruling*, *Built* detail (v)).
    ///
    /// Defaults to `None`, which reads as "same-nest, or this seam cannot
    /// tell" — the honest answer for a seam with no channel routing, and the
    /// one that keeps every caller on the plain read it made before.
    async fn room_home_nest_url(&self, room: [u8; 32]) -> Option<String> {
        let _ = room;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::types::MlsGroupId;

    #[test]
    fn a_community_arm_names_its_room_and_generation() {
        let access = KeyAccess::Room {
            group_id: MlsGroupId(vec![0xC7; 32]),
            epoch: 0,
            generation: Some([0x9A; 32]),
        };
        assert_eq!(
            room_post_of(&access),
            Some((
                [0xC7; 32],
                RoomPostSeal::Community {
                    generation: [0x9A; 32]
                }
            ))
        );
    }

    #[test]
    fn an_end_to_end_arm_names_its_room_and_epoch() {
        let access = KeyAccess::Room {
            group_id: MlsGroupId(vec![0x11; 32]),
            epoch: 12,
            generation: None,
        };
        assert_eq!(
            room_post_of(&access),
            Some(([0x11; 32], RoomPostSeal::EndToEnd { epoch: 12 }))
        );
    }

    #[test]
    fn an_arm_that_names_no_room_resolves_to_nothing() {
        // A malformed arm names an MLS group by an arbitrary id, never a
        // 32-byte channel — so there is no room to open.
        let malformed = KeyAccess::Room {
            group_id: MlsGroupId(b"group-1".to_vec()),
            epoch: 1,
            generation: None,
        };
        assert_eq!(room_post_of(&malformed), None);
        let broadcast = KeyAccess::Broadcast {
            key_blob_ref: crate::data::ContentHash::from_digest_raw([1; 32]),
        };
        assert_eq!(room_post_of(&broadcast), None);
    }
}
