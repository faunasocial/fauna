//! Client-set `[nest]`-policy WS-RPC payload types —
//! `fauna.admin.set_subhandles` (the bool toggle),
//! `fauna.admin.set_max_storage_bytes` (the nullable-int capacity cap),
//! and `fauna.admin.set_cors_origins` (the string-list of trusted browser
//! origins). The wire surface that makes the nest's
//! subhandle advertisement, storage cap, and CORS allow-list client-set instead
//! of CLI/config-baked (a product invariant: nest config a user/admin
//! picks comes from clients; the CLI/env value is the pre-claim seed only).
//!
//! These run on the admin's **authed** connection (Admin-class via
//! `bridge_method_allowlist`), mirroring `set_mail_enabled` — *not* the
//! pre-identity signed ceremony `nat_mode` uses (those are post-claim toggles
//! with sensible fresh defaults). The nest upserts the matching DB singleton and
//! swaps the live `AppState` RwLock; the admin reads the value back on
//! `fauna.setup.status` (`SetupStatusReply.subhandles`).
//!
//! Each carries only `{ enabled }` → `{ ok }`, with the `#[serde(flatten)]`
//! forward-compat catch-all (transport.md § Schema and forward-compat
//! discipline, rule 4). Kind registry: `kind.rs::register_admin_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

/// `fauna.admin.set_subhandles` — flip whether the nest advertises the
/// `handle@domain` / `@handle.domain` subhandle address forms.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetSubhandlesRequest {
    pub enabled: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetSubhandlesReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.admin.set_age_verification_required` — flip the **"accept only
/// signups carrying app age verification"** gate (`family-safety.md` § The
/// account age band, D5+D6; gating scope: `public-mode.md` § Registration
/// Modes → *Age at registration*). App-set nest state with a hard-coded
/// default of **off** and deliberately **no config/env seed** — unlike
/// `registration_mode`'s pre-claim `[nest]` seed, there is no pre-claim moment
/// where this knob matters (a nest with no admin admits nobody self-service
/// anyway), so a seed would be configuration-file theatre.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetAgeVerificationRequiredRequest {
    pub required: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetAgeVerificationRequiredReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.admin.set_max_storage_bytes` — set (or clear) the deployment-wide
/// storage/capacity cap. `Some(v)` caps the nest at `v` bytes; `None` clears the
/// cap (no limit). The int counterpart of the bool toggles above — same
/// Admin-class authed shape, but the value is the nullable `max_bytes` rather
/// than `enabled`. A missing field decodes to `None` (clear the cap).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetMaxStorageBytesRequest {
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetMaxStorageBytesReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.admin.set_cors_origins` — set (or clear) the deployment-wide list of
/// browser origins the nest's own client-facing HTTP API trusts for credentialed
/// cross-origin requests. The **list** counterpart of the bool/int knobs above —
/// same Admin-class authed shape, but the value is the whole `origins` list
/// (replace-not-merge; the admin form submits the complete list). An empty list
/// (or a missing field) decodes to `vec![]`, which the nest treats as "trust only
/// the built-in default origin" — distinct, at the DB layer, from never having
/// set it (⇒ the `config.nest.cors_origins` seed). Which web origins an admin
/// trusts is a security policy (same class as federation peers) → client-set per
/// a product invariant: the client UI is the config surface, not the
/// `--cors-origins` CLI seed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetCorsOriginsRequest {
    #[serde(default)]
    pub origins: Vec<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetCorsOriginsReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Hard-coded fallback for the client-facing API serving port when the admin has
/// never set the `serving_port` singleton — the privileged HTTPS port `443`, the
/// uniform client-facing port a nest is reached on regardless of client origin
/// (`docs/goal/architecture/nest/common.md` § Serving ports). The symmetric twin
/// of [`crate::bridge_routing::DEFAULT_CALDAV_PORT`].
pub const DEFAULT_SERVING_PORT: u16 = 443;

/// The fixed **internal-loopback** port a direct-listener nest binds for
/// co-located inter-process communication — the canonical internal port (the
/// `--bind`/`config listen` / Docker `FAUNA_PORT` default `3000`). **This is NOT
/// an admin policy** (despite living next to [`DEFAULT_SERVING_PORT`] for
/// cold-read discoverability): it is **bucket-2 artifact-wiring IPC** (the
/// "nest↔MDA↔SNI-router loopback split" bucket), a
/// hard-coded constant no human ever chooses.
///
/// On a **desktop-direct** deployment the nest binds this `127.0.0.1:<port>`
/// listener *alongside* the external `serving_port` listener, so the co-located
/// bridge + same-box app dial a **stable** port that never moves when the admin
/// changes the (external) `serving_port` — that is what keeps a port change from
/// stranding same-box clients (DoD #6). Docker already has this split intrinsically
/// (the nest binds `0.0.0.0:3000` and the SNI router fronts the external port), so
/// only the in-process Windows nest-service requests the extra listener (via the
/// `FAUNA_INTERNAL_LOOPBACK_PORT` IPC env). See `nest/common.md` § Serving ports.
pub const CANONICAL_INTERNAL_LOOPBACK_PORT: u16 = 3000;

/// The deployment's registration posture — the **one** knob that decides whether
/// (and how) a new account can be created. The client-set `nest_registration_mode`
/// DB singleton holds it; `fauna.admin.set_registration_mode` writes it; the admin
/// reads it back on `fauna.setup.status`. Owner:
/// `docs/goal/architecture/nest/public-mode.md` § Registration Modes.
///
/// **Why an enum and not the two booleans it replaces.** The old
/// `registration.open` + `registration.invite_required` pair was not two
/// independent axes: `open` meant "the registration endpoint is enabled *at all*",
/// so invite-only was the *combination* `open && invite_required`, and the fourth
/// combination (`!open && invite_required`) was meaningless — a state the type
/// system now cannot express. Three variants, exactly the three the owner doc
/// ratifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationMode {
    /// Anyone may register (`fauna.account.register` succeeds without an invite).
    Open,
    /// Registration requires a valid invite code.
    InviteRequired,
    /// Registration is closed; no new accounts. An admin can still admit a user
    /// directly (`fauna.admin.users.create`) or approve an invite *request* —
    /// those are admin actions, not self-service registration.
    Closed,
}

impl RegistrationMode {
    /// The wire string for this mode. Explicit (not a derived discriminant) so the
    /// at-rest DB value and the wire value can never drift apart from the variant
    /// order.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            RegistrationMode::Open => "open",
            RegistrationMode::InviteRequired => "invite_required",
            RegistrationMode::Closed => "closed",
        }
    }

    /// Parse a wire/at-rest mode string. `None` on anything unrecognized —
    /// including a **newer peer's** mode this binary predates. Callers must
    /// degrade to the safe posture ([`RegistrationMode::Closed`]) rather than
    /// guess: an unknown mode must never fail *open* and start admitting
    /// strangers. Mirrors `NodeMode::from_wire_str` / `StorageMode::from_wire_str`.
    pub fn from_wire_str(s: &str) -> Option<RegistrationMode> {
        match s {
            "open" => Some(RegistrationMode::Open),
            "invite_required" => Some(RegistrationMode::InviteRequired),
            "closed" => Some(RegistrationMode::Closed),
            _ => None,
        }
    }

    /// Project the mode onto the two legacy wire booleans `(open, invite_required)`.
    ///
    /// The inverse of the collapse that produced the enum: `open` means "the
    /// register endpoint is enabled at all", so invite-only is `open` **+**
    /// `invite_required`. The fourth boolean combination (closed + invite) was
    /// incoherent and is unrepresentable here by construction.
    ///
    /// The one surface that still reports the posture as the boolean pair —
    /// the nest's deployment-internal `/internal/router-status` for the router
    /// (`nest.info`'s client-wire copy left with the compat-remnant sweep) —
    /// projects it through this one function. The
    /// booleans are a *projection of client-set nest state*, never a source: a
    /// consumer that stores its own copy is config theatre.
    pub fn to_wire_booleans(self) -> (bool, bool) {
        (
            self != RegistrationMode::Closed,
            self == RegistrationMode::InviteRequired,
        )
    }
}

/// The default posture of a nest whose admin has never chosen one: **closed**.
/// A fresh box admits its first user through the claim ceremony
/// (`fauna.auth.claim_admin`, which is self-contained and needs no registration),
/// and further users through an admin action — so "closed" is the correct
/// out-of-the-box state and the safe one: a box that boots before its admin has
/// picked a posture never admits a stranger.
pub const DEFAULT_REGISTRATION_MODE: RegistrationMode = RegistrationMode::Closed;

/// `fauna.admin.set_registration_mode` — set the deployment's registration posture
/// and (orthogonally) the free-tier ceiling.
///
/// One kind carries both because they are one admin decision surface (a single
/// Save on the `admin-users` registration section). They stay **separate values**:
/// per `public-mode.md` § Registration Modes the cap "applies a ceiling to
/// free-tier accounts *regardless of mode*", so it is not a fourth variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetRegistrationModeRequest {
    /// The new posture, as [`RegistrationMode::as_wire_str`].
    pub mode: String,
    /// The free-tier ceiling; `None` clears it (no limit). Orthogonal to `mode`.
    #[serde(default)]
    pub max_free_users: Option<u64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetRegistrationModeReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Days an evicted user spends in the eviction ladder's `warning` phase before
/// entering `suspended`. **This is NOT an admin policy** — it is a hard-coded
/// constant no human chooses (`docs/goal/behavior/admin.md` § 2 Users →
/// *Cutting a user off* names the warning window as a ladder timing, never as a
/// choice, and no client surfaces a control for it).
///
/// The `warning` phase is the user's **export window** — full access, for the
/// whole period, before `suspended` is ever entered — so this constant is the
/// load-bearing half of the *User always controls their data* invariant on the
/// eviction path. Echoed to clients on `fauna.account.get`
/// ([`crate::account::AccountNodePolicy`]) so they can render the
/// eviction-warning UI.
pub const EVICTION_WARNING_DAYS: i64 = 14;

/// Days a suspended user spends in the eviction ladder's `suspended` phase
/// before the account is **deleted**. The twin of [`EVICTION_WARNING_DAYS`] and
/// likewise a hard-coded constant, not an admin choice.
///
/// Only the *timed* ladder (`fauna.admin.users.evict`) advances to deletion; an
/// immediate `fauna.admin.users.suspend` leaves `eviction_delete_at` null and
/// never reaches this phase (`admin.md` § *Cutting a user off*).
pub const EVICTION_SUSPENSION_DAYS: i64 = 14;

/// `fauna.admin.set_serving_port` — set the deployment-wide client-facing API
/// serving port (the nest's own HTTPS listener: the WS-RPC transport + the served
/// SPA). The **port** counterpart of the bool/int/list knobs above — same
/// Admin-class authed shape, value the `port`. Governs only the router-less /
/// direct-listener bind (desktop-native / bare-IP / domainless); inert behind the
/// `:443` SNI router on a domain box, where the external port is artifact-wiring.
/// Boot-resolved into the nest's listener (the singleton overrides the
/// `--bind`/`listen` seed's port), read back on `fauna.setup.status`
/// (`SetupStatusReply.serving_port`); applies on the next nest (re)start (the nest
/// cannot hot-rebind its own listener). A port a human picks is client-set per
/// a product invariant: the client UI is the config surface, not the
/// `--bind` CLI seed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetServingPortRequest {
    pub port: u16,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetServingPortReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};

    /// The boolean projection is the inverse of the enum collapse, and the
    /// incoherent fourth state (closed + invite_required) is unreachable.
    #[test]
    fn to_wire_booleans_projects_every_mode() {
        assert_eq!(RegistrationMode::Open.to_wire_booleans(), (true, false));
        assert_eq!(
            RegistrationMode::InviteRequired.to_wire_booleans(),
            (true, true),
            "invite-only is open + invite_required, not a third boolean state"
        );
        assert_eq!(RegistrationMode::Closed.to_wire_booleans(), (false, false));

        for mode in [
            RegistrationMode::Open,
            RegistrationMode::InviteRequired,
            RegistrationMode::Closed,
        ] {
            let (open, invite_required) = mode.to_wire_booleans();
            assert!(
                open || !invite_required,
                "{mode:?} projected to the incoherent closed+invite state"
            );
        }
    }

    #[test]
    fn subhandles_round_trips() {
        let reply = SetSubhandlesReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap().to_vec();
        let decoded: SetSubhandlesReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn max_storage_bytes_round_trips_some_and_none() {
        for max_bytes in [Some(8_000_000_000u64), None] {
            let req = SetMaxStorageBytesRequest {
                max_bytes,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).unwrap().to_vec();
            let decoded: SetMaxStorageBytesRequest = decode_strict(&bytes).unwrap();
            assert_eq!(decoded, req);
        }
        let reply = SetMaxStorageBytesReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap().to_vec();
        let decoded: SetMaxStorageBytesReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn cors_origins_round_trips_list_and_empty() {
        for origins in [
            vec![
                "https://app.example.com".to_string(),
                "https://admin.example.com".to_string(),
            ],
            vec![],
        ] {
            let req = SetCorsOriginsRequest {
                origins: origins.clone(),
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).unwrap().to_vec();
            let decoded: SetCorsOriginsRequest = decode_strict(&bytes).unwrap();
            assert_eq!(decoded, req);
        }
        let reply = SetCorsOriginsReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap().to_vec();
        let decoded: SetCorsOriginsReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    #[test]
    fn serving_port_round_trips() {
        for port in [443u16, 8443, 3443, 1, u16::MAX] {
            let req = SetServingPortRequest {
                port,
                extra: Default::default(),
            };
            let bytes = encode_canonical(&req).unwrap().to_vec();
            let decoded: SetServingPortRequest = decode_strict(&bytes).unwrap();
            assert_eq!(decoded, req);
        }
        let reply = SetServingPortReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap().to_vec();
        let decoded: SetServingPortReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }
}
