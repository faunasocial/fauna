/** The Profile route for `actorId`, carrying the actor's home when the door
 *  knows one (`profile.md` § Layout & flow → *Reaching another actor's profile
 *  with their home carried*): the Find User result's lookup nest URL, a post's
 *  `home_nest_url`, a contact row's `home_nest_url`. The home rides as `?home=`;
 *  the page seeds its actor-home slot from it. Blank or absent = no hint. */
export function profileHref(actorId: string, home?: string | null): string {
  const hint = home?.trim();
  return `/app/profile/${actorId}${hint ? `?home=${encodeURIComponent(hint)}` : ''}`;
}
