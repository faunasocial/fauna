// The succession's closing act, as a pure decision — the successor's first
// authenticated session mints, registers, escrows and SHOWS a fresh RecoveryKey,
// unbidden (`identity-succession.md` § The RecoveryKey → *At succession*).
//
// The effects are injected rather than imported so this is unit-testable
// (`succession-kit.test.ts`); the Settings page supplies the real ones. What is
// worth testing here is not the mint — that is one wasm call — but the ORDER and
// the failure arm, which is where every app that has built this step so far has
// paid: a failed mint that spends the obligation leaves the account kitless with
// nothing anywhere remembering it is owed one, and the goal doc calls that
// strictly worse than never-created.

/** The effects `dischargeOwedKit` needs, in the order it uses them. */
export interface OwedKitPort<Minted> {
  /** Take the obligation, clearing it in the same breath. `false` on every
   *  ordinary sign-in — the common case, and the reason this is the first call:
   *  nothing else may happen until the claim is won. */
  claim: () => Promise<boolean>;
  /** Mint, register and escrow the fresh kit. Rejects like any round trip. */
  mint: () => Promise<Minted>;
  /** Show it. Called only on success, and before nothing else. */
  show: (minted: Minted) => void;
  /** Put the obligation back. ⚠ Called on EVERY mint failure. */
  rearm: () => Promise<void>;
  /** Surface a failed mint to the user; the kit is still owed either way. */
  onError: (message: string) => void;
}

/**
 * Run the closing act if — and only if — this session owes it.
 *
 * Returns whether a kit was shown, so a caller can log the outcome; every
 * decision the return value could drive is already made here.
 *
 * **The claim is take-and-clear, not clear-on-success.** Two racing renders of
 * the section must not both mint, and the second would race the first's own
 * reconnect. The failure arm is what makes that safe: a mint that does not reach
 * the screen re-arms, and re-arming cannot double-mint because `create_kit`
 * re-reads the chain head and picks its own arm — a second mint supersedes a
 * stranded first rather than colliding with it. The cost of a spurious re-arm is
 * one extra kit; the cost of a missed one is an account whose only route back to
 * a held kit is the 30-day seed-alone window, which would additionally fire the
 * pending-replacement critical alert on the user's own remediation.
 */
export async function dischargeOwedKit<Minted>(port: OwedKitPort<Minted>): Promise<boolean> {
  if (!(await port.claim())) return false;
  try {
    port.show(await port.mint());
    return true;
  } catch (e) {
    port.onError(e instanceof Error ? e.message : String(e));
    // Never inside a conditional: the obligation outranks every reason the mint
    // might have failed. An unreachable nest, a revoked bearer racing the
    // ceremony's own teardown, a malformed reply — each leaves the account
    // kitless, and each is recoverable only if something still remembers.
    await port.rearm();
    return false;
  }
}
