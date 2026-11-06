//! The `error-message` texts a folder row's background reads put up
//! (`folders.md` § Errors & edge cases, "A folder row's own reads retire their
//! own error"), one slot per (read, set), so a later good answer of the same
//! read retires exactly that text and never a gesture's.
//!
//! The rows' device, member, actor and destination reads fire on expand, and a
//! reconnect re-runs them (`StaleSurfaces::devices`). Without the slot their
//! failure, "the connection to the nest was lost" painted while the nest is
//! down, stands on the banner after the nest came back and the same reads
//! answered.

use std::collections::HashMap;

/// What each row read put on the banner; see the module docs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RowReadErrors(HashMap<(String, String), String>);

impl RowReadErrors {
    /// Remember that `read` for `set` put `message` on the banner.
    pub fn put(&mut self, read: &str, set: &str, message: String) {
        self.0.insert((read.to_owned(), set.to_owned()), message);
    }

    /// `read` answered for `set`: forget its error, and answer whether the
    /// banner, now showing `shown`, should be cleared. Only when it still shows
    /// that read's own text, and no other row read still owns the same text
    /// (two open rows failing one read paint identical words).
    pub fn retire(&mut self, read: &str, set: &str, shown: &str) -> bool {
        let Some(own) = self.0.remove(&(read.to_owned(), set.to_owned())) else {
            return false;
        };
        own == shown && !self.0.values().any(|m| m == shown)
    }

    /// `set`'s row closed, so no later read of it will answer: forget every
    /// error it holds, and answer whether the banner should be cleared, under
    /// [`Self::retire`]'s rule.
    pub fn retire_set(&mut self, set: &str, shown: &str) -> bool {
        let mut owned = false;
        self.0.retain(|(_, s), message| {
            let ours = s == set;
            owned |= ours && message == shown;
            !ours
        });
        owned && !self.0.values().any(|m| m == shown)
    }

    /// Whether the banner, now showing `shown`, is some row read's text. For an
    /// app whose page machine repaints the banner on every tick: that tick must
    /// leave a row read's text for the read itself to retire.
    pub fn owns(&self, shown: &str) -> bool {
        self.0.values().any(|m| m == shown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_good_row_read_retires_the_error_it_put_up() {
        let mut own = RowReadErrors::default();
        let lost = "fauna.backup.destination.list: The connection to the nest was lost.";
        own.put("fauna.backup.destination.list", "Photos", lost.into());
        assert!(own.retire("fauna.backup.destination.list", "Photos", lost));
        // Forgotten once retired: a second answer clears nothing.
        assert!(!own.retire("fauna.backup.destination.list", "Photos", lost));
    }

    #[test]
    fn a_good_row_read_leaves_a_gesture_error_standing() {
        let mut own = RowReadErrors::default();
        own.put(
            "fauna.folders.members.list",
            "Photos",
            "fauna.folders.members.list: lost".into(),
        );
        assert!(!own.retire(
            "fauna.folders.members.list",
            "Photos",
            "Couldn't save the paths."
        ));
    }

    #[test]
    fn a_good_row_read_leaves_another_reads_error_standing() {
        let mut own = RowReadErrors::default();
        let members = "fauna.folders.members.list: lost";
        let places = "fauna.backup.destination.list: lost";
        own.put("fauna.folders.members.list", "Photos", members.into());
        own.put("fauna.backup.destination.list", "Photos", places.into());
        assert!(!own.retire("fauna.folders.members.list", "Photos", places));
        assert!(own.retire("fauna.backup.destination.list", "Photos", places));
    }

    #[test]
    fn a_good_row_read_leaves_the_same_text_another_open_row_still_owns() {
        let mut own = RowReadErrors::default();
        let lost = "fauna.folders.devices: lost";
        own.put("fauna.folders.devices", "Photos", lost.into());
        own.put("fauna.folders.devices", "Notes", lost.into());
        assert!(!own.retire("fauna.folders.devices", "Photos", lost));
        assert!(own.retire("fauna.folders.devices", "Notes", lost));
    }

    #[test]
    fn a_read_that_never_failed_clears_nothing() {
        let mut own = RowReadErrors::default();
        assert!(!own.retire("fauna.folders.devices", "Photos", ""));
    }

    #[test]
    fn owns_names_a_row_reads_text_and_nothing_else() {
        let mut own = RowReadErrors::default();
        let lost = "fauna.folders.devices: lost";
        own.put("fauna.folders.devices", "Photos", lost.into());
        assert!(own.owns(lost));
        assert!(!own.owns("Couldn't save the paths."));
        assert!(!own.owns(""));
        assert!(own.retire("fauna.folders.devices", "Photos", lost));
        assert!(!own.owns(lost));
    }

    #[test]
    fn a_closed_row_retires_what_it_put_up_and_only_that() {
        let mut own = RowReadErrors::default();
        let lost = "fauna.folders.devices: lost";
        own.put("fauna.folders.devices", "Photos", lost.into());
        assert!(!own.retire_set("Photos", "Couldn't save the paths."));

        own.put("fauna.folders.devices", "Photos", lost.into());
        own.put("fauna.folders.devices", "Notes", lost.into());
        assert!(!own.retire_set("Photos", lost), "Notes still owns the text");
        assert!(own.retire_set("Notes", lost));
    }
}
