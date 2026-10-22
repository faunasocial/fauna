//! The updater's report catalogue (`update-channel.md` § What the updater
//! reports → *The catalogue*): every event it can put in the nest container's
//! report file, each a compile-time template whose interpolations are the
//! bounded classes the plane's authoring rule allows — a digest's first twelve
//! hex digits, a version label or tag only inside the label charset, a channel
//! id, the closed kind and step enums below, counts and durations. Never an
//! engine or registry error string, a URL, a container id or a health body's
//! text: the updater's own stderr keeps those.

use std::fmt;
use std::time::Duration;

use fauna_provisioning::update_channel::UpdateChannel;
use fauna_provisioning::update_channel::registry::{Digest, RegistryError};
use fauna_provisioning::update_channel::report::reportable_label;

/// A step of a re-create or a hand-off. Attached as the [`anyhow`] context of
/// the call that failed (`.context(Step::Create)`), so the failure's kind is
/// read back by type ([`Step::of`]), never parsed from a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Pull,
    Tag,
    Rename,
    Stop,
    Create,
    Start,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Tag => "tag",
            Self::Rename => "rename",
            Self::Stop => "stop",
            Self::Create => "create",
            Self::Start => "start",
        }
    }

    /// The step an error chain failed at, when one was attached.
    pub fn of(err: &anyhow::Error) -> Option<Step> {
        err.downcast_ref::<Step>().copied()
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a registry check failed — a closed enum, never the error's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckFailure {
    Dns,
    Connect,
    Tls,
    Timeout,
    /// An HTTP status of the given class (4 or 5; 0 for any other).
    Status(u8),
    UnparseableManifest,
}

impl CheckFailure {
    pub fn of(err: &RegistryError) -> Self {
        match err {
            RegistryError::Http(e) if e.is_timeout() => Self::Timeout,
            RegistryError::Http(e) => {
                // Classified, never interpolated: the text stays in stderr.
                let mut chain = String::new();
                let mut source: Option<&dyn std::error::Error> = Some(e);
                while let Some(s) = source {
                    chain.push_str(&s.to_string().to_ascii_lowercase());
                    source = s.source();
                }
                if chain.contains("dns") {
                    Self::Dns
                } else if chain.contains("certificate") || chain.contains("tls") {
                    Self::Tls
                } else {
                    Self::Connect
                }
            }
            RegistryError::Status { status, .. } => match status.as_u16() / 100 {
                4 => Self::Status(4),
                5 => Self::Status(5),
                _ => Self::Status(0),
            },
            RegistryError::Malformed(_) => Self::UnparseableManifest,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Connect => "connect",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::Status(4) => "http 4xx",
            Self::Status(5) => "http 5xx",
            Self::Status(_) => "http status",
            Self::UnparseableManifest => "unparseable manifest",
        }
    }
}

/// Why a pull failed — a closed enum, never the engine's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullFailure {
    /// The engine reports the image or a blob unknown.
    NotFound,
    /// The pull stream reported a registry-side failure.
    Registry,
    /// Any other engine failure.
    Engine,
}

impl PullFailure {
    pub fn of(err: &anyhow::Error) -> Self {
        use bollard::errors::Error as E;
        match err.downcast_ref::<E>() {
            Some(E::DockerResponseServerError {
                status_code: 404, ..
            }) => Self::NotFound,
            Some(E::DockerStreamError { .. }) => Self::Registry,
            _ => Self::Engine,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not found",
            Self::Registry => "registry",
            Self::Engine => "engine",
        }
    }
}

/// What a start-time settle found and settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// A re-create's successor ran; the container left aside was removed.
    RecreateFinished,
    /// A re-create stopped short; the old container was put back.
    RecreatePutBack,
    /// A hand-off with no running successor; the updater took its name back.
    HandOffTakenBack,
}

impl Settled {
    fn as_str(self) -> &'static str {
        match self {
            Self::RecreateFinished => "a finished re-create, its old container removed",
            Self::RecreatePutBack => "an unfinished re-create, its old container put back",
            Self::HandOffTakenBack => "a hand-off without a running successor, taken back",
        }
    }
}

/// One report event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    RegistryFailed {
        kind: CheckFailure,
        channel: UpdateChannel,
        interval: Duration,
    },
    RegistryRecovered {
        failures: u32,
        outage: Duration,
    },
    NestUnreachable,
    NestRecovered {
        outage: Duration,
    },
    Hold {
        channel: UpdateChannel,
        running: String,
        head: String,
    },
    Pinned {
        tag: String,
    },
    ApplyStarted {
        channel: UpdateChannel,
        version: Option<String>,
        digest: Digest,
    },
    PullFailed {
        kind: PullFailure,
        digest: Digest,
    },
    RecreateFailed {
        step: Step,
    },
    ApplyCompleted {
        version: Option<String>,
        digest: Digest,
        took: Duration,
    },
    SelfUpdateCompleted,
    SelfUpdateFailed {
        step: Option<Step>,
    },
    ReconcileSettled {
        found: Settled,
    },
}

/// A digest's first twelve hex digits.
fn short(digest: &Digest) -> &str {
    let hex = digest.as_str().strip_prefix("sha256:").unwrap_or("");
    hex.get(..12).unwrap_or(hex)
}

fn version(label: &Option<String>) -> &str {
    label.as_deref().map_or("unlabelled", reportable_label)
}

fn secs(d: Duration) -> u64 {
    d.as_secs()
}

impl Event {
    /// The catalogue id — the `<event>` half of the ring's `updater:<event>`.
    pub fn id(&self) -> &'static str {
        match self {
            Self::RegistryFailed { .. } => "check.registry_failed",
            Self::RegistryRecovered { .. } => "check.registry_recovered",
            Self::NestUnreachable => "check.nest_unreachable",
            Self::NestRecovered { .. } => "check.nest_recovered",
            Self::Hold { .. } => "decision.hold",
            Self::Pinned { .. } => "decision.pinned",
            Self::ApplyStarted { .. } => "apply.started",
            Self::PullFailed { .. } => "apply.pull_failed",
            Self::RecreateFailed { .. } => "apply.recreate_failed",
            Self::ApplyCompleted { .. } => "apply.completed",
            Self::SelfUpdateCompleted => "self_update.completed",
            Self::SelfUpdateFailed { .. } => "self_update.failed",
            Self::ReconcileSettled { .. } => "reconcile.settled",
        }
    }

    /// `error` for a failed step, `warn` for a hold and a check that started
    /// failing, `info` for the rest.
    pub fn level(&self) -> &'static str {
        match self {
            Self::PullFailed { .. }
            | Self::RecreateFailed { .. }
            | Self::SelfUpdateFailed { .. } => "error",
            Self::RegistryFailed { .. } | Self::NestUnreachable | Self::Hold { .. } => "warn",
            _ => "info",
        }
    }

    /// The rendered template.
    pub fn message(&self) -> String {
        match self {
            Self::RegistryFailed {
                kind,
                channel,
                interval,
            } => format!(
                "the registry check for {} failed ({}); retrying every {} s",
                channel.id(),
                kind.as_str(),
                secs(*interval)
            ),
            Self::RegistryRecovered { failures, outage } => format!(
                "the registry check succeeds again after {failures} failed check(s) over {} s",
                secs(*outage)
            ),
            Self::NestUnreachable => {
                "the nest's health cannot be read; checks are skipped until it can".to_string()
            }
            Self::NestRecovered { outage } => format!(
                "the nest's health is readable again after {} s",
                secs(*outage)
            ),
            Self::Hold {
                channel,
                running,
                head,
            } => format!(
                "holding on {}: the box runs {}, newer than the channel's head {}; it moves once the channel reaches it",
                channel.id(),
                reportable_label(running),
                reportable_label(head)
            ),
            Self::Pinned { tag } => format!(
                "the box follows no channel: pinned to the image tag {}",
                reportable_label(tag)
            ),
            Self::ApplyStarted {
                channel,
                version: v,
                digest,
            } => format!(
                "applying {} {} ({})",
                channel.id(),
                version(v),
                short(digest)
            ),
            Self::PullFailed { kind, digest } => format!(
                "the pull of {} failed ({}); the box stays on its build",
                short(digest),
                kind.as_str()
            ),
            Self::RecreateFailed { step } => {
                format!("the re-create failed at {step}; the old container was put back")
            }
            Self::ApplyCompleted {
                version: v,
                digest,
                took,
            } => format!(
                "now runs {} ({}); the re-create took {} s",
                version(v),
                short(digest),
                secs(*took)
            ),
            Self::SelfUpdateCompleted => "the updater moved itself to the nest's build".to_string(),
            Self::SelfUpdateFailed { step } => format!(
                "the updater could not move itself to the nest's build (at {}); it keeps working on its own",
                step.map_or("an unknown step", Step::as_str)
            ),
            Self::ReconcileSettled { found } => {
                format!("settled at start: {}", found.as_str())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> Digest {
        Digest::of(b"x")
    }

    #[test]
    fn ids_are_in_the_planes_charset_and_levels_follow_the_rule() {
        let all = [
            Event::RegistryFailed {
                kind: CheckFailure::Dns,
                channel: UpdateChannel::Dev,
                interval: Duration::from_secs(60),
            },
            Event::RegistryRecovered {
                failures: 2,
                outage: Duration::from_secs(120),
            },
            Event::NestUnreachable,
            Event::NestRecovered {
                outage: Duration::from_secs(5),
            },
            Event::Hold {
                channel: UpdateChannel::Test,
                running: "0.2.0".into(),
                head: "0.1.9".into(),
            },
            Event::Pinned {
                tag: "sha-abc".into(),
            },
            Event::ApplyStarted {
                channel: UpdateChannel::Dev,
                version: Some("0.2.0".into()),
                digest: digest(),
            },
            Event::PullFailed {
                kind: PullFailure::Registry,
                digest: digest(),
            },
            Event::RecreateFailed { step: Step::Start },
            Event::ApplyCompleted {
                version: None,
                digest: digest(),
                took: Duration::from_secs(9),
            },
            Event::SelfUpdateCompleted,
            Event::SelfUpdateFailed { step: None },
            Event::ReconcileSettled {
                found: Settled::RecreatePutBack,
            },
        ];
        for e in &all {
            assert!(
                e.id()
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{}",
                e.id()
            );
            assert!(!e.message().is_empty());
            assert!(!e.message().chars().any(char::is_control));
        }
        let levels: Vec<&str> = all.iter().map(Event::level).collect();
        assert_eq!(
            levels,
            [
                "warn", "info", "warn", "info", "warn", "info", "info", "error", "error", "info",
                "info", "error", "info"
            ]
        );
    }

    #[test]
    fn a_registry_sourced_label_is_interpolated_only_in_the_charset() {
        let held = Event::Hold {
            channel: UpdateChannel::Test,
            running: "0.2.0".into(),
            head: "1.0; rm -rf /".into(),
        };
        assert!(held.message().contains("unparseable"));
        assert!(!held.message().contains("rm -rf"));
    }

    #[test]
    fn a_digest_is_its_first_twelve_hex_digits() {
        let d = digest();
        let started = Event::ApplyStarted {
            channel: UpdateChannel::Dev,
            version: None,
            digest: d.clone(),
        };
        let hex = &d.as_str()["sha256:".len()..];
        assert!(started.message().contains(&hex[..12]));
        assert!(!started.message().contains(&hex[..13]));
    }

    #[test]
    fn a_step_is_read_back_from_the_error_chain_by_type() {
        let err = anyhow::anyhow!("engine said no").context(Step::Create);
        assert_eq!(Step::of(&err), Some(Step::Create));
        assert_eq!(Step::of(&anyhow::anyhow!("bare")), None);
        assert_eq!(PullFailure::of(&err), PullFailure::Engine);
    }
}
