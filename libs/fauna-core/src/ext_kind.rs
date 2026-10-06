//! Third-party kind strings — `ext.<publisher>.<name>` — and the `records`
//! qualifier that names one kind or every kind of one publisher.
//!
//! Owner: `docs/goal/architecture/third-party-kinds.md` § The grammar. The
//! rulings this module is the only door for:
//!
//! - **`<name>` is exactly one label and `<publisher>` is everything between
//!   `ext.` and the last `.`.** Two distinct `(publisher, name)` pairs can
//!   therefore never spell one kind — `example.co` + `uk.notes` is refused
//!   (a dotted name), so only `example.co.uk` + `notes` spells
//!   `ext.example.co.uk.notes`.
//! - **`<publisher>` is the metadata document's host**, lowercase, as DNS
//!   labels of `a–z 0–9 -` (no leading/trailing `-`, each 1–63 bytes, the
//!   whole at most 253; punycode for an IDN, never a trailing dot, a port or
//!   an IP literal's brackets). Host equality, never a registrable-domain
//!   rule.
//! - **`<name>` is one label of `a–z 0–9 _ -`**, starting with a letter or a
//!   digit, 1–63 bytes, never a `.`.
//! - **Parse by the last dot, refuse rather than repair.** Every consumer —
//!   the manifest parser, the `ext` scope family, the `records` arm's reach
//!   check, the registry overlay — takes an [`ExtKind`], never a `&str` it
//!   splits itself.
//! - **The wildcard matches one publisher, structurally.** [`ExtQualifier`]'s
//!   `ext.<publisher>.*` covers exactly the kinds whose parsed publisher
//!   *equals* `<publisher>` — never a `starts_with` on the string.
//!
//! The string freezes at first seal: it is the `keyed_hash` input of the
//! kind's delegable key pair (`fauna_core::crypto::DelegableSchedule::for_kind`).

use std::fmt;
use std::str::FromStr;

/// Every third-party kind string starts with this tag; `fauna.*` is reserved
/// for first-party kinds.
pub const EXT_KIND_PREFIX: &str = "ext.";

/// The `records` qualifier's wildcard suffix (`ext.<publisher>.*`).
pub const EXT_WILDCARD_SUFFIX: &str = ".*";

/// The longest publisher (a DNS name) the grammar admits.
pub const MAX_PUBLISHER_LEN: usize = 253;

/// The longest single label — a publisher label or the kind's name.
pub const MAX_LABEL_LEN: usize = 63;

/// Why a kind string (or a qualifier) was refused. Every refusal is
/// deliberate where a repair would be possible (§ The grammar: refused, not
/// repaired).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtKindError {
    /// The string does not start with `ext.` (a `fauna.` kind included).
    #[error("not an ext.* kind: {0:?}")]
    NotExt(String),
    /// No `.` separates a publisher from a name, or one half is empty.
    #[error("an ext.* kind is ext.<publisher>.<name>: {0:?}")]
    BadShape(String),
    /// The publisher is not a lowercase DNS host (§ The grammar).
    #[error("malformed publisher {0:?}")]
    BadPublisher(String),
    /// The name is not one label of `a–z 0–9 _ -`.
    #[error("malformed kind name {0:?}")]
    BadName(String),
}

/// One DNS label of a publisher: `a–z 0–9 -`, 1–63 bytes, no leading or
/// trailing `-`.
fn valid_host_label(label: &str) -> bool {
    let b = label.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_LABEL_LEN
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
        && b.iter()
            .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// Is `host` a publisher the grammar admits — the metadata document's host,
/// lowercase, as DNS labels (§ The grammar)? The manifest parser uses this
/// same predicate on `publisher.domain`, so a host that is not a valid
/// publisher can never publish a kind.
pub fn is_valid_publisher(host: &str) -> bool {
    !host.is_empty() && host.len() <= MAX_PUBLISHER_LEN && host.split('.').all(valid_host_label)
}

/// The kind's name: one label of `a–z 0–9 _ -`, starting with a letter or a
/// digit, 1–63 bytes. Public because the manifest's `bridge.id` follows this
/// same label rule (`third-party.md` § The manifest → *The `bridge` block*),
/// so the two can never drift apart.
pub fn is_valid_kind_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_LABEL_LEN
        && matches!(b[0], b'a'..=b'z' | b'0'..=b'9')
        && b.iter()
            .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// A parsed third-party kind. Constructing or parsing one is the only door,
/// so a value is canonical by construction and its `Display` is the frozen
/// kind string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtKind {
    publisher: String,
    name: String,
}

impl ExtKind {
    /// Build the kind `ext.<publisher>.<name>`, validating both halves.
    pub fn new(publisher: &str, name: &str) -> Result<Self, ExtKindError> {
        if !is_valid_publisher(publisher) {
            return Err(ExtKindError::BadPublisher(publisher.to_string()));
        }
        if !is_valid_kind_name(name) {
            return Err(ExtKindError::BadName(name.to_string()));
        }
        Ok(Self {
            publisher: publisher.to_string(),
            name: name.to_string(),
        })
    }

    /// The publisher — the host of the metadata document that declared it.
    pub fn publisher(&self) -> &str {
        &self.publisher
    }

    /// The kind's one-label name within its publisher.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for ExtKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{EXT_KIND_PREFIX}{}.{}", self.publisher, self.name)
    }
}

impl FromStr for ExtKind {
    type Err = ExtKindError;

    fn from_str(s: &str) -> Result<Self, ExtKindError> {
        let Some(rest) = s.strip_prefix(EXT_KIND_PREFIX) else {
            return Err(ExtKindError::NotExt(s.to_string()));
        };
        let Some((publisher, name)) = rest.rsplit_once('.') else {
            return Err(ExtKindError::BadShape(s.to_string()));
        };
        if publisher.is_empty() || name.is_empty() {
            return Err(ExtKindError::BadShape(s.to_string()));
        }
        Self::new(publisher, name)
    }
}

/// Does `s` parse as an `ext.*` kind? The cheap predicate the registry
/// lookups use to route a kind string to the overlay.
pub fn is_ext_kind(s: &str) -> bool {
    s.parse::<ExtKind>().is_ok()
}

/// A `records` qualifier: one kind, or every kind of one publisher
/// (`ext.<publisher>.*`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExtQualifier {
    /// Exactly one kind.
    Kind(ExtKind),
    /// Every kind whose parsed publisher equals this host.
    Publisher(String),
}

impl ExtQualifier {
    /// Does the qualifier cover `kind`? Structural — a wildcard compares the
    /// parsed publisher for equality, never a string prefix, so
    /// `ext.example.co.*` does not cover `ext.example.co.uk.notes`.
    pub fn covers(&self, kind: &ExtKind) -> bool {
        match self {
            Self::Kind(k) => k == kind,
            Self::Publisher(p) => p == kind.publisher(),
        }
    }

    /// The publisher every covered kind belongs to.
    pub fn publisher(&self) -> &str {
        match self {
            Self::Kind(k) => k.publisher(),
            Self::Publisher(p) => p,
        }
    }

    /// Is this the wildcard form?
    pub fn is_wildcard(&self) -> bool {
        matches!(self, Self::Publisher(_))
    }
}

impl fmt::Display for ExtQualifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kind(k) => k.fmt(f),
            Self::Publisher(p) => write!(f, "{EXT_KIND_PREFIX}{p}{EXT_WILDCARD_SUFFIX}"),
        }
    }
}

impl FromStr for ExtQualifier {
    type Err = ExtKindError;

    fn from_str(s: &str) -> Result<Self, ExtKindError> {
        if let Some(head) = s.strip_suffix(EXT_WILDCARD_SUFFIX) {
            let Some(publisher) = head.strip_prefix(EXT_KIND_PREFIX) else {
                return Err(ExtKindError::NotExt(s.to_string()));
            };
            if !is_valid_publisher(publisher) {
                return Err(ExtKindError::BadPublisher(publisher.to_string()));
            }
            return Ok(Self::Publisher(publisher.to_string()));
        }
        s.parse().map(Self::Kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(s: &str) -> ExtKind {
        s.parse()
            .unwrap_or_else(|e| panic!("{s:?} must parse: {e}"))
    }

    #[test]
    fn a_kind_round_trips_at_exactly_its_string() {
        let k = ExtKind::new("example.com", "reading-list").unwrap();
        assert_eq!(k.to_string(), "ext.example.com.reading-list");
        assert_eq!(kind("ext.example.com.reading-list"), k);
        assert_eq!(k.publisher(), "example.com");
        assert_eq!(k.name(), "reading-list");
    }

    /// The collision the 2026-08-11 spelling allowed: `example.co` +
    /// `uk.notes` and `example.co.uk` + `notes` minted one string. Now only
    /// the second pair constructs, and the string parses to it alone.
    #[test]
    fn the_two_collision_pairs_cannot_spell_one_kind() {
        assert!(matches!(
            ExtKind::new("example.co", "uk.notes"),
            Err(ExtKindError::BadName(_))
        ));
        let k = kind("ext.example.co.uk.notes");
        assert_eq!(k.publisher(), "example.co.uk");
        assert_eq!(k.name(), "notes");
        // A dotted "hierarchy" is read as a longer publisher, never repaired:
        // `ext.example.com.reading.list` names publisher example.com.reading.
        assert_eq!(
            kind("ext.example.com.reading.list").publisher(),
            "example.com.reading"
        );
    }

    #[test]
    fn the_wildcard_matches_one_publisher_structurally() {
        let q: ExtQualifier = "ext.example.co.*".parse().unwrap();
        assert!(q.covers(&kind("ext.example.co.notes")));
        assert!(!q.covers(&kind("ext.example.co.uk.notes")));
        assert!(!q.covers(&kind("ext.api.example.co.notes")));
        assert_eq!(q.to_string(), "ext.example.co.*");
        let one: ExtQualifier = "ext.example.com.notes".parse().unwrap();
        assert!(one.covers(&kind("ext.example.com.notes")));
        assert!(!one.covers(&kind("ext.example.com.other")));
        assert_eq!(one.publisher(), "example.com");
    }

    #[test]
    fn parse_refuses_rather_than_repairs() {
        for bad in [
            "fauna.state.profile",        // first-party
            "ext.",                       // empty
            "ext.example",                // no name
            "ext..notes",                 // empty publisher
            "ext.example.com.",           // empty name
            "ext.Example.com.notes",      // uppercase publisher
            "ext.example.com.Notes",      // uppercase name
            "ext.example.com.-notes",     // name leads with '-'
            "ext.-example.com.notes",     // label leads with '-'
            "ext.example-.com.notes",     // label trails with '-'
            "ext.example..com.notes",     // empty label
            "ext.example.com:8080.notes", // a port
            "ext.[::1].notes",            // an IP literal's brackets
            "ext.exa mple.com.notes",     // a space
            "Ext.example.com.notes",      // the tag is lowercase
        ] {
            assert!(bad.parse::<ExtKind>().is_err(), "must refuse {bad:?}");
        }
        let long_label = "a".repeat(64);
        assert!(
            format!("ext.{long_label}.com.notes")
                .parse::<ExtKind>()
                .is_err()
        );
        assert!(
            format!("ext.example.com.{long_label}")
                .parse::<ExtKind>()
                .is_err()
        );
        let long_host = vec!["abcdefghi"; 26].join("."); // 259 bytes
        assert!(format!("ext.{long_host}.notes").parse::<ExtKind>().is_err());
    }

    #[test]
    fn names_take_underscores_and_digits_and_hosts_take_punycode() {
        assert!("ext.example.com.room_member".parse::<ExtKind>().is_ok());
        assert!("ext.example.com.2fa".parse::<ExtKind>().is_ok());
        assert!("ext.xn--bcher-kva.example.notes".parse::<ExtKind>().is_ok());
        // The loopback developer carve-out publishes under its host literally.
        assert!("ext.127.0.0.1.notes".parse::<ExtKind>().is_ok());
        assert!("ext.localhost.notes".parse::<ExtKind>().is_ok());
        assert!("ext.ex_ample.com.notes".parse::<ExtKind>().is_err());
    }

    #[test]
    fn a_qualifier_refuses_a_malformed_wildcard() {
        for bad in [
            "ext.*",
            "ext..*",
            "ext.Example.com.*",
            "example.com.*",
            "ext.example.com.**",
        ] {
            assert!(bad.parse::<ExtQualifier>().is_err(), "must refuse {bad:?}");
        }
    }
}
