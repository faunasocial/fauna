// The signed-in session's ONE `DevicesMachine`, shared by the Devices and
// Folders sections — the shape every other app already has (linux builds one
// over both sub-pages, tui one per sign-in, FaunaKit / android / windows one
// per session VM).
//
// ⚠ Why this is not built per mount any more: a section used to construct a
// fresh machine (and with it a fresh followed-folders source) on every visit.
// That machine's memory is load-bearing — the followed source answers a read
// that cannot reach the nest with the LAST rows it read (`ui/folders.md`
// § Following a public folder: a network fault leaves every followed row, and
// its last verdict, standing), and a machine born inside the gap has no last
// rows, so a revisit during a dropped connection emptied the followed list.
// It also reset the refresh barrier (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`)
// under any baseline taken on the previous visit.
//
// Each section keeps its own mount-scoped work (its timers, push handler,
// first `refresh()`); only the machine and its one-time wiring live here.

import { identity } from '$lib/store';
import { nodeUrl } from '$lib/api';
import { sharedRpcPort } from '$lib/rpc';
import { sharedAccountPort } from '$lib/account-runtime';
import { createDevicesMachine, setFollowedFoldersSource } from '$lib/wasm-folders';
import { conversationsManagerIfReady } from '$lib/conversations';
import type { DevicesMachine } from '../../static/fauna_wasm_folders.js';

interface Session {
  key: string;
  machine: Promise<DevicesMachine>;
  built: DevicesMachine | null;
  listeners: Set<() => void>;
}

let session: Session | null = null;

// A sign-out (or a switch to another actor) retires the machine with the
// identity it was wired for, so nothing reads the outgoing actor's rows.
identity.subscribe((id) => {
  if (!session) return;
  if (!id || sessionKey(id.secretHex) !== session.key) session = null;
});

// Keyed on the secret itself (in memory only), not the derived actor id: the
// identity subscription above can fire before the wasm that derives it loads.
function sessionKey(secretHex: string): string {
  return `${nodeUrl()}|${secretHex}`;
}

async function build(secretHex: string, s: Session): Promise<DevicesMachine> {
  // Over the SPA singleton's socket (`sharedRpcPort`): the machine's gestures
  // ride the one connection the `connection-status` indicator reports on, so
  // a Folders gesture the moment the app reads online finds a socket that is.
  const machine = await createDevicesMachine(
    { onChanged: () => s.listeners.forEach((l) => l()) },
    await sharedRpcPort(secretHex),
    secretHex,
  );
  // Every seam below MUST be wired before the first refresh.
  // B3 join-filter (folders.md § Sharing, Member list-visibility): the nest
  // returns every ROSTERED member set, but a stranger's knock rosters you
  // before you accept, so the machine drops every `role == "member"` row this
  // client has not actually MLS-joined. The engine lives in the OTHER wasm
  // bundle, so the callback bridges to it; while that bundle is still loading
  // it answers false — the fail-safe direction (hide, never surface unbidden),
  // and the sections' 15 s refresh re-asks.
  machine.setMlsQuery({
    isJoinedSharedSet: (groupIdHex: string) =>
      conversationsManagerIfReady()?.foldersIsJoined(groupIdHex) ?? false,
  });
  // Foreign-set (cross-nest) list source (folders.md § Implementation status
  // today). Unwired, a set shared from ANOTHER nest has no row.
  machine.setForeignSetsSource(secretHex);
  // Label custody: unwired, a sealed device/folder label degrades to its id-shaped
  // fallback instead of the name the owner set.
  machine.setLabelCustody(secretHex);
  // The audience attestor — the owner's signature a `public` flip carries
  // (encryption-at-rest.md § Readable classes → the declassification is
  // owner-attested). Unwired, the flip lands but every verifying seat keeps
  // the folder sealed.
  machine.setAudienceAttestor(secretHex);
  // The fleet door, through the account port (account-client-lifecycle.md
  // § The account port): a removal stages its fleet leg in this tab's account
  // runtime BEFORE the nest deletion and settles it on the outcome — the
  // native removal (account-data-taxonomy.md § Fleet-scope reclamation, clause
  // (4)). With no runtime serving this account the removal is refused and
  // deletes nothing; `sharedRpcPort` above has loaded the core chunk.
  machine.setAccountPort(sharedAccountPort(secretHex));
  // Followed public folders (`ui/folders.md` § Following a public folder): a
  // follow lives entirely in the account's `fauna.state.follows` rows, read
  // across the same account port.
  setFollowedFoldersSource(machine, secretHex);
  s.built = machine;
  return machine;
}

/**
 * The session's machine, built and wired on first use, plus `listen(onChanged)`
 * — the section's observer, until the returned unsubscribe runs (call it from
 * `onDestroy`). Two sections mounting at once share one build.
 */
export async function sessionDevicesMachine(
  secretHex: string,
  onChanged: () => void,
): Promise<{ machine: DevicesMachine; unlisten: () => void }> {
  const key = sessionKey(secretHex);
  if (!session || session.key !== key) {
    const s = { key, built: null, listeners: new Set() } as unknown as Session;
    s.machine = build(secretHex, s);
    // A failed build must not stick: the next mount retries.
    s.machine.catch(() => {
      if (session === s) session = null;
    });
    session = s;
  }
  const s = session;
  s.listeners.add(onChanged);
  try {
    return { machine: await s.machine, unlisten: () => s.listeners.delete(onChanged) };
  } catch (e) {
    s.listeners.delete(onChanged);
    throw e;
  }
}

/**
 * The refresh barrier's triple (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`) —
 * the zero triple before a machine exists (the legitimate "none yet"), never
 * an absent key. Counting and shape are shared Rust (`refreshesJson`).
 */
export function devicesRefreshes(): { started: number; completed: number; committed_gen: number } {
  const built = session?.built;
  if (!built) return { started: 0, completed: 0, committed_gen: 0 };
  return JSON.parse(built.refreshesJson());
}
