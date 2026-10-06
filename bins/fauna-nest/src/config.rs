//! TOML configuration file parsing for the node.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct NestConfig {
    pub nest: NestSection,
    // `[storage]` (distributed/tiered storage) and `[mls]` (an MLS-delivery
    // policy) were REMOVED 2026-10-01 with the baseline removal of dead
    // shapes: no code ever read either, and a future storage or delivery
    // policy is an in-app choice, not a file section. A `nest.toml` still
    // carrying them parses unchanged (no `deny_unknown_fields`).
    // `[pairing]` (the pull target) and `[forwarding]` (the post-forwarding
    // switch) were REMOVED 2026-10-02 with the move of the private-side
    // workers onto the pairing row: which nest a private box
    // syncs from, and whether a user's posts are forwarded there, are that
    // user's `fauna.pair.add` (`private-mode.md` § Implementation status
    // today). A `nest.toml` still carrying either table parses unchanged.
    pub bridges: Option<BridgesSection>,
    pub submission: Option<SubmissionSection>,
    pub acme: Option<AcmeSection>,
    pub email: Option<EmailSection>,
    // `[bluesky] enabled` was REMOVED 2026-09-02: no code ever
    // read it, and the bridge's availability is now derived from the claimed
    // identity domain, not declared in a file. `NestConfig` sets no
    // `deny_unknown_fields`, so a `nest.toml` still carrying the section parses
    // unchanged.
    //
    // `[algorithm]` (the algorithm-service mode: builtin / sidecar / remote)
    // was REMOVED 2026-10-01 with the algorithm-service mesh it configured;
    // ranking is the labeler registry's, chosen in the apps. A `nest.toml`
    // still carrying the section parses unchanged, for the same reason.
    #[serde(default)]
    pub update: UpdateSection,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct UpdateSection {
    #[serde(default = "default_update_check")]
    pub check: bool,
    #[serde(default)]
    pub github_token: Option<String>,
}

fn default_update_check() -> bool {
    true
}

/// The nest's NAT mode (`public` / `private`). One enum, one home:
/// the canonical definition lives in `fauna_core::nat_mode` (same variants,
/// same `snake_case` serde repr the config TOML parses, same
/// `as_str`/`from_wire_str`), shared with the onboarding machine's
/// `NatModeSnapshot` and the admin client — re-exported here for the nest's
/// existing `config::NodeMode` call sites.
pub use fauna_core::nat_mode::NodeMode;

#[derive(Debug, Clone, Deserialize)]
pub struct NestSection {
    pub mode: NodeMode,
    pub listen: String,
    pub db_path: String,
    #[serde(default)]
    pub tls_cert: Option<String>,
    #[serde(default)]
    pub tls_key: Option<String>,
    #[serde(default)]
    pub secret_key_path: Option<String>,
    #[serde(default)]
    pub max_storage_bytes: Option<u64>,
    // NOTE: no `require_registration` field. It had no production reader (its only
    // caller was a `#[test]`), and the value it carried in `config/default.toml`
    // (`false`) was the *opposite* of the real default — a trap. Registration is now
    // unconditionally required; the posture (open / invite-required / closed) is the
    // client-set `registration_mode` below. An old TOML still carrying the key parses
    // fine — serde ignores unknown fields — it just does nothing.
    // NOTE: no `required_obligations` field. It was never wired (nothing
    // resolved it into live state), and when a client-scanning framework lands, obligations
    // will be client-set — never a config-file seed (the only configuration
    // surface is the apps). An old TOML still carrying the key parses fine —
    // serde ignores unknown fields.
    // NOTE: no `onnx_model_path` / `onnx_vocab_path` fields. They located the
    // classifier model the nest served to the apps' on-device scorer, retired
    // with it (`content-scoring.md` § The placement matrix). An old TOML still
    // carrying either key parses fine — serde ignores unknown fields.
    #[serde(default)]
    pub domain: Option<String>,
    /// Whether this nest offers subhandle addressing (`handle@domain`,
    /// `@handle.domain`). A rarely-changing deployment policy; the nest serves
    /// the flag (node-info) and the alternative address strings from its DB —
    /// no DNS write is involved (the nest never writes DNS; only the client,
    /// which holds the DNS-provider keys, does). Client-set nest config; the
    /// admin-client toggle UI is a follow-up.
    #[serde(default)]
    pub subhandles: bool,
    /// **Pre-claim seed only** for the registration posture (`open` /
    /// `invite_required` / `closed`). The authoritative value is the client-set
    /// `nest_registration_mode` DB singleton (`fauna.admin.set_registration_mode`,
    /// Admin-class); this seed is used only until a client sets it. Absent or
    /// unparseable ⇒ [`fauna_protocol::node_policy::DEFAULT_REGISTRATION_MODE`]
    /// (`closed`) — a registration posture never defaults open.
    ///
    /// This is artifact-set wiring, NOT a human-edited knob: an admin chooses the
    /// posture in their client, never in this file (product invariant: the only
    /// configuration surface is the apps).
    #[serde(default)]
    pub registration_mode: Option<String>,
    /// Pre-claim seed only for the free-tier ceiling; orthogonal to the mode.
    #[serde(default)]
    pub max_free_users: Option<u64>,
    #[serde(default)]
    pub blob_dir: Option<String>,
    #[serde(default)]
    pub static_dir: Option<String>,
    #[serde(default)]
    pub cors_origins: Vec<String>,
}

/// Struct-update base for the ~10 test/`desktop_serve` fixtures that build a
/// `NestSection` by hand: `NestSection { db_path, ..Default::default() }`.
///
/// Hand-listing every field means each new field breaks every fixture — and
/// because `cargo check --lib` never compiles `bins/fauna-nest/tests/`, those
/// breaks land **without going red**: adding `registration_mode` +
/// `max_free_users` silently broke 7 integration-test binaries, found only when a
/// later slice built them explicitly. A `Default` makes the next additive field a
/// non-event, and lets two branches grow the struct without colliding.
///
/// `db_path` deliberately defaults to the **empty string**, not a real path: a
/// fixture that forgets to set it must fail loudly, never quietly open
/// `/data/nest.db`. `mode` / `listen` mirror `config/default.toml`.
impl Default for NestSection {
    fn default() -> Self {
        Self {
            mode: NodeMode::Public,
            listen: "0.0.0.0:3000".to_string(),
            db_path: String::new(),
            tls_cert: None,
            tls_key: None,
            secret_key_path: None,
            max_storage_bytes: None,
            domain: None,
            subhandles: false,
            // No seed ⇒ the safe `closed` default. A nest that boots before its
            // admin has picked a posture must not admit strangers.
            registration_mode: None,
            max_free_users: None,
            blob_dir: None,
            static_dir: None,
            cors_origins: vec![],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BridgesSection {
    #[serde(default)]
    pub imap: bool,
    #[serde(default)]
    pub caldav: bool,
    #[serde(default)]
    pub smtp_outbound: bool,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionPolicy {
    Open,
    PairedOnly,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmissionSection {
    #[serde(default = "default_submission_policy")]
    pub policy: SubmissionPolicy,
}

fn default_submission_policy() -> SubmissionPolicy {
    SubmissionPolicy::Open
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum AcmeMode {
    /// The running nest provisions TLS over HTTP-01 only (`acme_http01.rs`) — it
    /// serves the challenge on its own port and never writes DNS. DNS-01 was
    /// removed with the nest-side DNS-write retirement (the nest holds no
    /// DNS-provider key; only the client publishes DNS).
    Http01,
}

/// `[acme]` config table. Note there is **no `enabled` field**: whether a nest
/// runs ACME is *derived*, never configured — see `acme::build_acme_config`
/// (public NAT axis + a real orderable domain). The fields here are all
/// deployment topology (mode / dir / CA URL), pure IPC set by the deployment
/// artifact, never a user-facing preference. The CA (Let's Encrypt production)
/// and the account contact (none) are constants, so a retired `email` /
/// `staging` key in an old file parses (no `deny_unknown_fields`) and does
/// nothing.
#[derive(Debug, Clone, Deserialize)]
pub struct AcmeSection {
    #[serde(default = "default_acme_mode")]
    pub mode: AcmeMode,
    #[serde(default)]
    pub dir: Option<String>,
    /// Explicit ACME directory URL override: the nest's HTTP-01 client orders
    /// against this CA (the in-network `pebble` of the tier_4 acceptance, or a
    /// private ACME CA) instead of Let's Encrypt production. Unset ⇒ Let's
    /// Encrypt production. The Docker entrypoint maps `FAUNA_ACME_DIRECTORY_URL`
    /// here.
    #[serde(default)]
    pub directory_url: Option<String>,
}

fn default_acme_mode() -> AcmeMode {
    AcmeMode::Http01
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmailSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub smtp_bind: Option<String>,
    #[serde(default)]
    pub max_size: Option<u64>,
    #[serde(default)]
    pub require_tls: bool,
}

impl NestConfig {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config file {path}: {e}"))?;
        let config: NestConfig = toml::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("failed to parse config file {path}: {e}"))?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_mode_wire_round_trip() {
        for m in [NodeMode::Public, NodeMode::Private] {
            assert_eq!(NodeMode::from_wire_str(m.as_str()), Some(m));
        }
        assert_eq!(NodeMode::from_wire_str("public"), Some(NodeMode::Public));
        assert_eq!(NodeMode::from_wire_str("private"), Some(NodeMode::Private));
        // Strict: no trimming, no case folding, no other values.
        assert_eq!(NodeMode::from_wire_str("Public"), None);
        assert_eq!(NodeMode::from_wire_str(" private "), None);
        assert_eq!(NodeMode::from_wire_str("relay"), None);
        assert_eq!(NodeMode::from_wire_str(""), None);
    }

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:443"
db_path = "/var/lib/fauna/nest.db"
"#;
        let config: NestConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.nest.mode, NodeMode::Public);
        assert_eq!(config.nest.listen, "0.0.0.0:443");
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[nest]
mode = "private"
listen = "0.0.0.0:8443"
db_path = "nest.db"
secret_key_path = "node.key"
max_storage_bytes = 2000000000000

# The retired `[storage]` and `[mls]` sections: a file still carrying them
# parses unchanged, and nothing reads them.
[storage]
replication_factor = 2

[mls]
delivery = true
"#;
        let config: NestConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.nest.mode, NodeMode::Private);
    }

    #[test]
    fn parse_subhandles_node_flag() {
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:443"
db_path = "nest.db"
subhandles = true
"#;
        let config: NestConfig = toml::from_str(toml).unwrap();
        assert!(config.nest.subhandles);
    }

    #[test]
    fn parse_private_nest_config_with_bridges() {
        let toml = r#"
[nest]
mode = "private"
listen = "0.0.0.0:4433"
db_path = "nest.db"

[bridges]
imap = true
caldav = true
smtp_outbound = true
"#;
        let config: NestConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.nest.mode, NodeMode::Private);
        let bridges = config.bridges.unwrap();
        assert!(bridges.imap);
        assert!(bridges.caldav);
        assert!(bridges.smtp_outbound);
    }

    #[test]
    fn parse_public_nest_config_with_submission_policy() {
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:443"
db_path = "nest.db"

[submission]
policy = "open"
"#;
        let config: NestConfig = toml::from_str(toml).unwrap();
        let sub = config.submission.unwrap();
        assert_eq!(sub.policy, SubmissionPolicy::Open);
    }

    #[test]
    fn parse_node_section_with_domain_and_extras() {
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:443"
db_path = "/var/lib/fauna/nest.db"
domain = "alice.nest.fauna.social"
blob_dir = "/data/blobs"
static_dir = "/data/static"
cors_origins = ["https://fauna.social"]
"#;
        let config: NestConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.nest.domain.as_deref(),
            Some("alice.nest.fauna.social")
        );
        assert_eq!(config.nest.blob_dir.as_deref(), Some("/data/blobs"));
        assert_eq!(config.nest.static_dir.as_deref(), Some("/data/static"));
        assert_eq!(config.nest.cors_origins, vec!["https://fauna.social"]);
    }

    #[test]
    fn parse_acme_section() {
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:443"
db_path = "nest.db"

[acme]
mode = "http01"
dir = "/data/acme"
"#;
        let config: NestConfig = toml::from_str(toml).unwrap();
        let acme = config.acme.unwrap();
        assert_eq!(acme.mode, AcmeMode::Http01);
        assert_eq!(acme.dir.as_deref(), Some("/data/acme"));
    }

    #[test]
    fn parse_email_section() {
        let toml = r#"
[nest]
mode = "public"
listen = "0.0.0.0:443"
db_path = "nest.db"

[email]
enabled = true
domain = "alice.nest.fauna.social"
smtp_bind = "0.0.0.0:25"
max_size = 26214400
"#;
        let config: NestConfig = toml::from_str(toml).unwrap();
        let email = config.email.unwrap();
        assert!(email.enabled);
        assert_eq!(email.domain, "alice.nest.fauna.social");
        assert_eq!(email.smtp_bind.as_deref(), Some("0.0.0.0:25"));
        assert_eq!(email.max_size, Some(26214400));
    }

    #[test]
    fn parse_default_config() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let path = repo_root.join("config/default.toml");
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let config: NestConfig = toml::from_str(&contents)
            .unwrap_or_else(|e| panic!("failed to parse default.toml: {e}"));
        assert_eq!(config.nest.mode, NodeMode::Public);
        assert_eq!(config.nest.db_path, "/data/nest.db");
        assert_eq!(config.nest.blob_dir.as_deref(), Some("/data/blobs"));
        // No optional sections should be set, EXCEPT `[acme]` which
        // carries the docker-default acme cert directory (matches the
        // /data/ convention for db_path and blob_dir). Platform
        // installers rewrite `/data/acme` to `/var/lib/fauna/acme`
        // (Linux) / `~/Library/Application Support/Fauna/acme`
        // (macOS) the same way they rewrite `/data/nest.db`.
        assert!(config.bridges.is_none());
        assert!(config.email.is_none());
        assert_eq!(
            config.acme.as_ref().and_then(|a| a.dir.as_deref()),
            Some("/data/acme")
        );
    }

    #[test]
    fn parse_test_web_config() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let path = repo_root.join("config/test-web.toml");
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let config: NestConfig = toml::from_str(&contents)
            .unwrap_or_else(|e| panic!("failed to parse test-web.toml: {e}"));
        assert_eq!(config.nest.mode, NodeMode::Public);
        assert!(config.nest.static_dir.is_some());
    }

    #[test]
    fn parse_test_private_paired_config() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let path = repo_root.join("config/test-private-paired.toml");
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let config: NestConfig = toml::from_str(&contents)
            .unwrap_or_else(|e| panic!("failed to parse test-private-paired.toml: {e}"));
        assert_eq!(config.nest.mode, NodeMode::Private);
    }
}
