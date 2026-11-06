/** The profile the page is showing — `{actor_id, home_nest_url}`, `null` while
 *  no Profile page is mounted. The actor-home slot the follow and the knock
 *  read is the one thing a test cannot see on the page; it is published as the
 *  `profile` state key (`#lib/e2e-automation` → the web bridge's `getState`),
 *  the twin of tui's `profile.home_nest_url`. Memory only; the page writes it,
 *  nothing else reads it in production. */
export interface ViewedProfile {
  actor_id: string;
  /** The page's actor-home slot (`null` = this nest). */
  home_nest_url: string | null;
}

let viewed: ViewedProfile | null = null;

export function setViewedProfile(v: ViewedProfile | null): void {
  viewed = v;
}

export function viewedProfile(): ViewedProfile | null {
  return viewed;
}
