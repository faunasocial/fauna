//! Discovery carriage — the share leg's endpoint advertisements (slice F).
//!
//! Contract owner: `docs/goal/behavior/p2p.md` § Cross-user shared-set
//! transfer → *Discovery* ("peer endpoints learned only through
//! authenticated channels") and `p2p-shared-set-build.md` § *Build contract* → *Discovery carriage is
//! its own slice* ("the set's own conversation channel carries members'
//! endpoint advertisements, cached for offline dial").
//!
//! **The wire payload IS the row type.** A member advertises a
//! [`ShareEndpoints`] whose `member_actor` is *self-asserted* — attacker-
//! controllable, exactly like `ChannelMessage.sender` before
//! `MlsEngine::decrypt` overwrites it with the authenticated leaf
//! credential, and exactly like `PeerSyncAdmitRequest::endpoints`' carried
//! `node_id`. This module is where that assertion is **bound to what the
//! channel proved** ([`bind_share_advertisement`]) before anything durable
//! is written. One type instead of a near-identical wire twin: the
//! difference between the two is not shape, it is whether the identity has
//! been checked yet, and a second struct would only invite writing the
//! unchecked one.
//!
//! The carriage itself is the set's own MLS group — the advertisement rides
//! as opaque canonical bytes in a `ChannelMessageBody::ShareEndpoints` body,
//! the `ChannelMessageBody::Custody` discipline verbatim (never a chat
//! bubble, sink-routed, additive-legal within the major). Two properties come
//! free from that choice and are why the contract picked it: only members
//! can read the advertisement (it is group-sealed), and only a member can
//! *make* one that anybody believes (MLS authenticates the sending leaf).

use fauna_core::identity::ActorId;
use fauna_core::share_endpoints::{ShareEndpoints, share_entry_key};

/// Why an advertisement was refused. Each arm is a distinct lie, kept apart
/// so a consumer's telemetry can tell a confused peer from a hostile one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvertisementRefusal {
    /// The advertisement names a member other than the channel-proven
    /// sender — the third-party redirect this binding exists to stop.
    NotTheSender,
    /// The advertisement names a set other than the channel it arrived on.
    /// A member of set A must not be able to write set B's dial rows, even
    /// when it is a member of both: the carrying channel is the only proof
    /// of set membership this path has.
    WrongSet,
    /// The advertised `node_id` is not the advertiser's actor key. The share
    /// leg dials actor keys (PT-1b), so a differing NodeId is either
    /// malformed or an attempt to point dials at a third node.
    NodeIdNotTheActor,
}

/// Bind one received advertisement to the identity the channel proved.
///
/// `advertised` is the decoded payload (a **claim**), `sender` the
/// MLS-authenticated author `MlsEngine::decrypt` recovered from the sending
/// leaf credential, and `channel_id` the set whose channel carried it.
///
/// On success returns the `fauna.state.share-endpoints` entry key and the row
/// to write — with `member_actor`, `channel_id` and the advertised `node_id`
/// all agreeing with what was proven rather than with what was claimed. The
/// row is then exactly what
/// `fauna_peer_sync::discovery::share_dial_targets` re-checks on the way out:
/// the same cross-check, run at both ends, so neither a lying advertiser nor
/// a tampered store can point a dial at a node its named member does not
/// control.
///
/// **Refuse, never repair.** A mismatch is not normalized into the proven
/// identity — a payload that lies about who it is has said nothing
/// trustworthy about where that peer can be reached either, and silently
/// rewriting it would durably record a hostile peer's candidates under an
/// honest member's name.
pub fn bind_share_advertisement(
    advertised: &ShareEndpoints,
    sender: ActorId,
    channel_id: &[u8; 32],
) -> Result<(String, ShareEndpoints), AdvertisementRefusal> {
    if advertised.member_actor.as_slice() != sender.0.as_slice() {
        tracing::warn!(
            claimed = %hex::encode(advertised.member_actor.as_slice()),
            proven = %fauna_core::hex32::encode(&sender.0),
            "share endpoint advertisement names a member other than its \
             channel-proven sender — refused"
        );
        return Err(AdvertisementRefusal::NotTheSender);
    }
    if advertised.channel_id.as_slice() != channel_id.as_slice() {
        tracing::warn!(
            claimed = %hex::encode(advertised.channel_id.as_slice()),
            carrying = %fauna_core::hex32::encode(channel_id),
            "share endpoint advertisement names a set other than the channel \
             it arrived on — refused"
        );
        return Err(AdvertisementRefusal::WrongSet);
    }
    if advertised.endpoints.node_id != sender.0 {
        tracing::warn!(
            "share endpoint advertisement's node_id is not the advertiser's \
             actor key — refused"
        );
        return Err(AdvertisementRefusal::NodeIdNotTheActor);
    }

    Ok((
        share_entry_key(channel_id, &sender.0),
        ShareEndpoints {
            channel_id: channel_id.to_vec(),
            member_actor: sender.0.to_vec(),
            endpoints: advertised.endpoints.clone(),
        },
    ))
}

/// Compose this device's own advertisement for one set — the publish half.
///
/// `own_actor` is this account's actor key (the share leg's NodeId, PT-1b)
/// and `endpoints` this device's candidates, composed exactly as the
/// same-account leg composes its own entry
/// (`fauna_peer_sync::discovery::discover_lan_candidates` in production,
/// injected in tests, so nothing here touches live interfaces).
///
/// The `node_id` is overwritten with `own_actor` rather than trusted from the
/// caller: the publish side must not be able to advertise a NodeId it does
/// not hold, which is the same rule the ingest side enforces from the other
/// direction — a caller that got it wrong now fails at its own door instead
/// of at every recipient's.
pub fn own_advertisement(
    channel_id: &[u8; 32],
    own_actor: &ActorId,
    endpoints: fauna_core::device_endpoints::DeviceEndpoints,
) -> ShareEndpoints {
    ShareEndpoints {
        channel_id: channel_id.to_vec(),
        member_actor: own_actor.0.to_vec(),
        endpoints: fauna_core::device_endpoints::DeviceEndpoints {
            node_id: own_actor.0,
            ..endpoints
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::device_endpoints::DeviceEndpoints;

    const SET: [u8; 32] = [0x5E; 32];
    const OTHER_SET: [u8; 32] = [0x5F; 32];
    const MEMBER: [u8; 32] = [0x3B; 32];
    const THIRD: [u8; 32] = [0x99; 32];

    fn candidates(node_id: [u8; 32]) -> DeviceEndpoints {
        DeviceEndpoints {
            node_id,
            lan_addrs: vec!["192.168.1.9:4433".into()],
            public_addrs: vec!["203.0.113.9:4433".into()],
            relay_url: Some("https://relay.example/".into()),
        }
    }

    fn advert(set: [u8; 32], member: [u8; 32], node_id: [u8; 32]) -> ShareEndpoints {
        ShareEndpoints {
            channel_id: set.to_vec(),
            member_actor: member.to_vec(),
            endpoints: candidates(node_id),
        }
    }

    /// The happy path: a member advertising itself over its own set's
    /// channel yields the row that set's dial reader will consume, keyed by
    /// the proven identity.
    #[test]
    fn a_member_advertising_itself_is_bound_and_keyed_by_the_proven_identity() {
        let (key, row) =
            bind_share_advertisement(&advert(SET, MEMBER, MEMBER), ActorId(MEMBER), &SET).unwrap();

        assert_eq!(key, share_entry_key(&SET, &MEMBER));
        assert_eq!(row.member_actor, MEMBER.to_vec());
        assert_eq!(row.channel_id, SET.to_vec());
        assert_eq!(row.endpoints, candidates(MEMBER));
    }

    /// The whole point of the binding: an admitted member cannot advertise
    /// a THIRD member's location. Without this, any member of a set could
    /// durably redirect every other member's dials at a box it controls.
    #[test]
    fn an_advertisement_naming_a_third_member_is_refused() {
        assert_eq!(
            bind_share_advertisement(&advert(SET, THIRD, THIRD), ActorId(MEMBER), &SET),
            Err(AdvertisementRefusal::NotTheSender),
        );
    }

    /// Refused, not repaired — the proven sender does not rescue a lying
    /// payload by having its candidates filed under the right name.
    #[test]
    fn a_lying_advertisement_is_never_rewritten_into_a_row() {
        let refused = bind_share_advertisement(&advert(SET, THIRD, THIRD), ActorId(MEMBER), &SET);
        assert!(
            refused.is_err(),
            "a mismatch must refuse rather than normalize: rewriting it would \
             durably record a hostile peer's candidates under an honest name"
        );
    }

    /// The carrying channel is the only proof of set membership this path
    /// has, so an advertisement naming a different set is refused even
    /// though its author is a proven member of the channel it arrived on.
    #[test]
    fn an_advertisement_naming_another_set_is_refused() {
        assert_eq!(
            bind_share_advertisement(&advert(OTHER_SET, MEMBER, MEMBER), ActorId(MEMBER), &SET),
            Err(AdvertisementRefusal::WrongSet),
        );
    }

    /// The share leg dials actor keys; a NodeId that is not the advertiser's
    /// actor is either malformed or a redirect attempt.
    #[test]
    fn an_advertisement_whose_node_id_is_not_its_actor_is_refused() {
        assert_eq!(
            bind_share_advertisement(&advert(SET, MEMBER, THIRD), ActorId(MEMBER), &SET),
            Err(AdvertisementRefusal::NodeIdNotTheActor),
        );
    }

    /// The publish side stamps its own NodeId rather than trusting the
    /// caller's — a wrong one fails here instead of at every recipient.
    #[test]
    fn the_publish_side_stamps_its_own_actor_as_the_node_id() {
        let mine = own_advertisement(&SET, &ActorId(MEMBER), candidates(THIRD));
        assert_eq!(mine.endpoints.node_id, MEMBER);
        assert_eq!(mine.member_actor, MEMBER.to_vec());
        assert_eq!(mine.channel_id, SET.to_vec());
    }

    /// Publish → ingest is a closed loop: what this device advertises is
    /// exactly what a member on the other side binds and stores.
    #[test]
    fn an_own_advertisement_round_trips_through_the_binding() {
        let mine = own_advertisement(&SET, &ActorId(MEMBER), candidates(MEMBER));
        let bytes = fauna_core::encoding::canonical_encode(&mine).unwrap();
        let received: ShareEndpoints = fauna_core::encoding::canonical_decode(&bytes).unwrap();

        let (key, row) = bind_share_advertisement(&received, ActorId(MEMBER), &SET).unwrap();
        assert_eq!(key, share_entry_key(&SET, &MEMBER));
        assert_eq!(row, mine);
    }
}
