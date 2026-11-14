// The memo of a two-stage singleton build's LOCAL half — web's conversations
// build (`account-client-lifecycle.md` § The client-side lifecycle → *The
// trigger fired*, decision (k)). Stage one (the manager's construction and the
// account runtime's start) needs no socket; stage two opens with the
// connection wait. Stage one is memoized apart from the build's own memo
// because it outlives a build that fails AT the wait: the next attempt waits
// again over the same manager and the same runtime, so no second engine is
// built in the tab. A build that fails AFTER the wait retires it, and so does
// stage one's own failure.
//
// Kept apart from `#lib/conversations` so the keep/retire rule is tested on
// its own (`stage-one.test.ts`); the conversations build is its one user.

/** One account's stage one: the promise of its product, and the product once
 *  it has landed. */
export interface StageOneEntry<T> {
  readonly key: string;
  readonly promise: Promise<T>;
  built: T | null;
}

export class StageOneSlot<T> {
  #entry: StageOneEntry<T> | null = null;

  /** The entry for `key` when one stands, else a fresh one running `run`.
   *  `run` is handed its own entry, to check that it is still the slot's
   *  before it records its product (`landed`). An entry whose run rejects drops
   *  itself, so the next attempt builds anew. */
  enter(key: string, run: (entry: StageOneEntry<T>) => Promise<T>): StageOneEntry<T> {
    if (this.#entry && this.#entry.key === key) return this.#entry;
    const entry: { key: string; promise: Promise<T>; built: T | null } = {
      key,
      promise: Promise.resolve(null as unknown as T),
      built: null,
    };
    this.#entry = entry;
    entry.promise = run(entry).catch((e: unknown) => {
      if (this.#entry === entry) this.#entry = null;
      throw e;
    });
    // Every attempt awaiting the entry handles its rejection; this keeps an
    // entry no attempt awaits any more from reporting an unhandled one.
    entry.promise.catch(() => {});
    return entry;
  }

  /** Is `entry` still the slot's? A run checks it before recording its
   *  product, so a run the slot dropped never installs. */
  holds(entry: StageOneEntry<T>): boolean {
    return this.#entry === entry;
  }

  /** The landed product, or `null` while stage one is building or none is. */
  built(): T | null {
    return this.#entry?.built ?? null;
  }

  /** Retire `entry` if it is still the slot's — a build that failed after the
   *  connection wait. */
  retire(entry: StageOneEntry<T> | null): void {
    if (entry !== null && this.#entry === entry) this.#entry = null;
  }

  /** Drop whatever stands — the actor-scoped reset. */
  clear(): void {
    this.#entry = null;
  }
}
