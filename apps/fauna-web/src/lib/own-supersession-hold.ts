// Holds back a supersession this device's OWN stolen-identity ceremony caused
// (`docs/goal/ui/settings.md` § Recovery kit → *The persist-failure message
// survives the page*, its closing rule): while the ceremony runs, or its
// persist-failure message is parked, the escalation to the launch surface is
// owed rather than performed, and it is performed once the user leaves
// Account — or at once, when the ceremony ends while the user is elsewhere and
// nothing is parked.
//
// Why web needs it. The ceremony supersedes the identity this session holds,
// so the session's next bearer re-mint (or background silent refresh) is
// refused as superseded the instant the nest commits — typically before the
// ceremony's own result has been handled. `$lib/post-auth-escalation` answers
// that refusal with a full document navigation, which unloads the page, the
// ceremony's pending promise and the only surviving copy of the successor's
// key with it.
//
// tui is the model (`App::defer_own_supersession` /
// `escalate_deferred_supersession` / `forget_deferred_supersession` /
// `stolen_outcome_on_screen`, `apps/fauna-tui/src/app.rs`); macOS/iOS share
// FaunaKit's `StolenCeremonyHold`. A pure decision object, as
// `RecoveryErrorGuard` is, so it is unit-testable under `just web-unit-test`
// (`own-supersession-hold.test.ts`). Module-level — not page state — because
// the escalation is decided in `$lib`, far from the Settings page that owns
// the ceremony; one document runs at most one ceremony.

export class OwnSupersessionHold {
  /** The ceremony this device started is still running. */
  #ceremonyInFlight = false;
  /** The ceremony ended without adopting a successor while the user was on
   *  Account, so its message is what the screen shows. A refusal the ceremony
   *  caused has no fixed order against the ceremony's result (tui measured the
   *  launch machine's landing ~30 ms after the fold), so it extends the hold
   *  past the fold until the user leaves Account. */
  #outcomeOnScreen = false;
  /** The ceremony's persist-failure message is parked (`RecoveryErrorGuard`). */
  #parked = false;
  /** A supersession arrived while held: its escalation is owed. */
  #owed = false;

  /** Would a supersession arriving now be this device's own ceremony's? */
  get ownsSupersession(): boolean {
    return this.#ceremonyInFlight || this.#outcomeOnScreen || this.#parked;
  }

  /**
   * The superseded arm's question: hold this supersession back? Records the
   * escalation as owed and returns `true` when the ceremony owns it; `false`
   * means escalate as usual.
   */
  defer(): boolean {
    const owned = this.ownsSupersession;
    if (owned) this.#owed = true;
    return owned;
  }

  /** The `identity-stolen-button` ceremony was dispatched. */
  ceremonyStarted(): void {
    this.#ceremonyInFlight = true;
  }

  /**
   * The ceremony ended WITHOUT adopting a successor (an unlanded arm, the
   * persist-failure arm, or a thrown error). `onAccount`: the user is on the
   * Account sub-page now; `parked`: the ceremony's message was parked.
   * Returns `true` when the owed escalation must be performed NOW — the user
   * is elsewhere and nothing parked needs the page.
   */
  ceremonyEnded({ onAccount, parked }: { onAccount: boolean; parked: boolean }): boolean {
    this.#ceremonyInFlight = false;
    this.#parked = parked;
    if (onAccount) this.#outcomeOnScreen = true;
    if (!onAccount && !parked) return this.#take();
    return false;
  }

  /**
   * The ceremony adopted its successor and switched to it: the switch is
   * itself the full relaunch, so an owed escalation is spent, not performed.
   */
  adopted(): void {
    this.#ceremonyInFlight = false;
    this.#outcomeOnScreen = false;
    this.#parked = false;
    this.#owed = false;
  }

  /**
   * The nav edge away from Account (the same edge that discharges the parked
   * message): the user has had the whole visit to read or copy the key.
   * Returns `true` when an owed escalation must be performed now.
   */
  leftAccount(): boolean {
    this.#outcomeOnScreen = false;
    this.#parked = false;
    // A ceremony still running keeps owning its supersession wherever the
    // user is: its own end decides.
    if (this.#ceremonyInFlight) return false;
    return this.#take();
  }

  /** Test seam for the e2e agent's per-test reset (module state outlives a
   *  soft reset, exactly as the escalation latch does). */
  reset(): void {
    this.adopted();
  }

  #take(): boolean {
    const owed = this.#owed;
    this.#owed = false;
    return owed;
  }
}

/** The one hold the document's escalation path and its Settings page share. */
export const ownSupersessionHold = new OwnSupersessionHold();
