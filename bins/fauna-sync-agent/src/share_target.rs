//! The agent's `--share-target <path>` mode: the helper a file manager without
//! an in-process IPC client runs to ask the running agent where Share should
//! open (`docs/goal/architecture/apps/linux.md` § The file manager's Share
//! entry (GNOME Files), link 2). It connects to this user's agent, sends the
//! same `ShareFile` verb Explorer's leaf sends, and prints two lines — the
//! route (`fauna_core::app_route::AppRoute::for_share_target`, the one mapping
//! both leaves call) and the leaf's localized label. No target — a refusal, an
//! unreachable agent, a timeout (`SyncPipeClient::REQUEST_TIMEOUT`) — prints
//! nothing and exits non-zero, which is how the extension knows to offer no
//! item. Never a link: the agent is seedless, and the app mints.
//!
//! With `--open` (KDE Dolphin's service menu, which lists Share on every item
//! and cannot itself open a printed URI — `linux.md` → *The KDE Dolphin leg*)
//! the helper also launches the route through the notification tap's opener;
//! without a target it launches nothing and exits non-zero, as always.

use fauna_core::app_route::AppRoute;
use fauna_core::localized::LocalizedText;
use fauna_ipc::sync::{RequestMethod, Response, ResponsePayload, ResponseResult};

/// Ask the running agent for `path`'s share target and print it — and, with
/// `open`, launch it; the process exit code (0 with a target, 1 without or
/// when `open` finds no launcher).
pub fn run(path: &std::path::Path, open: bool) -> i32 {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let resp = fauna_ipc::endpoint::AgentEndpoint::default_for_user()
        .and_then(|endpoint| endpoint.connect())
        .and_then(|client| {
            client.request(RequestMethod::ShareFile {
                path: path.to_string_lossy().into_owned(),
            })
        })
        .ok();
    match render(resp.as_ref()) {
        Some(out) => {
            print!("{out}");
            let route = out.lines().next().unwrap_or_default();
            if open && !open_route(route) {
                return 1;
            }
            0
        }
        None => 1,
    }
}

#[cfg(target_os = "linux")]
fn open_route(route: &str) -> bool {
    crate::push_arm::tap::open_linux_app(route)
}

/// Only the linux file managers run the helper with `--open`.
#[cfg(not(target_os = "linux"))]
fn open_route(_route: &str) -> bool {
    false
}

/// The mode's whole output for a `ShareFile` response: the route URI and the
/// leaf's label on their own lines, or `None` when there is no target.
fn render(resp: Option<&Response>) -> Option<String> {
    let Some(ResponseResult::Ok(ResponsePayload::ShareTarget(target))) = resp.map(|r| &r.result)
    else {
        return None;
    };
    let route = AppRoute::for_share_target(target.folder_id, target.path.clone());
    let label = LocalizedText::key("file_context_menu.share").resolve(fauna_i18n::strings::lookup);
    Some(format!("{}\n{label}\n", route.to_uri()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_ipc::sync::ShareTargetInfo;

    fn ok(target: ShareTargetInfo) -> Response {
        Response {
            id: 1,
            result: ResponseResult::Ok(ResponsePayload::ShareTarget(target)),
        }
    }

    #[test]
    fn a_file_target_prints_its_share_link_route_then_the_label() {
        let out = render(Some(&ok(ShareTargetInfo {
            folder_id: 42,
            path: Some("photos/a b.jpg".into()),
        })))
        .expect("a file target prints");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            [
                "fauna://share-link?folder=42&path=photos/a%20b.jpg",
                "Share"
            ]
        );
        assert_eq!(
            AppRoute::parse(lines[0]),
            Some(AppRoute::ShareLink {
                folder_id: 42,
                path: "photos/a b.jpg".into()
            })
        );
    }

    #[test]
    fn a_set_root_prints_its_folder_share_route() {
        let out = render(Some(&ok(ShareTargetInfo {
            folder_id: 7,
            path: None,
        })))
        .expect("a set root prints");
        assert_eq!(out.lines().next(), Some("fauna://folder-share?folder=7"));
    }

    #[test]
    fn no_target_prints_nothing() {
        let refused = Response {
            id: 1,
            result: ResponseResult::Err("a sub-folder is not a shareable unit".into()),
        };
        assert_eq!(render(Some(&refused)), None);
        assert_eq!(render(None), None);
    }
}
