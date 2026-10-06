//! The `fauna://` in-app route's intake (`apps/tui.md` § System integration →
//! *In-app routes*).
//!
//! tui registers no OS URL-scheme handler (absence 8): a route reaches it as
//! the first `fauna://` argument on its command line — the argv leg the windows
//! app reads too (`App.xaml.cs`'s `RouteFromArgs`). The grammar is the one
//! shared `fauna_core::app_route` — this file never parses a URI itself. The
//! parsed route is held on `App` and applied through its one door,
//! [`crate::app::App::apply_route`], once the session is signed in.

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
}
