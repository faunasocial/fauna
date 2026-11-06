//! The `fauna://` in-app route's intake (`apps/tui.md` § System integration →
//! *In-app routes*).
//!
//! tui registers no OS URL-scheme handler (absence 8): a route reaches it as
//! the first `fauna://` argument on its command line — the argv leg the windows
//! app reads too (`App.xaml.cs`'s `RouteFromArgs`). The grammar is the one
//! shared `fauna_core::app_route` — this file never parses a URI itself. The
//! parsed route is held on `App` and applied through its one door,
//! [`crate::app::App::apply_route`], once the session is signed in.
//!
//! The same command line carries tui's nest hint — a bare `host[:port]`
//! argument ([`first_nest_hint_arg`]; `apps/tui.md` § System integration →
//! *The nest hint*), held on `App` until the first launch decision.

use fauna_core::app_route::AppRoute;

const SCHEME: &str = "fauna://";

/// The route on the command line `args` (program name first, as
/// `std::env::args()` yields it): the first argument that is a `fauna://` URI,
/// scheme matched case-insensitively as the parser matches it. An unparseable
/// one is logged and dropped — never an error surface, since a terminal is not
/// what a stranger's link launches.
pub(crate) fn first_route_arg<I, S>(args: I) -> Option<AppRoute>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let arg = args.into_iter().skip(1).find(|a| {
        a.as_ref()
            .get(..SCHEME.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(SCHEME))
    })?;
    let route = AppRoute::parse(arg.as_ref());
    if route.is_none() {
        tracing::warn!(target: "fauna_tui::routes", "dropping an unparseable fauna:// argument");
    }
    route
}

/// The nest hint on the command line `args` (`apps/tui.md` § System
/// integration → *The nest hint*): the first argument after the program name
/// that is neither a `fauna://` URI (that one is the route,
/// [`first_route_arg`]) nor option-shaped (a leading `-`), returned raw. No
/// trim, no classification — the drop rule is
/// `OnboardingMachine::set_nest_hint`'s, so junk comes back as typed.
pub(crate) fn first_nest_hint_arg<I, S>(args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .skip(1)
        .find(|a| {
            let a = a.as_ref();
            !a.starts_with('-')
                && !a
                    .get(..SCHEME.len())
                    .is_some_and(|p| p.eq_ignore_ascii_case(SCHEME))
        })
        .map(|a| a.as_ref().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDLE: &str = "urn:ietf:params:oauth:request_uri:abc_DEF-123";

    fn consent() -> AppRoute {
        AppRoute::Consent {
            request_uri: HANDLE.into(),
        }
    }

    #[test]
    fn the_first_fauna_argument_is_the_route() {
        let uri = consent().to_uri();
        assert_eq!(
            first_route_arg([
                "fauna-tui",
                "x",
                uri.as_str(),
                "fauna://folder-share?folder=1"
            ]),
            Some(consent())
        );
    }

    #[test]
    fn no_route_argument_is_no_route() {
        assert_eq!(first_route_arg(["fauna-tui"]), None);
        assert_eq!(first_route_arg(["fauna-tui", "--help"]), None);
    }

    #[test]
    fn the_program_name_is_never_the_route() {
        assert_eq!(first_route_arg([consent().to_uri()]), None);
    }

    #[test]
    fn the_scheme_matches_case_insensitively() {
        let upper = format!("FAUNA://consent/{HANDLE}");
        assert_eq!(
            first_route_arg(["fauna-tui", upper.as_str()]),
            Some(consent())
        );
    }

    #[test]
    fn an_unparseable_route_is_dropped() {
        assert_eq!(
            first_route_arg(["fauna-tui", "fauna://consent?folder=1"]),
            None
        );
    }

    #[test]
    fn a_bare_address_is_the_nest_hint() {
        assert_eq!(
            first_nest_hint_arg(["fauna-tui", "nest.example.org"]),
            Some("nest.example.org".into())
        );
        assert_eq!(
            first_nest_hint_arg(["fauna-tui", "localhost:1"]),
            Some("localhost:1".into())
        );
    }

    #[test]
    fn a_route_and_a_hint_land_in_either_order() {
        let uri = consent().to_uri();
        for args in [
            ["fauna-tui", uri.as_str(), "nest.example.org"],
            ["fauna-tui", "nest.example.org", uri.as_str()],
        ] {
            assert_eq!(first_route_arg(args), Some(consent()));
            assert_eq!(first_nest_hint_arg(args), Some("nest.example.org".into()));
        }
        let upper = format!("FAUNA://consent/{HANDLE}");
        assert_eq!(first_nest_hint_arg(["fauna-tui", upper.as_str()]), None);
    }

    #[test]
    fn an_option_is_never_the_nest_hint() {
        assert_eq!(first_nest_hint_arg(["fauna-tui", "--anything"]), None);
        assert_eq!(first_nest_hint_arg(["fauna-tui", "-x"]), None);
    }

    #[test]
    fn the_program_name_alone_is_no_nest_hint() {
        assert_eq!(first_nest_hint_arg(["fauna-tui"]), None);
        assert_eq!(first_nest_hint_arg(["nest.example.org"]), None);
    }

    /// Junk comes back raw — the drop is `set_nest_hint`'s, never this file's.
    #[test]
    fn junk_comes_back_raw() {
        assert_eq!(
            first_nest_hint_arg(["fauna-tui", "evil.example/x?y"]),
            Some("evil.example/x?y".into())
        );
    }
}
