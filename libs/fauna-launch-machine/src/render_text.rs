//! Display text shared between tui and linux (the two native shells that
//! render [`crate::snapshots::LaunchPhase::Offline`]'s `transient` arm
//! directly in Rust).

/// The transient-retry surface's error body: the machine's own `last_error`
/// when it carries one, else the localized generic. The machine's message
/// names the actual fault ("connection refused", a 5xx), which is strictly
/// more useful than the generic — and the generic keeps the element
/// non-empty (and so registered) when the machine had nothing to say.
pub fn transient_error_text(error: &str) -> String {
    if error.is_empty() {
        fauna_i18n::strings::onboarding::launch::TRANSIENT_ERROR.to_string()
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_error_renders_verbatim() {
        assert_eq!(
            transient_error_text("connection refused"),
            "connection refused"
        );
    }

    #[test]
    fn an_empty_error_falls_back_to_the_localized_generic() {
        assert_eq!(
            transient_error_text(""),
            fauna_i18n::strings::onboarding::launch::TRANSIENT_ERROR
        );
    }
}
