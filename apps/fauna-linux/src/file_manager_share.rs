//! The file manager's Share entry, installed by the app
//! (`docs/goal/architecture/apps/linux.md` § The file manager's Share entry
//! (GNOME Files), link 2, and → *The KDE Dolphin leg*): GNOME Files' module
//! and Dolphin's service menu, both written by the same hook from the same
//! agent command.
//!
//! The module (`packaging/nautilus/fauna_share.py`, a nautilus-python
//! `MenuProvider`) runs the sync agent's `--share-target` helper, so it must
//! name this install channel's agent — the same command the agent's systemd
//! user unit runs. The universal post-auth hook writes it with that command
//! filled in from the one shared derivation (`UnitExec::exec_start`, through
//! [`crate::sync_agent::channel_unit_exec`]), the autostart entry's shape
//! (`crate::autostart`): every login re-writes it, which self-heals a moved
//! AppImage or an upgraded install, and no second spelling of the agent's path
//! exists anywhere. The Flatpak build reaches the host's directory through its
//! `xdg-data/nautilus-python/extensions:create` grant.
//!
//! Dolphin's service menu (`<data home>/kio/servicemenus/`) carries the static
//! keys alone — KIO 6 reads no per-selection gate — so Share is listed on every
//! single local item and its `Exec` runs the helper with `--open`, which
//! launches the route or, on an ineligible item, opens nothing.

use std::path::{Path, PathBuf};

use fauna_client_sync::agent_spawner::UnitExec;

/// The module's source, carrying one agent-command slot.
const MODULE_SOURCE: &str = include_str!("../packaging/nautilus/fauna_share.py");

/// The slot, a Python string literal the agent command replaces.
const AGENT_COMMAND_SLOT: &str = "\"@AGENT_COMMAND@\"";

/// The file nautilus-python loads from its per-user extension directory.
pub const MODULE_FILE: &str = "fauna_share.py";

/// The service menu Dolphin loads from its per-user directory.
pub const SERVICE_MENU_FILE: &str = "social.fauna.fauna-share.desktop";

/// The module with `exec`'s agent command in its slot, as a Python string
/// literal (a JSON string is one: `UnitExec` has already refused every control
/// character, and the rest escapes the same way in both).
pub fn module_contents(exec: &UnitExec) -> String {
    let literal = serde_json::to_string(exec.exec_start()).expect("a string serializes");
    MODULE_SOURCE.replacen(AGENT_COMMAND_SLOT, &literal, 1)
}

/// The KIO service menu running `exec`'s agent command, or `None` when that
/// command cannot be put in a desktop entry's `Exec` as it stands: `\\`, `$`
/// and `` ` `` would need re-quoting the unit's spelling never had, so they
/// are refused rather than rewritten (`UnitExec`'s own rule). `%` doubles, the
/// field-code escape. The flags precede the path because `--share-target`
/// takes the value after it.
pub fn service_menu_contents(exec: &UnitExec) -> Option<String> {
    let command = exec.exec_start();
    if command.contains(['\\', '$', '`']) {
        tracing::warn!(
            "the agent command cannot be a service menu's Exec — writing no Dolphin entry"
        );
        return None;
    }
    let command = command.replace('%', "%%");
    let lookup = fauna_i18n::strings::lookup;
    let submenu = fauna_core::localized::LocalizedText::key("common.app_name").resolve(lookup);
    let share =
        fauna_core::localized::LocalizedText::key("file_context_menu.share").resolve(lookup);
    Some(format!(
        "[Desktop Entry]\n\
         Type=Service\n\
         MimeType=application/octet-stream;inode/directory;\n\
         X-KDE-Protocols=file\n\
         X-KDE-RequiredNumberOfUrls=1\n\
         X-KDE-Submenu={submenu}\n\
         Actions=share;\n\
         \n\
         [Desktop Action share]\n\
         Name={share}\n\
         Exec={command} --open --share-target %f\n"
    ))
}

/// The host's `<data home>` (`$XDG_DATA_HOME`, or `~/.local/share`).
/// Inside Flatpak the sandbox redirects `XDG_DATA_HOME` to an app-private
/// directory GNOME Files never reads, while `$HOME` is the real host home and
/// the grant mounts the host's directory at its own path — so a sandboxed app
/// writes under `$HOME/.local/share`, as its host unit is written under
/// `$HOME/.config` (`crate::sync_agent`).
fn data_home_from(
    home: Option<&Path>,
    xdg_data_home: Option<&Path>,
    in_flatpak: bool,
) -> Option<PathBuf> {
    match xdg_data_home.filter(|p| !in_flatpak && p.is_absolute()) {
        Some(xdg) => Some(xdg.to_path_buf()),
        None => Some(
            home.filter(|h| h.is_absolute())?
                .join(".local")
                .join("share"),
        ),
    }
}

fn module_path_from(data_home: &Path) -> PathBuf {
    data_home
        .join("nautilus-python")
        .join("extensions")
        .join(MODULE_FILE)
}

/// `<data home>/kio/servicemenus/social.fauna.fauna-share.desktop`, the
/// per-user directory KIO reads since KDE Frameworks 5.85; Flatpak through
/// its `xdg-data/kio/servicemenus:create` grant.
fn service_menu_path_from(data_home: &Path) -> PathBuf {
    data_home
        .join("kio")
        .join("servicemenus")
        .join(SERVICE_MENU_FILE)
}

fn data_home() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let in_flatpak = std::env::var_os("FLATPAK_ID").is_some_and(|id| !id.is_empty());
    data_home_from(home.as_deref(), xdg.as_deref(), in_flatpak)
}

/// Write `contents` at `path` with `mode` unless it already holds both.
/// Best-effort, like the autostart entry: a directory we cannot write never
/// blocks a launch.
fn write_file_at(path: &Path, contents: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mode_of = |p: &Path| std::fs::metadata(p).map(|m| m.permissions().mode() & 0o777);
    if !std::fs::read_to_string(path).is_ok_and(|on_disk| on_disk == contents) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(path, contents) {
            tracing::warn!("could not write the file manager's Share entry: {e}");
            return;
        }
    }
    if mode_of(path).is_ok_and(|m| m != mode) {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
}

/// The post-auth hook's step (`launch_authenticated`). Never under e2e
/// automation — a harness login must not write the dev machine's real
/// extension directories — and not on a channel whose agent the host cannot
/// reach (Snap).
pub fn install_at_post_auth() {
    if crate::e2e_mode_enabled() {
        return;
    }
    let (Some(exec), Some(data_home)) = (crate::sync_agent::channel_unit_exec(), data_home())
    else {
        return;
    };
    write_file_at(
        &module_path_from(&data_home),
        &module_contents(&exec),
        0o644,
    );
    // KIO authorizes a per-user service menu only with its executable bit.
    if let Some(menu) = service_menu_contents(&exec) {
        write_file_at(&service_menu_path_from(&data_home), &menu, 0o755);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_carries_exactly_one_agent_command_slot() {
        assert_eq!(MODULE_SOURCE.matches(AGENT_COMMAND_SLOT).count(), 1);
    }

    /// The module names the agent exactly as the channel's unit `ExecStart=`
    /// does — the same string, quoting included.
    #[test]
    fn the_written_module_runs_the_units_agent_command() {
        let exec = UnitExec::for_binary(PathBuf::from("/opt/my apps/fauna-sync-agent")).unwrap();
        let module = module_contents(&exec);
        assert!(!module.contains(AGENT_COMMAND_SLOT));
        assert!(
            module.contains(r#"AGENT_COMMAND = "\"/opt/my apps/fauna-sync-agent\"""#),
            "{module}"
        );
        assert!(
            fauna_client_sync::agent_spawner::unit_contents(&exec)
                .contains("ExecStart=\"/opt/my apps/fauna-sync-agent\"\n")
        );
    }

    fn data_home(home: &str, xdg: Option<&str>, in_flatpak: bool) -> PathBuf {
        data_home_from(Some(Path::new(home)), xdg.map(Path::new), in_flatpak).unwrap()
    }

    #[test]
    fn a_native_install_writes_under_the_xdg_data_home() {
        let xdg = data_home("/home/u", Some("/data/u"), false);
        assert_eq!(
            module_path_from(&xdg),
            PathBuf::from("/data/u/nautilus-python/extensions/fauna_share.py")
        );
        assert_eq!(
            service_menu_path_from(&xdg),
            PathBuf::from("/data/u/kio/servicemenus/social.fauna.fauna-share.desktop")
        );
        assert_eq!(
            module_path_from(&data_home("/home/u", None, false)),
            PathBuf::from("/home/u/.local/share/nautilus-python/extensions/fauna_share.py")
        );
    }

    /// The sandbox's `XDG_DATA_HOME` is app-private; the host's directories
    /// are under the real `$HOME`.
    #[test]
    fn a_flatpak_install_writes_the_hosts_directories() {
        let host = data_home(
            "/home/u",
            Some("/home/u/.var/app/social.fauna.fauna/data"),
            true,
        );
        assert_eq!(
            module_path_from(&host),
            PathBuf::from("/home/u/.local/share/nautilus-python/extensions/fauna_share.py")
        );
        assert_eq!(
            service_menu_path_from(&host),
            PathBuf::from("/home/u/.local/share/kio/servicemenus/social.fauna.fauna-share.desktop")
        );
    }

    #[test]
    fn no_home_writes_nothing() {
        assert_eq!(data_home_from(None, None, false), None);
    }

    #[test]
    fn an_unchanged_file_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ext").join(MODULE_FILE);
        write_file_at(&path, "one", 0o644);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        write_file_at(&path, "one", 0o644);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        write_file_at(&path, "two", 0o644);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    }

    /// Dolphin's service menu: the documented static keys, no gate key, and
    /// the helper run through the unit's very agent command (witness (d)).
    #[test]
    fn the_service_menu_carries_the_static_keys_and_the_units_agent_command() {
        let exec = UnitExec::for_binary(PathBuf::from("/opt/my apps/fauna-sync-agent")).unwrap();
        let menu = service_menu_contents(&exec).expect("a plain path is an Exec");
        let lines: Vec<&str> = menu.lines().collect();
        for key in [
            "[Desktop Entry]",
            "Type=Service",
            "MimeType=application/octet-stream;inode/directory;",
            "X-KDE-Protocols=file",
            "X-KDE-RequiredNumberOfUrls=1",
            "X-KDE-Submenu=Fauna",
            "Actions=share;",
            "[Desktop Action share]",
            "Name=Share",
            r#"Exec="/opt/my apps/fauna-sync-agent" --open --share-target %f"#,
        ] {
            assert!(lines.contains(&key), "{key:?} missing from\n{menu}");
        }
        for gate in [
            "X-KDE-ShowIfDBusCall",
            "X-KDE-GetActionMenu",
            "X-KDE-ShowIfRunning",
        ] {
            assert!(!menu.contains(gate), "KIO 6 reads no {gate}:\n{menu}");
        }
    }

    /// Every channel's command spelling, verbatim — `flatpak run`'s line and
    /// the AppImage's `--sync-agent` dispatch included.
    #[test]
    fn the_service_menu_exec_is_the_channels_command_verbatim() {
        for command in [
            "/usr/bin/flatpak run --command=fauna-sync-agent \"social.fauna.fauna\"",
            "\"/home/u/Apps/Fauna.AppImage\" --sync-agent",
        ] {
            let exec = UnitExec::new(command.into(), PathBuf::from("/x")).unwrap();
            let menu = service_menu_contents(&exec).unwrap();
            assert!(
                menu.contains(&format!("\nExec={command} --open --share-target %f\n")),
                "{menu}"
            );
        }
    }

    /// `%` is a field code in `Exec`; `\`, `$` and `` ` `` would need a
    /// re-quoting the unit never had, so no entry is written for them.
    #[test]
    fn an_exec_that_cannot_be_spelled_is_refused_and_percent_doubles() {
        let pct = UnitExec::for_binary(PathBuf::from("/opt/100%/fauna-sync-agent")).unwrap();
        assert!(
            service_menu_contents(&pct)
                .unwrap()
                .contains(r#"Exec="/opt/100%%/fauna-sync-agent" "#)
        );
        for bad in ["/opt/a$b/agent", "/opt/a`b/agent", "/opt/a\\b/agent"] {
            let exec = UnitExec::for_binary(PathBuf::from(bad)).unwrap();
            assert_eq!(service_menu_contents(&exec), None, "{bad}");
        }
    }

    /// KIO authorizes a per-user service menu only with its executable bit,
    /// and a file whose content is current still gets the bit back.
    #[test]
    fn the_service_menu_is_written_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = service_menu_path_from(dir.path());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        write_file_at(&path, "menu", 0o755);
        assert_eq!(mode(&path), 0o755);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_file_at(&path, "menu", 0o755);
        assert_eq!(mode(&path), 0o755);
    }
}
