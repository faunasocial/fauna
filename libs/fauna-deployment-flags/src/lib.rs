//! The flag-file names the nest materializes in its data dir, and the one place
//! they are written.
//!
//! These are a **cross-process on-disk contract**: the nest creates or removes a
//! file, and a supervisor — the s6 run-scripts in the container, the Windows SCM
//! service, the macOS LaunchDaemon — reads its presence to decide which role
//! processes to run and which ports to bind. Presence is the whole signal for an
//! enable flag; the two `*_PORT_FLAG` files carry decimal text.
//!
//! # Why this crate exists at all
//!
//! Until 2026-08-22 each side held its own string literal, under a doc comment
//! naming the other as authoritative and asserting *"it must match the nest
//! constant exactly"* — a promise no build checked. The failure that shape
//! invites is silent and product-visible rather than cosmetic: a divergent
//! filename means the supervisor never brings the role process up, so the user's
//! mail (or CalDAV, or WebDAV) is simply off, with nothing logged at the layer
//! that decided it.
//!
//! The reason the copies were kept — *"depending on the whole `fauna-nest` crate
//! from a desktop service shell would be backwards"* — is correct, and this crate
//! preserves it rather than overruling it. What it rejects is the inference that
//! no owner was therefore possible. The constraint the reason really imposes is
//! on **weight**, not on ownership: both reading shells are std-only (one has
//! zero dependencies, the other four), so the owner had to be something they
//! could take on for free. That rules out the conceptually tidy neighbour —
//! `fauna_protocol::node_policy`, which `mail_enable.rs` already cites for
//! `DEFAULT_SERVING_PORT` — and leaves a zero-dependency leaf crate.
//! **Enumerate the consumers' dependency graphs before letting the tidiest crate
//! win.**
//!
//! # Scope — what this does NOT close
//!
//! Two references to these names live outside Rust and are unaffected:
//! `installer/macos/scripts/common.sh` lists several of them in a shell array,
//! and `bins/fauna-bridges/cmd/fauna-supervisor/supervisor.go` names
//! `/data/imap-enabled` in a comment. The contract is single-sourced **for
//! Rust**, not fleet-wide.
//!
//! Names that are genuinely one-sided stay with their owner and are deliberately
//! absent here: the nest's `atproto-enabled` (no supervisor-shell reader — the
//! Go supervisor sidekick handles that role), and the MDA supervisor's
//! `operator-hatch.toml`.

#![forbid(unsafe_code)]

/// Presence = mail (SMTP/IMAP) enabled.
///
/// Writer: `fauna_nest::mail_enable`. Readers: `fauna_mda_supervisor`, and the
/// MDA's s6 run-script, which gates on `imap-enabled OR caldav-enabled OR
/// carddav-enabled OR webdav-enabled` — the MDA process hosts all four
/// protocols, so any one flag keeps it up and each protocol then binds its own
/// listener per its own config. Per `mail-bridge-lifecycle.md`
/// § Default-off on first claim.
pub const MAIL_ENABLE_FLAG: &str = "imap-enabled";

/// Presence = CalDAV enabled — the calendar twin of [`MAIL_ENABLE_FLAG`].
/// Per `caldav-server.md` § Independent enablement.
pub const CALDAV_ENABLE_FLAG: &str = "caldav-enabled";

/// Presence = CardDAV enabled — the contacts twin of [`CALDAV_ENABLE_FLAG`].
/// CardDAV rides the SAME DAV listener as CalDAV (no separate port).
pub const CARDDAV_ENABLE_FLAG: &str = "carddav-enabled";

/// Presence = WebDAV enabled — the files twin of [`CARDDAV_ENABLE_FLAG`], and
/// likewise on the shared DAV listener. Per `webdav-server.md`
/// § Independent enablement.
pub const WEBDAV_ENABLE_FLAG: &str = "webdav-enabled";

/// The admin-set DAV port, decimal text. The MDA supervisor's twin of
/// [`SERVING_PORT_FLAG`] — a supervisor detects a change off the flag the nest
/// writes, without re-opening the DB.
pub const CALDAV_PORT_FLAG: &str = "caldav-port";

/// The admin-set client-facing serving port the nest has materialized, decimal
/// text. Absent ⇒ the supervisor keeps its install-time default. The nest cannot
/// hot-rebind its own `TcpListener`, so the supervisor restarts it on a change.
/// Per `nest/common.md` § Serving ports.
pub const SERVING_PORT_FLAG: &str = "serving-port";

/// Parse a `*_PORT_FLAG` file's already-read contents into a bindable port, or
/// `None` when the text is unparseable or `0` — a `0` is not a bindable
/// listener port (both `set_caldav_port`/`set_serving_port` reject it on write),
/// so it reads as a corrupt flag, not a real value. The read itself stays with
/// the caller (this crate is deliberately zero-dependency, so it never touches
/// `std::fs` or logs) — a caller wanting to warn on the corrupt case still can,
/// since it alone knows whether `raw` came from a present-but-bad file or one
/// it decided not to read.
#[must_use]
pub fn parse_port_flag(raw: &str) -> Option<u16> {
    match raw.trim().parse::<u16>() {
        Ok(port) if port != 0 => Some(port),
        _ => None,
    }
}

/// Parse a `FAUNA_DATA_DIR`-shaped env override: `None`, empty, or
/// whitespace-only input all count as "not set" and yield `None`; anything
/// else is trimmed and returned as a path. Shared by `fauna-nest`,
/// `fauna-nest-daemon` and `fauna-bridge-supervisor`'s own `FAUNA_DATA_DIR`
/// readers — this crate stays zero-dependency, so the env read itself
/// (`std::env::var`) is still each caller's own job.
#[must_use]
pub fn parse_dir_override(raw: Option<&str>) -> Option<std::path::PathBuf> {
    let dir = raw?.trim();
    (!dir.is_empty()).then(|| std::path::PathBuf::from(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unset / empty / whitespace-only ⇒ `None`; a real path is trimmed and
    /// returned. The one rule all three `FAUNA_DATA_DIR` readers must agree
    /// on, since a divergence here would silently relocate one daemon's data
    /// while leaving its siblings on the fallback path.
    #[test]
    fn parse_dir_override_treats_blank_as_unset() {
        assert_eq!(parse_dir_override(None), None);
        assert_eq!(parse_dir_override(Some("")), None);
        assert_eq!(parse_dir_override(Some("   ")), None);
        assert_eq!(
            parse_dir_override(Some("/Library/Application Support/Fauna")),
            Some(std::path::PathBuf::from(
                "/Library/Application Support/Fauna"
            )),
        );
        assert_eq!(
            parse_dir_override(Some("/srv/fauna\n")),
            Some(std::path::PathBuf::from("/srv/fauna")),
            "trailing whitespace trimmed",
        );
    }

    /// `0` is not a bindable port, so it reads as corrupt like any other
    /// unparseable text — this is the one rule both `*_PORT_FLAG` readers must
    /// agree on, or a corrupt flag on one side silently binds port 0 while the
    /// other falls back to its default.
    #[test]
    fn parse_port_flag_rejects_zero_and_garbage() {
        assert_eq!(parse_port_flag("8443"), Some(8443));
        assert_eq!(parse_port_flag("  8443\n"), Some(8443), "trims whitespace");
        assert_eq!(parse_port_flag("0"), None);
        assert_eq!(parse_port_flag(""), None);
        assert_eq!(parse_port_flag("not-a-port"), None);
        assert_eq!(parse_port_flag("-1"), None, "u16 has no sign");
        assert_eq!(parse_port_flag("99999"), None, "past u16::MAX");
    }

    /// A flag name is a **filename**, joined onto a data dir by every consumer.
    /// A leading separator would make the join absolute and silently redirect
    /// the read out of the data dir; a trailing one would make it a directory.
    /// Neither is a mistake a reviewer would catch in a string literal, and both
    /// fail the same silent way the duplication did.
    #[test]
    fn every_flag_is_a_bare_relative_filename() {
        for name in ALL {
            assert!(!name.is_empty(), "empty flag name");
            assert!(
                !name.contains('/') && !name.contains('\\'),
                "{name}: a flag name is a bare filename, never a path"
            );
            assert!(
                !name.starts_with('.'),
                "{name}: a dotfile hides the flag from an admin listing the data dir"
            );
        }
    }

    /// Two flags sharing a name would make one role's enablement silently
    /// control another's — the presence test cannot tell them apart.
    #[test]
    fn flag_names_are_distinct() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a, b, "two flags share the name {a}");
            }
        }
    }

    const ALL: [&str; 6] = [
        MAIL_ENABLE_FLAG,
        CALDAV_ENABLE_FLAG,
        CARDDAV_ENABLE_FLAG,
        WEBDAV_ENABLE_FLAG,
        CALDAV_PORT_FLAG,
        SERVING_PORT_FLAG,
    ];
}
