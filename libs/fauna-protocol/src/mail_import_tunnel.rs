//! The web mail-import tunnel's wire — the blind WebSocket-to-TCP byte tunnel
//! the web app's source transport rides, because a browser has no raw TCP
//! (`docs/goal/behavior/mailbox-migration.md` § Where the IMAP client runs →
//! *The web tunnel*, the owner of every claim here).
//!
//! **Shape.** `GET` [`ROUTE`] upgrades to a WebSocket under [`SUBPROTOCOL`],
//! the actor's bearer offered as the second protocol token (`bearer.<token>`),
//! as the WS-RPC upgrade offers it. The client's first frame is a binary
//! DAG-CBOR [`TunnelOpen`]; the nest's first frame is a binary DAG-CBOR
//! [`TunnelAnswer`] — `opened`, or `refused` with a [`TunnelRefusal`] and a
//! close 1008 — so a browser, which sees no HTTP status of a failed upgrade,
//! still gets a typed answer. Every later binary frame is opaque bytes in its
//! direction; TLS ends inside the client, so the nest carries ciphertext only.
//! Each close names a [`TunnelClose`] in its reason text.
//!
//! The wire types live here, below both halves: the nest serves them
//! (`fauna_nest::mail_import_tunnel`) and the wasm source transport speaks
//! them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

/// The upgrade route, on the nest's existing listener.
pub const ROUTE: &str = "/api/v1/mail/import/tunnel";

/// The tunnel's WebSocket subprotocol — the first protocol token of the
/// upgrade; the second is `bearer.<token>`.
pub const SUBPROTOCOL: &str = "fauna.mail-tunnel.v1";

/// Implicit-TLS IMAP: TLS from the first byte.
pub const PORT_IMAPS: u16 = 993;

/// STARTTLS IMAP: one `<tag> STARTTLS` line, then TLS.
pub const PORT_IMAP_STARTTLS: u16 = 143;

/// HTTPS — admitted to [`MICROSOFT_AUTHORITY_HOST`] alone, for the Outlook
/// source's device-code sign-in (`mailbox-migration.md` § Microsoft sign-in —
/// the device-code grant), whose endpoints send no CORS headers.
pub const PORT_HTTPS: u16 = 443;

/// The Microsoft identity authority's host — the one host the tunnel dials on
/// [`PORT_HTTPS`]. The same host `fauna_mail::imap_client::oauth_device::MICROSOFT`
/// signs in at; a unit test there pins the agreement.
pub const MICROSOFT_AUTHORITY_HOST: &str = "login.microsoftonline.com";

/// The largest WebSocket frame either direction may carry (a TLS record is at
/// most 16 KiB plus overhead). A larger frame closes 1009.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Seconds with no byte in either direction before the nest closes the tunnel.
pub const IDLE_SECS: u64 = 300;

/// The client's first frame: the one destination this tunnel will carry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TunnelOpen {
    /// The destination host — a DNS name or an IP literal, as the user typed
    /// it or a provider preset names it.
    pub host: String,
    /// [`PORT_IMAPS`], [`PORT_IMAP_STARTTLS`] or [`PORT_HTTPS`].
    pub port: u16,
    /// `true` exactly on [`PORT_IMAP_STARTTLS`]: the client sends one
    /// `STARTTLS` command line before its TLS ClientHello.
    pub starttls: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The nest's first frame. Internally tagged on `outcome`, the
/// discriminated-reply convention (`LinkPreviewResolveReply`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TunnelAnswer {
    /// The destination is dialled; bytes flow from the next frame on.
    Opened {},
    /// Refused before any byte was dialled; a close 1008 follows.
    Refused { reason: TunnelRefusal },
}

/// Why an open was refused — the closed set the owner doc names.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TunnelRefusal {
    /// The host is not one the tunnel carries: a malformed open, a non-global
    /// address, or a host other than the Microsoft authority on 443.
    Destination,
    /// A port other than 993, 143 or 443, or a TLS mode that does not match it.
    Port,
    /// The host did not resolve.
    Resolve,
    /// The actor's or the nest's concurrent-tunnel bound is full.
    Busy,
    /// The actor's open-rate meter is spent.
    RateLimited,
    /// The destination resolved but the TCP dial failed.
    DialFailed,
}

impl TunnelRefusal {
    /// The reason's wire spelling — also the close frame's text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Destination => "destination",
            Self::Port => "port",
            Self::Resolve => "resolve",
            Self::Busy => "busy",
            Self::RateLimited => "rate_limited",
            Self::DialFailed => "dial_failed",
        }
    }
}

/// Why an opened tunnel closed — the close frame's code and reason text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelClose {
    /// The destination closed its side (1000).
    Eof,
    /// No byte in either direction for [`IDLE_SECS`] (1000).
    Idle,
    /// The client's first bytes were not TLS-shaped (1008): no ClientHello
    /// record, or on 143 anything but one `STARTTLS` line before it.
    NotTls,
    /// A text frame, or a first frame that was not a [`TunnelOpen`] (1008).
    BadFrame,
    /// A frame over [`MAX_FRAME_BYTES`] (1009).
    FrameTooLarge,
    /// A nest-side fault, such as a write error toward the destination (1011).
    Fault,
    /// The bearer's authority was revoked — lockout, suspension, the
    /// session's or device's revocation (4401, the WS-RPC socket's own
    /// revocation close).
    Revoked,
}

impl TunnelClose {
    /// The WebSocket close code.
    pub fn code(self) -> u16 {
        match self {
            Self::Eof | Self::Idle => 1000,
            Self::NotTls | Self::BadFrame => 1008,
            Self::FrameTooLarge => 1009,
            Self::Fault => 1011,
            Self::Revoked => 4401,
        }
    }

    /// The close frame's reason text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eof => "eof",
            Self::Idle => "idle",
            Self::NotTls => "not_tls",
            Self::BadFrame => "bad_frame",
            Self::FrameTooLarge => "frame_too_large",
            Self::Fault => "fault",
            Self::Revoked => "revoked",
        }
    }
}

/// The close code a refusal carries.
pub const REFUSAL_CLOSE_CODE: u16 = 1008;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    #[test]
    fn the_open_and_both_answers_round_trip() {
        let open = TunnelOpen {
            host: "imap.example.org".into(),
            port: PORT_IMAP_STARTTLS,
            starttls: true,
            ..Default::default()
        };
        let bytes = encode_canonical(&open).unwrap();
        assert_eq!(decode_strict::<TunnelOpen>(&bytes).unwrap(), open);

        for answer in [
            TunnelAnswer::Opened {},
            TunnelAnswer::Refused {
                reason: TunnelRefusal::RateLimited,
            },
        ] {
            let bytes = encode_canonical(&answer).unwrap();
            assert_eq!(decode_strict::<TunnelAnswer>(&bytes).unwrap(), answer);
        }
    }

    #[test]
    fn a_refusal_spells_the_same_text_on_the_wire_and_in_the_close() {
        for r in [
            TunnelRefusal::Destination,
            TunnelRefusal::Port,
            TunnelRefusal::Resolve,
            TunnelRefusal::Busy,
            TunnelRefusal::RateLimited,
            TunnelRefusal::DialFailed,
        ] {
            let bytes = encode_canonical(&TunnelAnswer::Refused { reason: r }).unwrap();
            let value: crate::Value = decode_strict(&bytes).unwrap();
            let text = format!("{value:?}");
            assert!(text.contains(r.as_str()), "{text} names {}", r.as_str());
        }
    }
}
