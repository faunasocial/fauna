package com.fauna.app.testing

/**
 * The agent commands whose arm names a declaration the `kids-safe,kids-floor`
 * bindings lack, each keyed to the plane the kids build excises
 * (`account-age-band.md` § The account age band, the kids app;
 * `dynamic-features.md` § Compile-time excision). Shared by both build types
 * that carry the agent, so [TestAgent]'s dispatch routes the same set the same
 * way on each: `debug` takes the real [KidsExcisedAgent] (`src/agentNoKids`),
 * `kidsDebug` the refusing one (`src/agentKids`), which answers every action
 * here with an error naming its plane — never a silent success (convention 11).
 */
object KidsExcisedActions {
    const val FEED = "feed"
    const val SUBSCRIPTIONS = "subscriptions"
    const val OUTBOX = "outbox"
    const val MAIL = "mail"
    const val ATPROTO = "atproto"
    const val OFFLINE_SHARE = "offline-share"
    const val ARCHIVE_IMPORT = "archive-import"
    const val CRITICAL_ALERTS = "critical-alerts"

    /**
     * Action → the excised plane its arm drives: every command [TestAgent]'s
     * dispatch hands [KidsExcisedAgent.dispatch]. `alert_sweep_wake`
     * ([CRITICAL_ALERTS]) is the one excised seam outside it: it keeps its
     * home in [E2eLoudSurfaces], which calls [KidsExcisedAgent.alertSweepWake].
     */
    val PLANES: Map<String, String> = mapOf(
        "feed_inject_posts" to FEED,
        "feed_inject_error" to FEED,
        "feed_seed_cue_rollup_for_test" to FEED,
        "feed_cue_verdict_for_test" to FEED,
        "feed_hold_next_reload" to FEED,
        "feed_release_held_reload" to FEED,
        "feed_cue_clock_freeze" to FEED,
        "feed_cue_clock_thaw" to FEED,
        "feed_cue_clock_advance" to FEED,
        "feed_cue_probe" to FEED,
        "feed_cue_scroll_by" to FEED,
        "subscriptions_hold_author_drain" to SUBSCRIPTIONS,
        "subscriptions_release_author_drain" to SUBSCRIPTIONS,
        "app_outbox_drain_now" to OUTBOX,
        "outbox_drain_hold" to OUTBOX,
        "enable_caldav_mailbox" to MAIL,
        "atproto_delegation_advance_clock" to ATPROTO,
        "offline_share_advance_clock" to OFFLINE_SHARE,
        "offline_share_drop_connections" to OFFLINE_SHARE,
        "offline_share_hold_serves" to OFFLINE_SHARE,
        "archive_import_pause_after" to ARCHIVE_IMPORT,
        "archive_import_hold_after" to ARCHIVE_IMPORT,
    )

    /** The refusal the kids build answers [action] with. */
    fun refusal(action: String, plane: String? = PLANES[action]): String =
        "$action: the ${plane ?: "unknown"} plane is excised from the kids build"
}
