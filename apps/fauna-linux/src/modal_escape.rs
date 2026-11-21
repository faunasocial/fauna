//! Escape closes a modal window — the keyboard's way out of every dialog, the
//! same close its title-bar button makes (web's dialogs, tui's Esc arm; GNOME's
//! own `adw::Dialog` binds it, a hand-built modal `adw::Window` does not).
//!
//! One binding for every modal `adw::Window` the app builds, so no dialog is
//! left without it: a dialog that ignored Escape trapped a keyboard user behind
//! its pointer-only close, and left the e2e actuation sweep unable to bring its
//! page back to the entry frame (`e2e-systematic-ui-walks.md` § Implementation
//! status today, the possibility-invariants bullet — the restore presses Escape
//! on the overlay's own records, `automation::agent::press_escape`).

use gtk::prelude::*;

/// Bind Escape on `window` to `window.close()` — the close-request path the
/// title-bar button takes, so a dialog's own close handling runs unchanged.
pub(crate) fn close_on_escape(window: &impl IsA<gtk::Window>) {
    let w = window.as_ref().downgrade();
    let escape = gtk::ShortcutController::new();
    escape.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::CallbackAction::new(move |_, _| {
            if let Some(w) = w.upgrade() {
                w.close();
            }
            gtk::glib::Propagation::Stop
        })),
    ));
    window.as_ref().add_controller(escape);
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Every modal `adw::Window` the app builds binds Escape through
    /// [`super::close_on_escape`] in the statement after its builder — a new
    /// dialog that forgets it is named here, not met by a keyboard user or the
    /// actuation sweep's restore.
    #[test]
    fn every_modal_window_binds_escape() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut missing = Vec::new();
        let mut seen = 0;
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let lines: Vec<&str> = text.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    let Some((_, rest)) = line.split_once("let ") else {
                        continue;
                    };
                    let Some((var, tail)) = rest.split_once(" = ") else {
                        continue;
                    };
                    if !tail.starts_with("adw::Window::builder()") {
                        continue;
                    }
                    let end = (i..lines.len())
                        .find(|&j| lines[j].trim_end().ends_with(".build();"))
                        .unwrap();
                    if !lines[i..=end].iter().any(|l| l.contains(".modal(true)")) {
                        continue;
                    }
                    seen += 1;
                    let bound = format!("close_on_escape(&{var})");
                    if !lines[end + 1..(end + 40).min(lines.len())]
                        .iter()
                        .any(|l| l.contains(&bound))
                    {
                        missing.push(format!("{}:{}", path.display(), i + 1));
                    }
                }
            }
        }
        assert!(
            seen > 0,
            "the scan found no modal window — the pattern drifted"
        );
        assert!(
            missing.is_empty(),
            "modal windows with no Escape close: {missing:?}"
        );
    }
}
