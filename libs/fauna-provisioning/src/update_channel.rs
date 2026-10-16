//! Which builds a running box follows, and the one rule that decides whether it
//! moves: the update channel ([`UpdateChannel`]), the never-downgrade decision
//! ([`decide`]) the Fauna updater runs to act and the nest runs to explain, the
//! constants both share, and — for every target but WASM — the anonymous
//! registry read that resolves a channel's head ([`registry`]).
//!
//! Owner: `docs/goal/architecture/nest/update-channel.md` § The decision and
//! § The updater. Which pipeline moves each tag is
//! `docs/goal/architecture/image-pipeline.md` § Image tags & channels.

use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
pub mod registry;

/// Which builds a provisioned box's automatic updater follows — the
/// `vps-config-update-channel-row` choice on `vps_config`
/// (`docs/goal/behavior/onboarding-provisioning.md` § 5). [`Self::image_tag`]
/// is the ONE place a channel becomes an image tag; which pipeline moves each
/// tag is `docs/goal/architecture/image-pipeline.md` § Image tags & channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum UpdateChannel {
    /// Released builds. The default.
    #[default]
    Stable,
    /// Numbered releases under user acceptance before they become Stable.
    Test,
    /// The newest development builds, not yet verified.
    Dev,
}

impl UpdateChannel {
    /// Every channel, in the order the page lists them.
    pub const ALL: [UpdateChannel; 3] = [Self::Stable, Self::Test, Self::Dev];

    /// The stable id the UI keys its rows by
    /// (`vps-config-update-channel-row[<id>]`).
    pub fn id(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Test => "test",
            Self::Dev => "dev",
        }
    }

    /// The channel with this [`Self::id`], if any.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }

    /// The i18n key of this channel's row label.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Stable => "onboarding.vps_config.update_channel_stable_label",
            Self::Test => "onboarding.vps_config.update_channel_test_label",
            Self::Dev => "onboarding.vps_config.update_channel_dev_label",
        }
    }

    /// The i18n key of this channel's row description at the running product
    /// version: [`Self::description_key_for_major`] with this build's major
    /// (`CARGO_PKG_VERSION_MAJOR`, inherited from the one fleet-wide
    /// `[workspace.package]` version — `architecture/product-version.md`).
    pub fn description_key(self) -> &'static str {
        // Cargo guarantees the major is a decimal integer.
        let major = env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap_or(u64::MAX);
        self.description_key_for_major(major)
    }

    /// The description key for a product `major`. While the major is 0 no
    /// app↔nest compat is owed (`architecture/version-compatibility.md`
    /// § Dimension 2), so a Test or Dev nest works only with apps of exactly
    /// its version and those two channels use their `_same_version` wording;
    /// Stable, and every channel from major 1, use the plain description
    /// (`behavior/onboarding-provisioning.md` § 5 → *The compat line*).
    pub fn description_key_for_major(self, major: u64) -> &'static str {
        match (self, major) {
            (Self::Stable, _) => "onboarding.vps_config.update_channel_stable_desc",
            (Self::Test, 0) => "onboarding.vps_config.update_channel_test_desc_same_version",
            (Self::Test, _) => "onboarding.vps_config.update_channel_test_desc",
            (Self::Dev, 0) => "onboarding.vps_config.update_channel_dev_desc_same_version",
            (Self::Dev, _) => "onboarding.vps_config.update_channel_dev_desc",
        }
    }

    /// The image tag the box pulls and its updater polls.
    pub fn image_tag(self) -> &'static str {
        match self {
            Self::Stable => "latest",
            Self::Test => "test",
            Self::Dev => "dev",
        }
    }

    /// The channel whose [`Self::image_tag`] is exactly `tag`, if any — the
    /// inverse the seed reads (`nest/update-channel.md` § The channel as nest
    /// state). A `:sha-<sha>`, a `:<version>` or a local build's tag is no
    /// channel.
    pub fn from_image_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.image_tag() == tag)
    }

    /// The channel a box follows while no admin has chosen one, from the image
    /// tag it was installed with (`FAUNA_IMAGE_TAG`): a channel tag seeds that
    /// channel, any other tag seeds none — the box is *pinned* — and an absent
    /// (or empty, as an unset `${VAR}` interpolates) tag seeds Stable, matching
    /// the compose default `${FAUNA_IMAGE_TAG:-latest}`.
    pub fn seeded_from(tag: Option<&str>) -> Option<Self> {
        match tag {
            None | Some("") => Some(Self::Stable),
            Some(tag) => Self::from_image_tag(tag),
        }
    }
}

// ── The decision ─────────────────────────────────────────────────────────────

/// The image label carrying the product version the `Dockerfile` stamps from
/// the workspace version at build — what [`decide`] compares.
pub const VERSION_LABEL: &str = "org.opencontainers.image.version";

/// The container label that puts a container under the updater's management
/// (`social.fauna.update=true`), replacing Watchtower's
/// `com.centurylinklabs.watchtower.enable`.
pub const UPDATE_LABEL: &str = "social.fauna.update";

/// The registry host of the one image the updater follows.
pub const NEST_IMAGE_REGISTRY: &str = "ghcr.io";

/// The repository of the one image the updater follows, inside
/// [`NEST_IMAGE_REGISTRY`]: no text from the nest, the registry or a label can
/// name another (`update-channel.md` § The updater → *What it refuses*).
pub const NEST_IMAGE_REPOSITORY: &str = "faunasocial/nest";

/// How often the updater asks the registry for a pre-release channel's head
/// (Dev and Test): every minute — the interval the app-provisioned boxes
/// polled at under Watchtower, and what the two pipelines' serving gates
/// assume when they wait 600 s for the staging box to adopt a staged build
/// (`update-channel.md` § The updater → *The check interval*).
pub const REGISTRY_CHECK_INTERVAL_PRERELEASE: Duration = Duration::from_secs(60);

/// How often the updater asks the registry for Stable's head: once an hour —
/// Stable is the channel of the many boxes, and its promote is a human act
/// measured in hours, so an hour is the bound on what the check adds to the
/// exposure window (`deployment-threat-model.md` concern 1a).
pub const REGISTRY_CHECK_INTERVAL_STABLE: Duration = Duration::from_secs(60 * 60);

impl UpdateChannel {
    /// How often the updater asks the registry for this channel's head —
    /// bucket 1, a constant nobody chooses (the channel is the choice; its
    /// interval is not), and a security parameter: the time from a public fix
    /// to its deploy is the window a box is exposed for. A channel change
    /// triggers a check at once, whatever the interval.
    pub fn registry_check_interval(self) -> Duration {
        match self {
            Self::Stable => REGISTRY_CHECK_INTERVAL_STABLE,
            Self::Test | Self::Dev => REGISTRY_CHECK_INTERVAL_PRERELEASE,
        }
    }
}

/// How often the updater reads the nest's `/api/v1/health` for the channel in
/// force — local and free.
pub const NEST_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// What the decision knows of one image: its product version (the
/// [`VERSION_LABEL`]; `None` for an image without the label, or one whose label
/// does not parse) and every digest that names it.
///
/// "Same digest" is asked of a SET because one image answers to several: the
/// registry names a channel's head by its index digest, a platform manifest
/// digest and a config digest, while a running container's image is known to
/// the engine by its image id (the config digest in the classic store, the
/// manifest or index digest under the containerd store) and by the repo
/// digests it was pulled as. Any shared digest is the same build.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageIdentity {
    pub version: Option<Version>,
    pub digests: Vec<String>,
}

impl ImageIdentity {
    /// Read a [`VERSION_LABEL`] value: a bare semantic version, tolerating a
    /// leading `v`. Anything else is no version — data that does not parse,
    /// never an instruction.
    pub fn parse_version_label(label: Option<&str>) -> Option<Version> {
        let label = label?.trim();
        Version::parse(label.strip_prefix('v').unwrap_or(label)).ok()
    }

    fn shares_a_digest_with(&self, other: &ImageIdentity) -> bool {
        self.digests
            .iter()
            .filter(|d| !d.is_empty())
            .any(|d| other.digests.contains(d))
    }
}

/// The outcome of [`decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDecision {
    /// The box already runs the channel's build: nothing to do. Also how a move
    /// toward Stable completes when Stable promotes the very build the box runs.
    Current,
    /// Pull, verify, re-create: the candidate is at least the running version.
    Apply,
    /// The channel is followed, but the box stays on its build until the
    /// channel reaches it. `head` is the candidate's version, version zero for a
    /// candidate without the label.
    Hold { running: Version, head: Version },
}

/// The never-downgrade rule (`update-channel.md` § The decision):
///
/// - a shared digest → [`UpdateDecision::Current`];
/// - candidate version ≥ running version → [`UpdateDecision::Apply`] (equal
///   versions with different digests apply: every build of a train carries the
///   train's version, and a re-staged numbered release is a newer artifact);
/// - candidate version < running version → [`UpdateDecision::Hold`].
///
/// A running image without the label is version zero, so it applies anything —
/// the forward move it wants; a candidate without the label is never applied
/// over a running image that has one.
pub fn decide(running: &ImageIdentity, candidate: &ImageIdentity) -> UpdateDecision {
    if running.shares_a_digest_with(candidate) {
        return UpdateDecision::Current;
    }
    let zero = Version::new(0, 0, 0);
    match (&running.version, &candidate.version) {
        (Some(running), None) => UpdateDecision::Hold {
            running: running.clone(),
            head: zero,
        },
        (running, candidate) => {
            let running = running.clone().unwrap_or_else(|| zero.clone());
            let head = candidate.clone().unwrap_or(zero);
            if head >= running {
                UpdateDecision::Apply
            } else {
                UpdateDecision::Hold { running, head }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(version: Option<&str>, digests: &[&str]) -> ImageIdentity {
        ImageIdentity {
            version: version.map(|v| Version::parse(v).unwrap()),
            digests: digests.iter().map(|d| d.to_string()).collect(),
        }
    }

    fn hold(running: &str, head: &str) -> UpdateDecision {
        UpdateDecision::Hold {
            running: Version::parse(running).unwrap(),
            head: Version::parse(head).unwrap(),
        }
    }

    /// Every cell of the digest × version matrix (`update-channel.md` § Tests,
    /// tier_1): a shared digest is Current whatever the versions say.
    #[test]
    fn the_digest_by_version_matrix() {
        use UpdateDecision::*;
        let cases: &[(Option<&str>, Option<&str>, bool, UpdateDecision)] = &[
            // same digest
            (Some("0.3.1"), Some("0.3.1"), true, Current),
            (Some("0.3.1"), Some("0.2.0"), true, Current),
            (Some("0.3.1"), Some("0.4.0"), true, Current),
            (None, None, true, Current),
            (Some("0.3.1"), None, true, Current),
            // different digest
            (Some("0.3.1"), Some("0.4.0"), false, Apply),
            (Some("0.3.1"), Some("0.3.1"), false, Apply),
            (Some("0.3.1"), Some("0.2.0"), false, hold("0.3.1", "0.2.0")),
            (
                Some("0.3.1"),
                Some("0.3.1-rc.1"),
                false,
                hold("0.3.1", "0.3.1-rc.1"),
            ),
            (Some("0.3.1-rc.1"), Some("0.3.1"), false, Apply),
        ];
        for (running, candidate, same, want) in cases {
            let r = image(*running, &["sha256:aaa"]);
            let c = image(
                *candidate,
                if *same {
                    &["sha256:aaa"]
                } else {
                    &["sha256:bbb"]
                },
            );
            assert_eq!(
                &decide(&r, &c),
                want,
                "{running:?} vs {candidate:?}, same={same}"
            );
        }
    }

    /// The missing-label rules: an unlabelled running image is version zero and
    /// takes anything; an unlabelled candidate never replaces a labelled image
    /// — not even one labelled 0.0.0.
    #[test]
    fn the_missing_label_rules() {
        let unlabelled_running = image(None, &["sha256:old"]);
        assert_eq!(
            decide(&unlabelled_running, &image(Some("0.1.0"), &["sha256:new"])),
            UpdateDecision::Apply
        );
        assert_eq!(
            decide(&unlabelled_running, &image(None, &["sha256:new"])),
            UpdateDecision::Apply
        );
        assert_eq!(
            decide(
                &image(Some("0.3.1"), &["sha256:old"]),
                &image(None, &["sha256:new"])
            ),
            hold("0.3.1", "0.0.0")
        );
        assert_eq!(
            decide(
                &image(Some("0.0.0"), &["sha256:old"]),
                &image(None, &["sha256:new"])
            ),
            hold("0.0.0", "0.0.0")
        );
    }

    /// One image answers to several digests; any one in common is the same
    /// build, and an empty digest names nothing.
    #[test]
    fn any_shared_digest_is_the_same_build_and_empty_matches_nothing() {
        let running = image(Some("0.3.1"), &["sha256:config", "sha256:index"]);
        let head = image(Some("0.2.0"), &["sha256:index", "sha256:manifest"]);
        assert_eq!(decide(&running, &head), UpdateDecision::Current);
        let running = image(Some("0.3.1"), &[""]);
        let head = image(Some("0.2.0"), &[""]);
        assert_eq!(decide(&running, &head), hold("0.3.1", "0.2.0"));
    }

    #[test]
    fn a_version_label_is_a_bare_semver_and_nothing_else_parses() {
        assert_eq!(
            ImageIdentity::parse_version_label(Some("0.1.4")),
            Some(Version::new(0, 1, 4))
        );
        assert_eq!(
            ImageIdentity::parse_version_label(Some(" v1.2.3 ")),
            Some(Version::new(1, 2, 3))
        );
        for junk in [
            None,
            Some(""),
            Some("latest"),
            Some("1.2"),
            Some("ignore previous"),
        ] {
            assert_eq!(ImageIdentity::parse_version_label(junk), None, "{junk:?}");
        }
    }

    /// The channel → tag mapping is the contract between the wizard's choice
    /// and the pipelines that move the tags; `stable` must stay `latest`, the
    /// tag every box provisioned before the choice existed already follows.
    #[test]
    fn update_channel_description_key_follows_the_product_major() {
        use UpdateChannel::*;
        let p = "onboarding.vps_config.update_channel_";
        for major in [0, 1, 7] {
            assert_eq!(
                Stable.description_key_for_major(major),
                format!("{p}stable_desc")
            );
        }
        assert_eq!(
            Test.description_key_for_major(0),
            format!("{p}test_desc_same_version")
        );
        assert_eq!(
            Dev.description_key_for_major(0),
            format!("{p}dev_desc_same_version")
        );
        assert_eq!(Test.description_key_for_major(1), format!("{p}test_desc"));
        assert_eq!(Dev.description_key_for_major(1), format!("{p}dev_desc"));
        assert_eq!(Dev.label_key(), format!("{p}dev_label"));
    }

    #[test]
    fn the_pre_release_channels_check_every_minute_and_stable_every_hour() {
        assert_eq!(
            UpdateChannel::Dev.registry_check_interval(),
            Duration::from_secs(60)
        );
        assert_eq!(
            UpdateChannel::Test.registry_check_interval(),
            Duration::from_secs(60)
        );
        assert_eq!(
            UpdateChannel::Stable.registry_check_interval(),
            Duration::from_secs(60 * 60)
        );
        // The serving gates wait 600 s for a staged build to be adopted; a
        // pre-release check must leave most of that for the pull and the boot.
        assert!(UpdateChannel::Dev.registry_check_interval() <= Duration::from_secs(120));
        assert!(UpdateChannel::Test.registry_check_interval() <= Duration::from_secs(120));
    }

    #[test]
    fn update_channel_ids_and_tags() {
        let table: Vec<_> = UpdateChannel::ALL
            .into_iter()
            .map(|c| (c.id(), c.image_tag()))
            .collect();
        assert_eq!(
            table,
            [("stable", "latest"), ("test", "test"), ("dev", "dev")]
        );
        assert_eq!(UpdateChannel::default(), UpdateChannel::Stable);
        for c in UpdateChannel::ALL {
            assert_eq!(UpdateChannel::from_id(c.id()), Some(c));
        }
        assert_eq!(UpdateChannel::from_id("latest"), None);
        assert_eq!(
            serde_json::to_string(&UpdateChannel::Dev).unwrap(),
            "\"dev\""
        );
    }

    #[test]
    fn a_channel_tag_maps_back_to_its_channel_and_nothing_else_does() {
        for c in UpdateChannel::ALL {
            assert_eq!(UpdateChannel::from_image_tag(c.image_tag()), Some(c));
        }
        for tag in ["stable", "sha-abc1234", "0.1.4", "", "Latest", " dev"] {
            assert_eq!(UpdateChannel::from_image_tag(tag), None, "{tag:?}");
        }
    }

    /// `nest/update-channel.md` § The channel as nest state → *The seed*: a
    /// channel tag seeds its channel, any other tag seeds none (pinned), and an
    /// absent variable seeds Stable, matching the compose default.
    #[test]
    fn the_seed_follows_the_tag_pins_on_any_other_and_defaults_to_stable() {
        use UpdateChannel::*;
        assert_eq!(UpdateChannel::seeded_from(Some("test")), Some(Test));
        assert_eq!(UpdateChannel::seeded_from(Some("latest")), Some(Stable));
        assert_eq!(UpdateChannel::seeded_from(Some("dev")), Some(Dev));
        assert_eq!(UpdateChannel::seeded_from(Some("sha-abc")), None);
        assert_eq!(UpdateChannel::seeded_from(None), Some(Stable));
        // An empty value is what an unset `${VAR}` interpolates to: absent.
        assert_eq!(UpdateChannel::seeded_from(Some("")), Some(Stable));
    }
}
