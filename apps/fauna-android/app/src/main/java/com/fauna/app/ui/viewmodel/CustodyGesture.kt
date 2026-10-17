package com.fauna.app.ui.viewmodel

/**
 * One custody gesture on Settings → Devices (`docs/goal/ui/devices.md` § Custody
 * facet), raised by a control on the page and run by [DevicesVM.runCustodyGesture]
 * through the shared `fauna_client_custody::run_custody_act`, whose error reaches
 * the page's `error-message`. The android twin of linux's `CustodyGesture`
 * (`apps/fauna-linux/src/views/devices_folders/custody.rs`) — one gesture type,
 * one page dispatcher.
 *
 * Every row gesture carries its GRANT ID, never a row index: a facet refresh
 * re-orders rows, so an index captured at paint time can address a different
 * custody by the time the act runs.
 */
sealed interface CustodyGesture {
    /** `custody-holder-revoke-button` — `holder` is the row's accept-bound custodian key. */
    class Revoke(val grantId: ByteArray, val holder: ByteArray?) : CustodyGesture

    /** `custody-offer-accept-button` — `onNest` is the target select's answer. */
    class Accept(val grantId: ByteArray, val onNest: Boolean) : CustodyGesture

    /** `custody-offer-decline-button`. */
    class Decline(val grantId: ByteArray) : CustodyGesture

    /**
     * The `custody-held-budget-input` commit — the typed text, parsed by the VM
     * with the shared `parseByteSize` (an unparseable or zero budget makes no
     * call and says so).
     */
    class SetBudget(val grantId: ByteArray, val typed: String) : CustodyGesture

    /** `custody-held-stop-button` — pauses the hold, keeps the bytes. */
    class Stop(val grantId: ByteArray) : CustodyGesture

    /** `custody-held-remove-button` — the reclaim. */
    class Remove(val grantId: ByteArray) : CustodyGesture

    /**
     * `custody-mint-button` — the VM reads the candidates and opens the flow, or
     * answers `devices.custody_mint_no_contacts` when there is no one to ask.
     */
    data object MintOpen : CustodyGesture

    /** `custody-mint-cancel-button` — close the flow without sending. */
    data object MintCancel : CustodyGesture

    /**
     * `custody-mint-confirm-button` over one candidate row's `host` +
     * `channelHex`, passed back unchanged — the page picks a row, it never
     * assembles a channel.
     */
    class Mint(val host: ByteArray, val channelHex: String) : CustodyGesture
}
