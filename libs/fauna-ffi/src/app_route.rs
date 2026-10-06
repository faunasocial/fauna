//! The `fauna://` in-app routes' FFI face — `fauna_core::app_route`, the one
//! grammar every producer and every app shares (the Windows Explorer Share
//! leaf builds a route; the app parses it off its launch arguments). Applying a
//! route is navigation only (`docs/goal/architecture/apps/windows.md` § Shell
//! Extension → *The Share hand-off*).

use fauna_core::app_route::AppRoute;

/// Parse a `fauna://` route URI; `None` for anything that is not exactly one
/// of the known routes (never an error — an app ignores what it cannot open).
#[uniffi::export]
pub fn parse_app_route(uri: String) -> Option<AppRoute> {
    AppRoute::parse(&uri)
}

/// The URI for `route`.
#[uniffi::export]
pub fn app_route_uri(route: AppRoute) -> String {
    route.to_uri()
}
