/** The token id behind each painted `session-card`, in order — `null` before
 *  the Sessions page's first hydrate and after it unmounts. The one thing a
 *  test cannot read off the page: which nest session each card is. Published
 *  as the `settings.session_token_ids` state key (`#lib/e2e-automation` →
 *  the web bridge's `getState`), the twin of linux's
 *  `settings::sessions::painted_token_ids_json` and tui's `state_json`.
 *  Memory only; the page writes it, nothing else reads it in production. */
let painted: string[] | null = null;

export function setPaintedSessionTokenIds(ids: string[] | null): void {
  painted = ids;
}

export function paintedSessionTokenIds(): string[] | null {
  return painted;
}
