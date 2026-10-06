//! Forwarding configuration validators — the shared core behind the
//! forward-all knob and the per-account forward rate
//! (`docs/goal/behavior/mail-forwarding.md`).
//!
//! Pure (std only): no tokio/DNS/parser, so it is WASM-safe and an app
//! validates with the *same* predicate the nest set-RPC enforces (priority #2
//! — write the rule once, share it).
//!
//! The forward-target address check ([`validate_forward_target`], `:31` and
//! `:244`) lives in the wasm-safe protocol base beside the filter wire enums,
//! because the shared rule-editor encoder
//! (`fauna_protocol::email::encode_filter_action`) validates a per-rule
//! destination with it too; it is re-exported here for the nest and
//! forward-all callers.

use thiserror::Error;

pub use fauna_protocol::email::{ForwardTargetError, validate_forward_target};

/// Per-account forward rate cap default (`mail.account.forward_per_hour`,
/// Tier-3 in `mail-forwarding.md` § Per-account forward rate-limit `:174`).
/// This is the DB column default on `mail_account_settings.forward_per_hour`
/// and the value the rate-cap reads when the per-account knob is unset.
pub const FORWARD_PER_HOUR_DEFAULT: u32 = 100;

/// Admin-side forward rate ceiling (`mail.outbound.forward_max_per_account_
/// per_hour`, Tier-2 in `mail-forwarding.md` `:173`). The per-account cap can
/// be lower than this but never higher; the effective cap is
/// `min(forward_per_hour, FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING)`.
/// Hardcoded here (like the alias [`crate::aliases::EXACT_ALIASES_MAX_DEFAULT`])
/// until the outbound-policy write-path lands the admin knob
/// (`mail-policy-config.md:327` — most knobs stuck at compile-time default).
pub const FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING: u32 = 500;

/// The per-account forward **queue** ceiling multiplier
/// (`mail-forwarding.md` § Queue ceiling `:183`): the holding queue holds at
/// most `min(account-cap, admin-ceiling) * FORWARD_QUEUE_CEILING_MULTIPLIER`
/// messages before FIFO newest-evicts-oldest kicks in (default 100 × 24 =
/// 2400). 24 = "one day of forwards at the cap".
pub const FORWARD_QUEUE_CEILING_MULTIPLIER: u32 = 24;

/// Why a per-account forward cap (`mail.account.forward_per_hour`) was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ForwardPerHourError {
    /// Zero would park every forward until the queue evicts it — a setting that
    /// silently loses mail rather than limiting it.
    #[error("the hourly forward limit must be at least 1")]
    Zero,
    /// The per-account cap may be lower than the admin ceiling, never higher
    /// (`mail-forwarding.md` § Per-account forward rate-limit).
    #[error("the hourly forward limit cannot exceed {ceiling}")]
    AboveCeiling { ceiling: u32 },
}

/// Validate a per-account hourly forward cap against the admin `ceiling`
/// (today [`FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING`]; the caller passes it so
/// the admin tier stays the one source). The nest enforces this at
/// `set_forward_per_hour`; an app may run it first to explain a refusal before
/// the round trip.
pub fn validate_forward_per_hour(value: u32, ceiling: u32) -> Result<(), ForwardPerHourError> {
    if value == 0 {
        return Err(ForwardPerHourError::Zero);
    }
    if value > ceiling {
        return Err(ForwardPerHourError::AboveCeiling { ceiling });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_per_hour_accepts_one_through_the_ceiling() {
        for ok in [1, FORWARD_PER_HOUR_DEFAULT, 499, 500] {
            assert_eq!(validate_forward_per_hour(ok, 500), Ok(()), "value {ok}");
        }
    }

    #[test]
    fn forward_per_hour_refuses_zero_and_above_the_ceiling() {
        assert_eq!(
            validate_forward_per_hour(0, 500),
            Err(ForwardPerHourError::Zero)
        );
        assert_eq!(
            validate_forward_per_hour(501, 500),
            Err(ForwardPerHourError::AboveCeiling { ceiling: 500 })
        );
        // The ceiling is the caller's (the admin tier), never assumed.
        assert_eq!(
            validate_forward_per_hour(100, 50),
            Err(ForwardPerHourError::AboveCeiling { ceiling: 50 })
        );
    }
}
