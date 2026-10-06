// What a successful post submit clears — exactly what it SENT, and nothing the
// user typed or picked after the click (feed.md § User actions, the
// `post-submit-button` row).
//
// Why a rule is needed at all: "clear the composer on success" has two readings
// that agree only while nobody touches the composer mid-submit — the composer the
// user pressed Post on, or the composer on screen when the submit resolves. On web
// those are far apart: the submit spans a provenance read, a thumbnail, two blob
// uploads, `fauna.posts.create` and the post-submit reload, and a reload the page
// started later can put the new post on screen while this submit still waits on its
// own superseded fetch. A user who saw their post land and began the next one lost
// it — text, tags and picked file — the moment the first submit resolved, with no
// error anywhere. That is `test_feed.py`'s second compose in a journey sitting
// disabled for the whole 90 s ceiling.
//
// The rule, per field: a field still holding the value that was sent IS the sent
// value, so it clears; a field the user changed since the click is new input, so
// it stays. Compared with `Object.is` — strings by value, a picked `File` and its
// bytes by reference, so any pick made after the click counts as new even when it
// carries the same name.
//
// One group of fields is NOT symmetric with the rest (owner ruling,
// mirrored from `FeedComposeState::clear_sent`,
// `libs/fauna-feed/src/compose.rs`): the post's audience — gate tier, teaser,
// room and sale — is STICKY. It clears only when the composer is otherwise
// untouched (its `contentKeys` still hold what was sent); if the user has
// already started the next post, the audience they picked stays instead of
// silently widening to Public. Pass `sticky` from a call site that has such a
// group; omit it and every field clears by the plain per-field rule above.

/** The composer after a successful submit: each field of `live` that still equals
 *  its `sent` counterpart is reset to `empty`'s; every other field is kept as the
 *  user left it. Pure — returns a fresh object and mutates none of its inputs.
 *
 *  `sticky`, when given, exempts `stickyKeys` from clearing unless every one of
 *  `contentKeys` still equals what was sent — see the module doc above. */
export function clearSent<T extends Record<string, unknown>>(
  live: T,
  sent: T,
  empty: T,
  sticky?: { stickyKeys: (keyof T)[]; contentKeys: (keyof T)[] },
): T {
  const next = { ...live };
  const untouched = !sticky || sticky.contentKeys.every((key) => Object.is(live[key], sent[key]));
  for (const key of Object.keys(sent) as (keyof T)[]) {
    if (!untouched && sticky!.stickyKeys.includes(key)) continue;
    if (Object.is(live[key], sent[key])) next[key] = empty[key];
  }
  return next;
}
