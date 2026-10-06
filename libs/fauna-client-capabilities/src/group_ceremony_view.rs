//! The offline share-initiation **affordance's** paint-ready projection —
//! the half of `p2p.md` § Offline share initiation that every app renders
//! identically (contract point 1's in-person key-verification affordance).
//!
//! The eight element ids this backs are rule-A user-approved 2026-08-17 on
//! ui.yaml's `folders` page: `offline-share-button`,
//! `offline-receive-button`, `offline-share-own-code`,
//! `offline-share-peer-code-input`, `offline-share-begin-button`,
//! `offline-receive-expect-button`, `offline-share-status`,
//! `offline-share-cancel-button`. The **consent card mints no ids** — it
//! reuses the knock trio (`folder-pending-share` + accept/decline) each app
//! already paints — but its ROWS are projected here too
//! ([`pending_group_invitations`]), because deciding when consent is still
//! outstanding is exactly the kind of judgement seven apps must not each make
//! for themselves.
//!
//! # Why a shared projection at all
//!
//! Seven apps otherwise each decide when Begin is clickable, what counts as
//! a valid compare code, and which of the two panels is open — seven chances
//! to disagree about a security-relevant gesture. [`OfflineShareView`] is the
//! whole decision surface; an app resolves [`status_label`] /
//! [`code_error_label`] through its own i18n pipeline and paints. **No text
//! lives here**: those doors return an i18n *key*, never a sentence, so the
//! strings stay in `i18n/strings/en.yaml` where they belong.
//!
//! # The code is the actor key AND the addressing
//!
//! The first build shipped the code as the actor key alone, and measured the
//! consequence: nothing could dial, because the actor key is an identity and
//! not a location (`p2p.md` § Offline share initiation → *Measured — the
//! co-present dial has no discovery*). A nest-free ceremony has no nest to
//! advertise addresses through, and § LAN detection's arithmetic keys on a
//! *peer-advertised* address it therefore never receives.
//!
//! So **the code itself is the advertisement channel**: [`format_peer_code`]
//! appends this device's bound LAN endpoints, [`parse_peer_code`] returns
//! them as a [`PeerCode`], and the dial hands them over as `PathCandidates`.
//! This is the resolution of the three the goal doc named, chosen 2026-08-19:
//! it costs no new third-party dependency, and it works between two terminals
//! — which a camera-scanned QR does not, tui being the lead app.
//!
//! The widening is **additive**: a bare 64-hex key still parses, carrying no
//! addressing, exactly as before.
//!
//! [`parse_peer_code`] is deliberately strict, and its refusals are the
//! security-relevant part: whitespace is forgiven (a code read aloud gets
//! typed with stray spaces), but a malformed key or candidate is refused
//! rather than repaired, and **your own code is refused** — a self-dial would
//! mint a scope shared with nobody and is far likelier to be a mis-paste than
//! an intent.

use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

/// Which half of the co-present affordance is open. Exactly one at a time:
/// the two roles are opposite ends of one ceremony, and a device that is
/// mid-initiation is not also awaiting one.
///
/// `Serialize`/`Deserialize` always, `uniffi::Enum` behind the off-by-default
/// `uniffi` feature — the `custody_view`/`fauna-client-pair::trust` split
/// (one row type, two faces): `fauna-ffi` turns the feature on for the four
/// native apps, the web SPA's wasm crate reads the same type through serde
/// with it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum OfflineSharePanel {
    /// Neither panel — the page shows only the two entry buttons.
    #[default]
    Closed,
    /// The initiator's panel: read your code out, type theirs, Begin.
    Initiate,
    /// The recipient's panel: read your code out, type theirs, Expect.
    Receive,
}

/// Where this device's side of the ceremony has got to — what
/// `offline-share-status` reports. A state, never a sentence: [`status_label`]
/// names the i18n key, and each app resolves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum CeremonyStatus {
    /// Nothing in flight.
    #[default]
    Idle,
    /// Recipient: the receive act minted the expectation; the initiator's
    /// offer may now arrive.
    Expecting,
    /// Initiator: the scope is minted and the offer is on the wire.
    OfferSent,
    /// Initiator: the recipient's user has not answered yet.
    AwaitingConsent,
    /// Initiator: consent arrived; the deliver is being built and pushed.
    Delivering,
    /// Initiator: the deliver crossed — done on this side.
    Delivered,
    /// Recipient: the delivered machinery verified and was adopted.
    Admitted,
    /// The ceremony stopped short. The *reason* rides the page's
    /// `error-message`, never this enum (e2e convention 2).
    Failed,
}

impl CeremonyStatus {
    /// Whether a ceremony is in flight — the cancel affordance's gate, and
    /// what stops a second Begin from minting a competing scope.
    pub fn in_flight(self) -> bool {
        matches!(
            self,
            Self::Expecting | Self::OfferSent | Self::AwaitingConsent | Self::Delivering
        )
    }

    /// Whether this side's ceremony just landed a scope this device can list:
    /// the initiator's deliver crossed, or the recipient's admission ran. The
    /// folders page re-reads its group listing on this edge, so the set the
    /// user just shared or accepted lists on the page they are looking at — a
    /// landed scope lists as an ordinary set row (`p2p.md` § Offline share
    /// initiation) — rather than only after they navigate away and back.
    pub fn lands_a_scope(self) -> bool {
        matches!(self, Self::Delivered | Self::Admitted)
    }
}

/// Why a typed compare code was refused. [`code_error_label`] names each
/// refusal's i18n key and the app resolves it; the discrimination and the
/// reading are both shared, so seven apps refuse identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum PeerCodeError {
    /// Nothing typed yet.
    #[error("no code entered")]
    Empty,
    /// Not a 64-hex actor key.
    #[error("not a valid compare code")]
    Malformed,
    /// The user typed their OWN code — a mis-paste, not an intent.
    #[error("that is this device's own code")]
    OwnCode,
}

/// The `offline-share-status` reading for a state, as a [`LocalizedText`]
/// each app resolves through its own i18n pipeline.
///
/// The state → **key** decision is a fact and lives here, exactly once; the
/// **sentence** is still each app's to resolve. That is the same split
/// `p2p.md` § Cross-user shared-set transfer already ratifies for this page's
/// other readings ("the six ids' readings as `LocalizedText`", under *what is
/// shared, and therefore never re-derived by a leg*) and that
/// `fauna_sync_engine::share_glue::serve_status_label` implements for the
/// `share-serve-status` line one section up the same page.
///
/// This lives beside the enum rather than in `fauna_sync_engine::share_glue`
/// with its sibling for two reasons. An app that renders the status does not
/// otherwise need the sync engine — linux and tui reach it as a direct dep.
/// And this crate is the one the *other* legs already cross: `fauna-ffi`
/// re-exports both doors (`offline_share_status_label` /
/// `offline_share_code_error_label`) so windows/macOS/iOS/android inherit the
/// mapping instead of writing a third copy of it, which the enum's UniFFI
/// face alone would have left them to do ([`LocalizedText`] is itself a
/// `uniffi::Record`). It costs the wasm build nothing: this crate is
/// wasm-clean by default and the doors are plain data.
pub fn status_label(status: CeremonyStatus) -> LocalizedText {
    match status {
        CeremonyStatus::Idle => LocalizedText::key("folders.offline_share_status_idle"),
        CeremonyStatus::Expecting => LocalizedText::key("folders.offline_share_status_expecting"),
        CeremonyStatus::OfferSent => LocalizedText::key("folders.offline_share_status_offer_sent"),
        CeremonyStatus::AwaitingConsent => {
            LocalizedText::key("folders.offline_share_status_awaiting_consent")
        }
        CeremonyStatus::Delivering => LocalizedText::key("folders.offline_share_status_delivering"),
        CeremonyStatus::Delivered => LocalizedText::key("folders.offline_share_status_delivered"),
        CeremonyStatus::Admitted => LocalizedText::key("folders.offline_share_status_admitted"),
        CeremonyStatus::Failed => LocalizedText::key("folders.offline_share_status_failed"),
    }
}

/// The reading for a refused compare code, as a [`LocalizedText`] each app
/// resolves — or `None` when the refusal is not one to shout.
///
/// [`PeerCodeError::Empty`] is deliberately silent: nothing typed yet is not
/// a mistake, and the act button being disabled is the honest signal. That
/// judgement is part of the same shared decision — an app that painted a
/// scolding message on an empty box would be diverging on behavior, not on
/// wording.
pub fn code_error_label(e: PeerCodeError) -> Option<LocalizedText> {
    match e {
        PeerCodeError::Empty => None,
        PeerCodeError::Malformed => {
            Some(LocalizedText::key("folders.offline_share_code_malformed"))
        }
        PeerCodeError::OwnCode => Some(LocalizedText::key("folders.offline_share_code_own")),
    }
}

/// Separates the actor key from each addressing candidate. A dash is what
/// people already use to group a long code, and it cannot collide with hex.
pub const CODE_SEPARATOR: char = '-';

/// Hex width of one candidate: four octets of IPv4 plus a two-byte port.
const CANDIDATE_HEX_LEN: usize = 12;

/// The most candidates a compare code will ever carry.
///
/// This is a **UX budget, not a parse bound**: a human transcribes this code
/// while standing next to the other person, so every extra candidate costs 13
/// more characters to read out. Four covers wifi + ethernet + a VPN or bridge
/// on the machines people actually pair; a host with more interfaces than
/// that publishes its first four rather than an untypeable code. The dial
/// only needs *one* of them to be reachable.
pub const MAX_CODE_CANDIDATES: usize = 4;

/// A parsed compare code: **who** to dial, and **where** to try.
///
/// The addressing half is what makes a nest-free ceremony dialable at all.
/// The actor key alone leaves iroh with an `EndpointId` and no path, which is
/// exactly the `No addressing information available` the first build measured
/// (`p2p.md` § Offline share initiation → *Measured — the co-present dial has
/// no discovery*). **The code itself is the advertisement channel**, because a
/// co-present ceremony has no nest to advertise through — which is precisely
/// the gap § LAN detection's peer-advertised arithmetic could not close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCode {
    /// The counterpart's actor key — which IS the dial's `NodeId` (PT-1b), so
    /// no registry lookup is involved and a wrong key can only ever fail to
    /// connect, never reach the wrong person.
    pub actor: ActorId,
    /// Where to try. Peer-advertised and therefore attacker-influenceable, so
    /// these ride the **existing** PT-4 filter
    /// (`fauna_transport::is_safe_candidate`, applied inside the transport's
    /// `dial`) rather than a second filter here — one audited hygiene, no
    /// per-surface drift (priority #4).
    pub lan_endpoints: Vec<SocketAddr>,
}

impl PeerCode {
    /// Parse without the own-code check — [`parse_peer_code`] is the form an
    /// app calls. This exists for reading back a code this device *minted*,
    /// where "it is your own" is the point rather than a refusal.
    pub fn parse(input: &str) -> Result<Self, PeerCodeError> {
        let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
        if cleaned.is_empty() {
            return Err(PeerCodeError::Empty);
        }
        let mut parts = cleaned.split(CODE_SEPARATOR);
        let actor = ActorId::from_hex(parts.next().unwrap_or_default())
            .map_err(|_| PeerCodeError::Malformed)?;
        let mut lan_endpoints = Vec::new();
        for segment in parts {
            if lan_endpoints.len() >= MAX_CODE_CANDIDATES {
                return Err(PeerCodeError::Malformed);
            }
            lan_endpoints.push(parse_candidate(segment)?);
        }
        Ok(Self {
            actor,
            lan_endpoints,
        })
    }
}

fn parse_candidate(segment: &str) -> Result<SocketAddr, PeerCodeError> {
    if segment.len() != CANDIDATE_HEX_LEN {
        return Err(PeerCodeError::Malformed);
    }
    let mut raw = [0u8; 6];
    for (i, byte) in raw.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&segment[i * 2..i * 2 + 2], 16)
            .map_err(|_| PeerCodeError::Malformed)?;
    }
    Ok(SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]),
        u16::from_be_bytes([raw[4], raw[5]]),
    )))
}

/// Render this device's compare code: the actor key, then the addressing the
/// other side's dial needs.
///
/// IPv6 candidates are **left out** rather than rendered: one would cost 32
/// hex characters a human has to read aloud, and § LAN detection's own
/// arithmetic is IPv4-only. This is a rendering choice about *our own*
/// addresses — not the repair-vs-refuse rule, which governs what we accept
/// from someone else.
pub fn format_peer_code(actor: &ActorId, lan_endpoints: &[SocketAddr]) -> String {
    let mut code = actor.to_hex();
    let mut taken: Vec<SocketAddrV4> = Vec::new();
    for endpoint in lan_endpoints {
        if taken.len() >= MAX_CODE_CANDIDATES {
            break;
        }
        let SocketAddr::V4(v4) = endpoint else {
            continue;
        };
        if taken.contains(v4) {
            continue;
        }
        taken.push(*v4);
        let octets = v4.ip().octets();
        code.push(CODE_SEPARATOR);
        code.push_str(&format!(
            "{:02x}{:02x}{:02x}{:02x}{:04x}",
            octets[0],
            octets[1],
            octets[2],
            octets[3],
            v4.port()
        ));
    }
    code
}

/// Parse the code the other side read out. Whitespace anywhere is forgiven
/// (it is a code read aloud and typed back); everything else is refused
/// rather than repaired — a garbled candidate refuses the whole code, so the
/// typo surfaces under the input while the other person is still standing
/// there, instead of as a failure-to-connect minutes later.
pub fn parse_peer_code(input: &str, own: &ActorId) -> Result<PeerCode, PeerCodeError> {
    let code = PeerCode::parse(input)?;
    if code.actor == *own {
        return Err(PeerCodeError::OwnCode);
    }
    Ok(code)
}

/// The whole paint decision for the offline-share affordance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct OfflineShareView {
    /// Which panel (if any) is open.
    pub panel: OfflineSharePanel,
    /// This device's own compare code — the actor key as hex. Empty when the
    /// affordance is unavailable (see [`Self::available`]).
    pub own_code: String,
    /// The typed counterpart code, verbatim as the user typed it.
    pub peer_code: String,
    /// This side's progress.
    pub status: CeremonyStatus,
    /// Whether the ceremony listener bound at all. `false` means the
    /// `p2p-share` brake refused, and the two entry buttons must not render:
    /// an affordance that cannot work is worse than an absent one.
    pub available: bool,
}

impl OfflineShareView {
    /// The projection, from the state an app holds.
    /// `own_endpoints` is what this device's *bound* listener observed — empty
    /// until a panel opens and the seat binds, which is correct rather than a
    /// gap: before binding there is no port to advertise, and the code shown
    /// then is the bare key it has always been.
    pub fn new(
        panel: OfflineSharePanel,
        own: Option<&ActorId>,
        own_endpoints: &[SocketAddr],
        peer_code: &str,
        status: CeremonyStatus,
    ) -> Self {
        Self {
            panel,
            own_code: own
                .map(|a| format_peer_code(a, own_endpoints))
                .unwrap_or_default(),
            peer_code: peer_code.to_string(),
            status,
            available: own.is_some(),
        }
    }

    /// Whether `offline-share-button` / `offline-receive-button` render.
    pub fn shows_entry_buttons(&self) -> bool {
        self.available && self.panel == OfflineSharePanel::Closed
    }

    /// Whether the open panel's code widgets render.
    pub fn shows_code_widgets(&self) -> bool {
        self.available && self.panel != OfflineSharePanel::Closed
    }

    /// Whether `offline-share-begin-button` is enabled: the initiator panel
    /// is open, nothing is already in flight, and the typed code parses.
    pub fn can_begin(&self) -> bool {
        self.panel == OfflineSharePanel::Initiate && !self.status.in_flight() && self.peer_ok()
    }

    /// Whether `offline-receive-expect-button` is enabled — the recipient's
    /// mirror of [`Self::can_begin`].
    pub fn can_expect(&self) -> bool {
        self.panel == OfflineSharePanel::Receive && !self.status.in_flight() && self.peer_ok()
    }

    /// Whether `offline-share-cancel-button` renders: any open panel can be
    /// closed, and an in-flight ceremony can be abandoned.
    pub fn shows_cancel(&self) -> bool {
        self.shows_code_widgets()
    }

    /// The parsed counterpart, or why not.
    pub fn peer_code_parsed(&self) -> Result<PeerCode, PeerCodeError> {
        let own = PeerCode::parse(&self.own_code)?.actor;
        parse_peer_code(&self.peer_code, &own)
    }

    /// The parsed counterpart's actor, for callers that need only the who.
    pub fn peer_actor(&self) -> Result<ActorId, PeerCodeError> {
        self.peer_code_parsed().map(|c| c.actor)
    }

    fn peer_ok(&self) -> bool {
        self.peer_actor().is_ok()
    }
}

// ── The consent card (the knock trio's group arm) ───────────────────────────
//
// The recipient's half of the ceremony is a DECISION, and it is the same
// decision on all seven apps: this person offered me a set — take it or not.
// So the rows are projected here, out of the durable ceremony record, and
// each app paints them through the `folder-pending-share` trio it already
// has. Nothing about which button is enabled is left to an app: a row exists
// exactly while consent is genuinely outstanding.

/// One offered share awaiting this account's consent — a
/// `folder-pending-share` row on the folders page, in its group arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingGroupInvitation {
    /// The offered scope — the accept/decline target. Addressed by id, never
    /// by position, so a list that shifts under a concurrent arrival can
    /// never accept the wrong invitation (the knock list's own rule).
    pub scope_id: [u8; 32],
    /// Who offered it. The consent row shows this beside the short scope id:
    /// v1 sets are nameless, so *who* is the only other thing to show.
    pub initiator: ActorId,
    /// The scope id's first eight hex characters — the nameless set's handle.
    pub short_id: String,
}

/// Every invitation whose consent is still outstanding, oldest recorded
/// first.
///
/// Outstanding means all three of: an offer was actually recorded (an empty
/// offer is a record the ingest never completed), no accept has been built
/// yet (consent already given is not a pending decision), and the invitation
/// was not declined — the decline being monotone and fleet-wide, so a row
/// dismissed on one device never re-knocks on another.
#[must_use]
pub fn pending_group_invitations(
    cfg: &fauna_core::group_ceremony::GroupShareConfig,
) -> Vec<PendingGroupInvitation> {
    cfg.invited
        .iter()
        .filter(|r| !r.declined && !r.offer.is_empty() && r.accept.is_empty())
        .map(|r| PendingGroupInvitation {
            scope_id: r.scope_id,
            initiator: r.initiator,
            short_id: short_scope_id(&r.scope_id),
        })
        .collect()
}

/// Every group scope this account's ceremony record knows about, either side,
/// declines excluded — the id list a folders page hands the store to find out
/// which of them actually landed.
///
/// Deliberately NOT a listing: this says "a ceremony touched this scope", and
/// only the store can say whether its machinery rows are here. The two-step
/// (config names the candidates, the plane confirms) is what keeps a
/// half-finished ceremony from painting a set the device cannot read.
#[must_use]
pub fn known_group_scopes(cfg: &fauna_core::group_ceremony::GroupShareConfig) -> Vec<[u8; 32]> {
    let mut scopes: Vec<[u8; 32]> = cfg
        .initiated
        .iter()
        .map(|r| r.scope_id)
        .chain(
            cfg.invited
                .iter()
                .filter(|r| !r.declined)
                .map(|r| r.scope_id),
        )
        .collect();
    scopes.sort_unstable();
    scopes.dedup();
    scopes
}

/// Has this account already consented to `scope_id`, and is the delivery not
/// yet admitted? The state an accepted card paints while the initiator's
/// deliver is still on its way.
#[must_use]
pub fn awaiting_delivery(
    cfg: &fauna_core::group_ceremony::GroupShareConfig,
    scope_id: &[u8; 32],
) -> bool {
    cfg.invited
        .iter()
        .any(|r| r.scope_id == *scope_id && !r.accept.is_empty() && !r.rows_adopted)
}

/// The first eight hex characters of a scope id — the one truncation every
/// app shows for a nameless v1 set. Shared so two apps cannot disagree about
/// how much of an id a user is asked to compare.
#[must_use]
pub fn short_scope_id(scope_id: &[u8; 32]) -> String {
    fauna_core::hex32::encode(scope_id)
        .chars()
        .take(8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;
    use std::net::SocketAddr;

    fn me() -> ActorId {
        ActorKeypair::from_secret([7u8; 32]).actor_id()
    }

    fn them() -> ActorId {
        ActorKeypair::from_secret([9u8; 32]).actor_id()
    }

    #[test]
    fn a_code_read_aloud_survives_the_spaces_it_is_typed_with() {
        let spaced = format!(
            "{} {}  \n{}",
            &them().to_hex()[..20],
            &them().to_hex()[20..40],
            &them().to_hex()[40..]
        );
        assert_eq!(parse_peer_code(&spaced, &me()).map(|c| c.actor), Ok(them()));
    }

    #[test]
    fn a_malformed_or_own_code_is_refused_never_repaired() {
        assert_eq!(parse_peer_code("", &me()), Err(PeerCodeError::Empty));
        assert_eq!(parse_peer_code("   ", &me()), Err(PeerCodeError::Empty));
        assert_eq!(
            parse_peer_code("nothex", &me()),
            Err(PeerCodeError::Malformed)
        );
        // One character short of a key is malformed, not truncatable.
        assert_eq!(
            parse_peer_code(&them().to_hex()[..63], &me()),
            Err(PeerCodeError::Malformed)
        );
        // The self-dial footgun.
        assert_eq!(
            parse_peer_code(&me().to_hex(), &me()),
            Err(PeerCodeError::OwnCode)
        );
    }

    #[test]
    fn a_code_carries_the_addressing_the_dial_needs() {
        // The whole point of the widening: a code the other side types must
        // arrive with somewhere to dial, or iroh answers "No addressing
        // information available" (p2p.md § Offline share initiation).
        let eps: Vec<SocketAddr> = vec![
            "192.168.1.42:41234".parse().unwrap(),
            "10.0.0.5:41234".parse().unwrap(),
        ];
        let code = format_peer_code(&them(), &eps);
        let parsed = parse_peer_code(&code, &me()).expect("own code round-trips");
        assert_eq!(parsed.actor, them());
        assert_eq!(parsed.lan_endpoints, eps);
    }

    #[test]
    fn the_widened_code_still_survives_the_spaces_it_is_typed_with() {
        let eps: Vec<SocketAddr> = vec!["192.168.1.42:41234".parse().unwrap()];
        let code = format_peer_code(&them(), &eps);
        let spaced = format!("{}  \n {}", &code[..30], &code[30..]);
        let parsed = parse_peer_code(&spaced, &me()).expect("whitespace is forgiven");
        assert_eq!(parsed.actor, them());
        assert_eq!(parsed.lan_endpoints, eps);
    }

    #[test]
    fn a_bare_key_still_parses_so_the_spoken_form_survives() {
        // ADDITIVE, not a replacement: a code read aloud is still a code.
        // It simply carries no addressing, which is the pre-existing state.
        let parsed = parse_peer_code(&them().to_hex(), &me()).expect("a bare key is a code");
        assert_eq!(parsed.actor, them());
        assert!(parsed.lan_endpoints.is_empty());
    }

    #[test]
    fn a_garbled_candidate_is_refused_never_dropped() {
        // Refusing beats repairing: a dropped candidate turns a typo into a
        // mysterious failure-to-connect minutes later, while a refusal shows
        // up under the input while the other person is still standing there.
        let good = format_peer_code(&them(), &["192.168.1.42:41234".parse().unwrap()]);
        let garbled = format!("{}ff", &good[..good.len() - 1]);
        assert_eq!(
            parse_peer_code(&garbled, &me()),
            Err(PeerCodeError::Malformed)
        );
    }

    #[test]
    fn the_candidate_list_is_capped_so_the_code_stays_typeable() {
        let mut eps: Vec<SocketAddr> = Vec::new();
        for i in 0..(MAX_CODE_CANDIDATES + 3) {
            eps.push(format!("192.168.1.{i}:41234").parse().unwrap());
        }
        let code = format_peer_code(&them(), &eps);
        let parsed = parse_peer_code(&code, &me()).expect("a capped code still parses");
        assert_eq!(parsed.lan_endpoints.len(), MAX_CODE_CANDIDATES);
        assert_eq!(parsed.lan_endpoints[0], eps[0]);
    }

    #[test]
    fn a_repeated_interface_does_not_spend_one_of_the_four_slots() {
        // Two bound sockets crossed with the same interface list is the
        // ordinary case, not a pathological one — the dedupe is what keeps a
        // two-port machine from burning the whole budget on one address.
        let a: SocketAddr = "192.168.1.42:41234".parse().unwrap();
        let b: SocketAddr = "10.0.0.5:41234".parse().unwrap();
        let code = format_peer_code(&them(), &[a, b, a, b, a]);
        let parsed = parse_peer_code(&code, &me()).expect("round-trips");
        assert_eq!(parsed.lan_endpoints, vec![a, b]);
    }

    #[test]
    fn an_ipv6_candidate_is_left_out_rather_than_making_the_code_untypeable() {
        // v1 is IPv4-only, matching § LAN detection's own arithmetic. An
        // IPv6 candidate would cost 32 hex characters a human has to read
        // out; it is dropped at FORMAT time (our own address, not a peer's,
        // so this is a rendering choice and not the repair-vs-refuse rule).
        let eps: Vec<SocketAddr> = vec![
            "[fd00::1]:41234".parse().unwrap(),
            "192.168.1.42:41234".parse().unwrap(),
        ];
        let code = format_peer_code(&them(), &eps);
        let parsed = parse_peer_code(&code, &me()).expect("round-trips");
        assert_eq!(
            parsed.lan_endpoints,
            vec!["192.168.1.42:41234".parse::<SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn your_own_widened_code_is_still_refused() {
        let code = format_peer_code(&me(), &["192.168.1.42:41234".parse().unwrap()]);
        assert_eq!(parse_peer_code(&code, &me()), Err(PeerCodeError::OwnCode));
    }

    #[test]
    fn the_entry_buttons_hide_when_the_brake_refused_the_listener() {
        let unavailable = OfflineShareView::new(
            OfflineSharePanel::Closed,
            None,
            &[],
            "",
            CeremonyStatus::Idle,
        );
        assert!(!unavailable.available);
        assert!(!unavailable.shows_entry_buttons());
        assert!(!unavailable.shows_code_widgets());

        let available = OfflineShareView::new(
            OfflineSharePanel::Closed,
            Some(&me()),
            &[],
            "",
            CeremonyStatus::Idle,
        );
        assert!(available.shows_entry_buttons());
        // Closed: no code widgets, and nothing to cancel.
        assert!(!available.shows_code_widgets());
        assert!(!available.shows_cancel());
    }

    #[test]
    fn begin_and_expect_gate_on_their_own_panel_a_parsed_code_and_an_idle_ceremony() {
        let good = them().to_hex();

        let initiate = OfflineShareView::new(
            OfflineSharePanel::Initiate,
            Some(&me()),
            &[],
            &good,
            CeremonyStatus::Idle,
        );
        assert!(initiate.can_begin());
        // Expect belongs to the other panel, even with a perfect code.
        assert!(!initiate.can_expect());
        assert!(initiate.shows_code_widgets());
        assert!(initiate.shows_cancel());
        // The entry buttons are gone while a panel is open.
        assert!(!initiate.shows_entry_buttons());

        let receive = OfflineShareView::new(
            OfflineSharePanel::Receive,
            Some(&me()),
            &[],
            &good,
            CeremonyStatus::Idle,
        );
        assert!(receive.can_expect());
        assert!(!receive.can_begin());

        // An unparseable code disables both, whichever panel is open.
        for panel in [OfflineSharePanel::Initiate, OfflineSharePanel::Receive] {
            let bad = OfflineShareView::new(panel, Some(&me()), &[], "nope", CeremonyStatus::Idle);
            assert!(!bad.can_begin());
            assert!(!bad.can_expect());
        }

        // An in-flight ceremony blocks a second mint — the whole point of
        // `in_flight`.
        for status in [
            CeremonyStatus::Expecting,
            CeremonyStatus::OfferSent,
            CeremonyStatus::AwaitingConsent,
            CeremonyStatus::Delivering,
        ] {
            assert!(status.in_flight(), "{status:?} must count as in flight");
            let busy =
                OfflineShareView::new(OfflineSharePanel::Initiate, Some(&me()), &[], &good, status);
            assert!(!busy.can_begin(), "{status:?} must block a second Begin");
            // …but cancel stays reachable, or an in-flight ceremony would be
            // a trap.
            assert!(busy.shows_cancel());
        }

        // Terminal states release the gate: the user may run another one.
        for status in [
            CeremonyStatus::Idle,
            CeremonyStatus::Delivered,
            CeremonyStatus::Admitted,
            CeremonyStatus::Failed,
        ] {
            assert!(
                !status.in_flight(),
                "{status:?} must not count as in flight"
            );
        }
    }

    /// Exactly the two statuses that land a scope this device can list — the
    /// edge the folders page re-reads its group listing on. Every other status
    /// lands nothing, so re-reading on it would only be noise.
    #[test]
    fn only_a_delivered_or_admitted_ceremony_lands_a_scope() {
        for status in [CeremonyStatus::Delivered, CeremonyStatus::Admitted] {
            assert!(status.lands_a_scope(), "{status:?} lands a scope");
        }
        for status in [
            CeremonyStatus::Idle,
            CeremonyStatus::Expecting,
            CeremonyStatus::OfferSent,
            CeremonyStatus::AwaitingConsent,
            CeremonyStatus::Delivering,
            CeremonyStatus::Failed,
        ] {
            assert!(!status.lands_a_scope(), "{status:?} lands nothing");
        }
    }

    // ── The consent card's rows ─────────────────────────────────────────────

    fn invited(scope: u8, initiator: ActorId) -> fauna_core::group_ceremony::InvitedGroupShare {
        fauna_core::group_ceremony::InvitedGroupShare {
            scope_id: [scope; 32],
            initiator,
            offer: vec![1, 2, 3],
            ..Default::default()
        }
    }

    fn config_with(
        invited_rows: Vec<fauna_core::group_ceremony::InvitedGroupShare>,
    ) -> fauna_core::group_ceremony::GroupShareConfig {
        fauna_core::group_ceremony::GroupShareConfig {
            invited: invited_rows,
            ..Default::default()
        }
    }

    #[test]
    fn consent_is_outstanding_only_while_it_genuinely_is() {
        let mut accepted = invited(0x02, them());
        accepted.accept = vec![9]; // already consented
        let mut declined = invited(0x03, them());
        declined.declined = true;
        let mut half_ingested = invited(0x04, them());
        half_ingested.offer.clear(); // the record exists, the offer never landed

        let cfg = config_with(vec![
            invited(0x01, them()),
            accepted,
            declined,
            half_ingested,
        ]);

        let rows = pending_group_invitations(&cfg);
        assert_eq!(rows.len(), 1, "exactly the undecided offer: {rows:?}");
        assert_eq!(rows[0].scope_id, [0x01; 32]);
        assert_eq!(rows[0].initiator, them());
        assert_eq!(rows[0].short_id, short_scope_id(&[0x01; 32]));
        assert_eq!(rows[0].short_id.len(), 8);
    }

    #[test]
    fn a_declined_invitation_never_re_knocks() {
        let mut declined = invited(0x05, them());
        declined.declined = true;
        // Even with an accept recorded elsewhere in the fleet, the monotone
        // decline is the absorbing answer.
        assert!(pending_group_invitations(&config_with(vec![declined])).is_empty());
    }

    #[test]
    fn known_scopes_span_both_sides_and_drop_declines() {
        let mut cfg = config_with(vec![invited(0x0A, them())]);
        let mut declined = invited(0x0B, them());
        declined.declined = true;
        cfg.invited.push(declined);
        cfg.initiated
            .push(fauna_core::group_ceremony::InitiatedGroupShare {
                scope_id: [0x0C; 32],
                recipient: them(),
                ..Default::default()
            });

        assert_eq!(
            known_group_scopes(&cfg),
            vec![[0x0A; 32], [0x0C; 32]],
            "a declined invitation is not a scope this account is in"
        );
    }

    #[test]
    fn awaiting_delivery_spans_exactly_the_gap_between_consent_and_admission() {
        let mut consented = invited(0x11, them());
        consented.accept = vec![7];
        let cfg = config_with(vec![consented.clone()]);
        assert!(awaiting_delivery(&cfg, &[0x11; 32]));

        // Before consent: not waiting on a delivery, waiting on the user.
        assert!(!awaiting_delivery(
            &config_with(vec![invited(0x11, them())]),
            &[0x11; 32]
        ));

        // After adoption: the set is listed, nothing is owed.
        let mut admitted = consented;
        admitted.rows_adopted = true;
        assert!(!awaiting_delivery(
            &config_with(vec![admitted]),
            &[0x11; 32]
        ));
    }

    /// Every ceremony state carries its own key — two sharing one would make
    /// `offline-share-status` lie. Resolution stays each app's, and linux and
    /// tui pin the *rendered* readings on their own side, so a key missing
    /// from the string table is caught where it would be painted.
    #[test]
    fn every_ceremony_status_has_its_own_key() {
        let all = [
            CeremonyStatus::Idle,
            CeremonyStatus::Expecting,
            CeremonyStatus::OfferSent,
            CeremonyStatus::AwaitingConsent,
            CeremonyStatus::Delivering,
            CeremonyStatus::Delivered,
            CeremonyStatus::Admitted,
            CeremonyStatus::Failed,
        ];
        let keys: std::collections::BTreeSet<String> =
            all.iter().map(|s| status_label(*s).key).collect();
        assert_eq!(
            keys.len(),
            all.len(),
            "two states sharing a reading would make the status element lie"
        );
        for k in &keys {
            assert!(
                k.starts_with("folders.offline_share_status_"),
                "an off-page key would resolve to someone else's sentence: {k}"
            );
        }
    }

    /// An empty box is not a scolding — the act button being disabled is the
    /// honest signal there. The other two refusals both speak, and they must
    /// not say the same thing.
    #[test]
    fn an_empty_code_is_silent_but_a_wrong_one_speaks() {
        assert!(code_error_label(PeerCodeError::Empty).is_none());
        let malformed = code_error_label(PeerCodeError::Malformed).expect("malformed speaks");
        let own = code_error_label(PeerCodeError::OwnCode).expect("own-code speaks");
        assert_ne!(
            malformed.key, own.key,
            "'not a code' and 'that is this device's own code' are different mistakes"
        );
        for t in [&malformed, &own] {
            assert!(
                t.key.starts_with("folders.offline_share_code_"),
                "an off-page key would resolve to someone else's sentence: {}",
                t.key
            );
            assert!(t.args.is_empty(), "neither refusal substitutes anything");
        }
    }
}
