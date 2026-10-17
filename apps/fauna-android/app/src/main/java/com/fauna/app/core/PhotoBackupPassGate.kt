package com.fauna.app.core

/**
 * The photo-backup **single-flight gate**: at most one pass runs at a time, and a
 * request that arrives while one is running is remembered so the finishing pass
 * runs exactly once more. Same concept and names as apple's `PhotoBackupPassGate`
 * (`PhotoBackupEngine.swift` — its doc comment states the reasoning).
 *
 * **Why a gate and not the old `if (_isSyncing.value) return 0`.** That guard was
 * lossy twice over. It dropped the request: the running pass has already taken its
 * MediaStore snapshot, so a photo taken mid-pass is not in it, and refusing the
 * second call meant nothing picked that photo up until the next WorkManager slice —
 * or never, if the app was killed first (`ui/folders.md` § Photo backup promises
 * new photos go up by themselves). And it was not atomic: `_isSyncing` was set on
 * the line AFTER it was tested, so two coroutines could both read `false` and both
 * run. Here claim-and-set is one step under [lock], and every method is
 * non-suspending so the critical section is never held across a suspension point.
 */
class PhotoBackupPassGate {
    private val lock = Any()
    private var running = false
    private var wanted = false

    /**
     * Claim the right to run a pass. `false` ⇒ a pass is already in flight and has
     * now been asked to run again when it finishes, so the caller must return
     * rather than start a second one.
     */
    fun claim(): Boolean = synchronized(lock) {
        if (running) {
            wanted = true
            return false
        }
        running = true
        wanted = false
        true
    }

    /**
     * Finish a pass. `true` ⇒ a request arrived while this pass ran and THIS caller
     * owns the trailing pass, so `running` deliberately stays set — the gate is
     * never open between a pass and its re-run. Consuming the flag here is what
     * collapses a burst of N mid-pass requests into exactly one trailing pass.
     */
    fun finish(): Boolean = synchronized(lock) {
        if (wanted) {
            wanted = false
            return true
        }
        running = false
        false
    }

    /**
     * Open the gate unconditionally, discarding any remembered request — the
     * throwing / cancelled path. A pass that failed would almost certainly fail
     * again at once, and nothing is lost by not re-running: an item whose ingest
     * threw has no synced ledger row, so the next trigger of any kind re-attempts it.
     */
    fun release() = synchronized(lock) {
        running = false
        wanted = false
    }

    /** Whether a pass is in flight, for tests and diagnostics only — never as a gate. */
    val isRunning: Boolean get() = synchronized(lock) { running }
}
