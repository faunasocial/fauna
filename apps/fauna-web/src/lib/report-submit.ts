// The report sheet's submit sequence (`moderation.md` § User-initiated
// reporting → *What the reporter is told*; `account-offline-mutation.md`
// § Implementation status today, the knock-accept and abuse-report composers).
// This module stays wasm-free so its routing is testable under plain
// `deno test` (`report-submit.test.ts`); `ReportHost.svelte` passes the real
// rpc.ts calls and the shared words.

/** What a submit calls. `submitOrQueue` resolves the nest's reply, or `null`
 *  when the report was QUEUED with no nest; `block` is the ticked
 *  `knocksBlock(author)` (absent when unticked); `hide` is the reporter-side
 *  hide, answering the stored list; `blockNotSent` is the shared
 *  `reportBlockNotSent` line, worded. */
export interface ReportSubmitVerbs<R> {
  submitOrQueue: () => Promise<R | null>;
  block?: () => Promise<void>;
  hide: () => Promise<string[]>;
  blockNotSent: () => string;
}

export interface ReportSettled<R> {
  /** The reply when sent; `null` when queued (the sheet says `reportQueued`). */
  reply: R | null;
  /** Whether the ticked block went out. */
  blocked: boolean;
  /** The stored hidden list after the hide, when it landed. */
  hidden: string[] | null;
  /** A follow-up failure for `error-message`, BESIDE the acknowledgement. */
  followup: string | null;
}

function detail(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/**
 * Submit → the ticked block → the reporter-side hide (tui's `report.rs`
 * order). A failing submit rejects (the sheet stays open for a retry). A
 * queued report does not try the block — it needs the nest, so the follow-up
 * line says so — and still lands the hide, which is local.
 */
export async function submitReport<R>(verbs: ReportSubmitVerbs<R>): Promise<ReportSettled<R>> {
  const reply = await verbs.submitOrQueue();
  let followup: string | null = null;
  let blocked = false;
  if (verbs.block) {
    if (reply === null) {
      followup = verbs.blockNotSent();
    } else {
      try {
        await verbs.block();
        blocked = true;
      } catch (e) {
        followup = `block: ${detail(e)}`;
      }
    }
  }
  let hidden: string[] | null = null;
  try {
    hidden = await verbs.hide();
  } catch (e) {
    followup ??= `hide: ${detail(e)}`;
  }
  return { reply, blocked, hidden, followup };
}
