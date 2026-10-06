//! The launch path's clock — the one `now` every clock read in this crate is
//! made against (the bearer deadline's anchoring — `login.md` § Token lifetime
//! on the client's clock — the own-session-id pruning, the TTL-refresh schedule,
//! the factory-reset claim grace), plus the
//! e2e offset that lets a test make a client's clock *wrong*.
//!
//! **Why it exists.** `docs/goal/behavior/login.md` § Goal promises that the app
//! launch fast path — the silent challenge — is immune to client clock skew: it
//! signs `actor_id ‖ nonce`, no timestamp, so a freshly-booted device whose NTP
//! has not run yet still signs in. That is true by construction, and exactly the
//! kind of truth that regresses silently the day someone unifies the two auth
//! ceremonies (the handshake carries a client timestamp the nest holds to
//! ±30 s). Witnessing it through a real app needs a real app with a wrong clock,
//! and the machines are shared, so the system clock is never touchable; this
//! module is the seam instead.
//!
//! **One clock, owned one crate down.** The clock itself — the real clock
//! plus the compile-gated e2e offset seeded from `FAUNA_E2E_CLOCK_OFFSET_SECS`
//! — is [`fauna_protocol::client_clock`]; this module is the launch path's
//! name for it. It used to hold its own offset static, the third instance of
//! the `audit_clock`/`delegation_clock` shape; that moved down so the mint
//! anchor (`fauna_anon_client::MintedBearer`) and `fauna-client`'s
//! `TokenCache` — the refresh clock of the four UniFFI apps, which this crate
//! cannot reach and which cannot reach this crate — read the SAME offset: a wrong-clock witness on a seat whose
//! refresh read another clock would pass without testing anything. The
//! per-domain clocks stay separate from this one on purpose (the
//! `client_clock` docs say why). Putting the offset under
//! `fauna_core::data::Timestamp` stays refused: `Timestamp` is the **nest's**
//! clock too, and an offset there would skew the server side of the very
//! ±30 s check the case-L witness leans on.
//!
//! **Why the seed is an environment variable.** The offset must be live
//! *before* [`crate::LaunchMachine::start`] runs the silent challenge, and
//! every app's automation agent starts in the same startup sequence that
//! immediately drives the launch, so no harness command can land first (the
//! seed's mechanics are `client_clock`'s). Web's browser has no environment,
//! so `fauna-wasm-launch`'s test flavor seeds it through
//! [`set_clock_offset_secs`] from a localStorage key named [`OFFSET_ENV`].
//! Compile-gated outer, env inner (convention 15): this crate's `e2e-agent`
//! forwards `fauna-protocol/e2e-agent`, and a release build without it has
//! neither the static nor the read.
//!
//! **What it deliberately does NOT reach.** The nest's own clock (above).
//!
//! Spec: `docs/goal/behavior/login.md` § Goal and § Silent Challenge (the
//! immunity), § Direct Auth (the handshake's ±30 s window this offset is
//! measured against), § E2E test login (the witnesses); the witness is
//! `docs/features/connect-and-sign-in.md` outcomes 9 and 10.

/// The environment variable the offset is seeded from
/// ([`fauna_protocol::client_clock::OFFSET_ENV`]). Gated with the static it
/// seeds (convention 15).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub use fauna_protocol::client_clock::OFFSET_ENV;

/// Set the client clock's offset
/// ([`fauna_protocol::client_clock::set_clock_offset_secs`]). ⚠
/// **Process-wide and nothing auto-resets it** — it moves the launch, the
/// bearer anchor and every refresh schedule this process makes.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub use fauna_protocol::client_clock::set_clock_offset_secs;

/// The launch clock's offset, in seconds — zero in every real run and in
/// every release build ([`fauna_protocol::client_clock::clock_offset_secs`]).
pub use fauna_protocol::client_clock::clock_offset_secs;

/// Epoch **milliseconds** on the launch clock — the real clock (or `0` when
/// it cannot be read) plus the offset. The TTL-refresh loop's `now` is read
/// here.
pub use fauna_protocol::client_clock::now_millis_or_zero;

/// Epoch **seconds** on the launch clock, same `0`-fold as
/// [`now_millis_or_zero`].
pub use fauna_protocol::client_clock::now_secs_or_zero;

#[cfg(test)]
mod tests {
    use super::*;

    /// The launch clock and the client clock every app-held bearer's refresh
    /// reads are ONE clock: moving it here moves the one `fauna-client`'s
    /// `TokenCache` and `fauna-anon-client`'s mint anchor read. Two statics
    /// would let case M go green on a seat whose refresh never saw the offset.
    /// One test for the process-global reason; reset before returning.
    #[test]
    fn the_launch_clock_is_the_client_clock() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                set_clock_offset_secs(0);
            }
        }
        let _reset = Reset;

        set_clock_offset_secs(-18_000);
        assert_eq!(fauna_protocol::client_clock::clock_offset_secs(), -18_000);
        assert!(
            (now_secs_or_zero() - fauna_protocol::client_clock::now_secs_or_zero()).abs() <= 10,
            "the launch clock must read the client clock's skewed instant"
        );
        fauna_protocol::client_clock::set_clock_offset_secs(7_200);
        assert_eq!(clock_offset_secs(), 7_200);

        set_clock_offset_secs(0);
        assert!(
            (fauna_core::data::Timestamp::now_secs_or_zero() - now_secs_or_zero()).abs() <= 10,
            "reset must return the launch clock to the real one"
        );
    }
}
