// What a `getClient` waiter does when, after its await, the WS-RPC singleton's
// slot belongs to another build (`rpc.ts::getClient`, the supersede arm).
//
// The singleton serves ONE actor at a time, the latest one asked for. A waiter
// superseded by a build for its OWN actor (a nest change, a retire-and-rebuild)
// rejoins: the current build is the one it wants. A waiter superseded by a
// build for ANOTHER actor is stale and must fail — re-entering `getClient` with
// its own actor would rebuild the singleton for it and supersede the build that
// superseded it, whose own waiter then does the same: two late calls for two
// actors ping-pong the singleton for ever, every turn retiring a client and
// running the canonical drop. Measured in web's append-mode "Add account"
// wizard (2026-11-02): the Settings page's member-review read resumed after a
// long await with the outgoing account's captured identity while the
// Conversations page's muted-words read dialled as the imported one — ~14,000
// rebuilds and a WebSocket exhaustion, from two calls.

/** The failure a superseded waiter for a no-longer-current actor gets. Its
 *  caller asked for a client the app has moved past; like any failed read, it
 *  keeps whatever it last showed. */
export class ClientSupersededError extends Error {
  constructor(wanted: string, current: string | null) {
    super(
      `WS-RPC client superseded: wanted actor ${wanted.slice(0, 8)}, ` +
        `the singleton now serves ${current ? current.slice(0, 8) : 'nobody'}`,
    );
    this.name = 'ClientSupersededError';
  }
}

/** `rejoin` — call `getClient` again (it joins or rebuilds the build for this
 *  actor at the nest URL it reads afresh); `refuse` — throw
 *  {@link ClientSupersededError}.
 *
 *  An EMPTY slot (`currentActor === null`: `retireRpcClient()` dropped the
 *  singleton, as the onboarding `LoggedIn` terminal and sign-out do) rejoins
 *  too — nobody holds it, so the first waiter to rejoin builds, and every
 *  later waiter for another actor then meets that build and is refused. */
export function supersededWaiterNext(
  wantedActor: string,
  currentActor: string | null,
): 'rejoin' | 'refuse' {
  return currentActor === null || currentActor === wantedActor ? 'rejoin' : 'refuse';
}
