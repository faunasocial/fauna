//! Which channels have a build at all — what the setup wizard learns from the
//! registry on entering `vps_config`, and the two choices it decides: the
//! default channel ([`UpdateChannel::default_for`]) and each row's description
//! ([`UpdateChannel::description_key_for`]). The orchestrator's Server step asks
//! the same question of the chosen tag once more ([`resolve`]) before anything
//! is bought.
//!
//! Owner: `docs/goal/behavior/onboarding-provisioning.md` § 5 → *What exists
//! decides the default, and nothing is bought for a channel with no build*.

use semver::Version;
use serde::{Deserialize, Serialize};

use super::UpdateChannel;
use super::registry::Registry;

/// The platform the wizard's probe and the Server step's pre-flight resolve:
/// every curated plan is x86 today (`providers_generated.rs` `curated_offers`);
/// a plan architecture field replaces this with the first arm plan.
pub const WIZARD_PLAN_ARCH: &str = "amd64";

/// The running product version — the one fleet-wide `[workspace.package]`
/// version every crate inherits (`architecture/product-version.md`), which is
/// what an app compares a channel's head against.
pub fn product_version() -> Version {
    // Cargo guarantees the package version is a semantic version.
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// What the registry says of one channel's tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "build")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ChannelBuild {
    /// The tag resolves to a build. `version` is its version label as a
    /// semantic version, `None` for a build without one.
    Available { version: Option<String> },
    /// The registry answered `404`: nothing has been published on the channel.
    NoBuild,
    /// Any other outcome — offline, a forwarder that is not live, a registry
    /// error. Treated as the status quo, never as a missing build.
    Unknown,
}

/// One channel's [`ChannelBuild`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ChannelAvailability {
    pub channel: UpdateChannel,
    pub build: ChannelBuild,
}

/// Resolve `tag` for `linux/<arch>` and classify the outcome.
pub async fn resolve(registry: &Registry, tag: &str, arch: &str) -> ChannelBuild {
    match registry.head(tag, arch).await {
        Ok(head) => ChannelBuild::Available {
            version: head.identity.version.map(|v| v.to_string()),
        },
        Err(e) if registry.is_missing_tag(&e, tag) => ChannelBuild::NoBuild,
        Err(_) => ChannelBuild::Unknown,
    }
}

/// [`resolve`] every channel, in page order.
pub async fn resolve_all(registry: &Registry, arch: &str) -> Vec<ChannelAvailability> {
    let mut out = Vec::with_capacity(UpdateChannel::ALL.len());
    for channel in UpdateChannel::ALL {
        out.push(ChannelAvailability {
            channel,
            build: resolve(registry, channel.image_tag(), arch).await,
        });
    }
    out
}

impl UpdateChannel {
    /// The channel the page selects while the user has chosen none: the first
    /// in page order whose build is available and usable from an app at
    /// `app_version`, or whose state is unknown — so an unprobed page (an empty
    /// slice) keeps Stable. Usable: Stable; or a product major of 1 or more; or
    /// the build's version equals the app's (`onboarding-provisioning.md` § 5 →
    /// *The compat line*: while the major is 0 a Test or Dev nest works only
    /// with apps of exactly its version). A build without a version label
    /// counts as usable. Nothing qualifying → Stable.
    pub fn default_for(availability: &[ChannelAvailability], app_version: &Version) -> Self {
        let usable = |channel: UpdateChannel, version: &Option<String>| {
            channel == Self::Stable
                || app_version.major >= 1
                || match version {
                    None => true,
                    Some(v) => Version::parse(v).is_ok_and(|v| v == *app_version),
                }
        };
        Self::ALL
            .into_iter()
            .find(|&channel| {
                match availability.iter().find(|a| a.channel == channel) {
                    // A channel the probe did not report is unknown.
                    None => true,
                    Some(a) => match &a.build {
                        ChannelBuild::Available { version } => usable(channel, version),
                        ChannelBuild::Unknown => true,
                        ChannelBuild::NoBuild => false,
                    },
                }
            })
            .unwrap_or(Self::Stable)
    }

    /// This row's description key given what the registry said of it: the
    /// `_desc_no_build` wording for a channel with no build, else the compat
    /// line's choice at `major` ([`Self::description_key_for_major`]).
    pub fn description_key_for(self, major: u64, build: Option<&ChannelBuild>) -> &'static str {
        match build {
            Some(ChannelBuild::NoBuild) => match self {
                Self::Stable => "onboarding.vps_config.update_channel_stable_desc_no_build",
                Self::Test => "onboarding.vps_config.update_channel_test_desc_no_build",
                Self::Dev => "onboarding.vps_config.update_channel_dev_desc_no_build",
            },
            _ => self.description_key_for_major(major),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::NEST_IMAGE_REPOSITORY;
    use UpdateChannel::*;

    fn avail(version: &str) -> ChannelBuild {
        ChannelBuild::Available {
            version: Some(version.to_string()),
        }
    }

    fn table(builds: [ChannelBuild; 3]) -> Vec<ChannelAvailability> {
        UpdateChannel::ALL
            .into_iter()
            .zip(builds)
            .map(|(channel, build)| ChannelAvailability { channel, build })
            .collect()
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn an_unprobed_page_defaults_to_stable() {
        assert_eq!(UpdateChannel::default_for(&[], &v("0.1.5")), Stable);
    }

    /// The pre-promote registry (`:latest` absent): the default skips Stable
    /// and lands on the pre-release channel whose head this app can talk to.
    #[test]
    fn without_a_stable_build_the_default_is_the_channel_matching_this_app() {
        let t = table([ChannelBuild::NoBuild, avail("0.1.4"), avail("0.1.5")]);
        assert_eq!(UpdateChannel::default_for(&t, &v("0.1.5")), Dev);
        assert_eq!(UpdateChannel::default_for(&t, &v("0.1.4")), Test);
        // Neither head matches a 0.1.6 app: nothing usable, Stable.
        assert_eq!(UpdateChannel::default_for(&t, &v("0.1.6")), Stable);
    }

    #[test]
    fn from_major_one_any_available_build_is_usable() {
        let t = table([ChannelBuild::NoBuild, avail("1.0.0"), avail("1.1.0")]);
        assert_eq!(UpdateChannel::default_for(&t, &v("1.2.0")), Test);
    }

    #[test]
    fn unknown_is_the_status_quo_and_a_present_stable_build_wins() {
        let unknown = table([
            ChannelBuild::Unknown,
            ChannelBuild::Unknown,
            ChannelBuild::Unknown,
        ]);
        assert_eq!(UpdateChannel::default_for(&unknown, &v("0.1.5")), Stable);
        let all = table([avail("0.1.3"), avail("0.1.4"), avail("0.1.5")]);
        assert_eq!(UpdateChannel::default_for(&all, &v("0.1.5")), Stable);
        // An unknown Test after a missing Stable is taken before a usable Dev.
        let t = table([ChannelBuild::NoBuild, ChannelBuild::Unknown, avail("0.1.5")]);
        assert_eq!(UpdateChannel::default_for(&t, &v("0.1.5")), Test);
        // A build without a version label is usable.
        let t = table([
            ChannelBuild::NoBuild,
            ChannelBuild::Available { version: None },
            avail("0.1.5"),
        ]);
        assert_eq!(UpdateChannel::default_for(&t, &v("0.1.9")), Test);
    }

    #[test]
    fn a_channel_with_no_build_says_so_and_the_rest_keep_the_compat_line() {
        let p = "onboarding.vps_config.update_channel_";
        for c in UpdateChannel::ALL {
            assert_eq!(
                c.description_key_for(0, Some(&ChannelBuild::NoBuild)),
                format!("{p}{}_desc_no_build", c.id())
            );
            for build in [None, Some(&ChannelBuild::Unknown), Some(&avail("0.1.5"))] {
                assert_eq!(
                    c.description_key_for(0, build),
                    c.description_key_for_major(0)
                );
            }
        }
        assert_eq!(
            Test.description_key_for(1, Some(&avail("1.0.0"))),
            format!("{p}test_desc")
        );
    }

    /// `404` is no build; anything but a resolved head or a `404` is unknown.
    #[tokio::test]
    async fn a_404_is_no_build_and_any_other_failure_is_unknown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/v2/faunasocial/nest/manifests/latest"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/v2/faunasocial/nest/manifests/test"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        // A tag that resolves to a digest the registry then cannot serve: a
        // `404` past the tag is a broken image, not a missing one.
        Mock::given(method("HEAD"))
            .and(path("/v2/faunasocial/nest/manifests/dev"))
            .respond_with(ResponseTemplate::new(200).insert_header(
                "docker-content-digest",
                format!("sha256:{}", "a".repeat(64)).as_str(),
            ))
            .mount(&server)
            .await;
        let r = Registry::new(reqwest::Client::new(), &server.uri(), NEST_IMAGE_REPOSITORY);
        assert_eq!(resolve(&r, "latest", "amd64").await, ChannelBuild::NoBuild);
        assert_eq!(resolve(&r, "test", "amd64").await, ChannelBuild::Unknown);
        assert_eq!(resolve(&r, "dev", "amd64").await, ChannelBuild::Unknown);
        let all = resolve_all(&r, "amd64").await;
        assert_eq!(
            all.iter().map(|a| a.channel).collect::<Vec<_>>(),
            UpdateChannel::ALL
        );
        assert_eq!(all[0].build, ChannelBuild::NoBuild);
        // A refused connection is unknown too.
        let gone = Registry::new(
            reqwest::Client::new(),
            "http://127.0.0.1:9",
            NEST_IMAGE_REPOSITORY,
        );
        assert_eq!(
            resolve(&gone, "latest", "amd64").await,
            ChannelBuild::Unknown
        );
    }
}
