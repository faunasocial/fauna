import Foundation

// Compiled out of release artifacts (testing.md convention 15): the UniFFI
// export it drives (`appOutboxDrainNowForTest`) exists only in the
// `test-helpers` flavor.
#if DEBUG

/// Shared handler for the cross-app `app_outbox_drain_now` TestAgent command
/// (`fauna_e2e_agent::APP_OUTBOX_DRAIN_NOW`, which owns the contract; tui's arm
/// is the reference). It rings the process outbox's own app-only drain — the
/// queued MLS sends and identity-bound intents — through fauna-ffi
/// (`e2e_seams::app_outbox_drain_now_for_test` → `offline_outbox::DEVICE`), the
/// same loop the lend, the store's ready edge and every offline→online word
/// start, never a second one. A test that has just deleted the e2e drain-hold
/// file (`helpers/outbox_drain.py`) rings it, because the held pass ended the
/// loop (`take-your-account-back` outcome 52's witness,
/// `test_identity_succession_outbox_carry.py`).
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation.
/// Fire-and-forget: a no-op pre-auth or before the first connection word, and
/// the barrier is the effect the caller polls for, never this ack.
public enum AppOutboxDrainTestCommand {
    public static let action = "app_outbox_drain_now"

    public static func apply() {
        appOutboxDrainNowForTest()
    }
}

#endif
