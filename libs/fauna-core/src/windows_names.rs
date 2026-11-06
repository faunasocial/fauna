//! The file-name shapes Windows will not create as an ordinary file — the
//! project's **one** implementation.
//!
//! Two callers write user- or principal-chosen names onto a disk that may be
//! Windows': the mail export's archive namer (`fauna_mail::export::paths`),
//! which renames such a component, and the deposit name check
//! (`fauna_protocol::folders::is_deposit_name`), which refuses one at the door
//! so an item a Windows seat cannot land never parks for ever. Pure and
//! wasm-safe.

/// Whether Windows would open a device instead of creating this component.
///
/// Windows reserves these names **with any extension** and ignoring trailing
/// spaces, so the test reads the part before the first `.` — `aux.txt` and
/// `Aux.Reports` are devices too. The superscript digits are reserved as well.
#[must_use]
pub fn is_windows_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ');
    let numbered = |prefix: &str| {
        let mut rest = stem.chars().skip(prefix.len());
        stem.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            && matches!(
                (rest.next(), rest.next()),
                (Some('0'..='9' | '¹' | '²' | '³'), None)
            )
    };
    ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
        || numbered("COM")
        || numbered("LPT")
}

/// The characters a Windows file name may not carry beyond the separators and
/// the control characters: `:` (a drive prefix such as `C:x`, or an NTFS
/// alternate data stream) and the wildcard and redirection characters.
pub const WINDOWS_FORBIDDEN_NAME_CHARS: [char; 7] = [':', '<', '>', '"', '|', '?', '*'];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_match_with_any_extension_and_case() {
        for device in [
            "CON", "con", "Aux", "nul.txt", "NUL .txt", "com1", "LPT9.log", "COM¹", "conin$",
        ] {
            assert!(is_windows_device_name(device), "{device:?}");
        }
        for plain in ["console", "COM10", "LPT", "aux_", "report.con", "nulx"] {
            assert!(!is_windows_device_name(plain), "{plain:?}");
        }
    }
}
