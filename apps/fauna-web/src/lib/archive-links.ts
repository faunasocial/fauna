// Phase two's login wiring on web (`archive-import.md` § Linking and merging,
// rulings 2, 4 and 8) — tui's `archive_glue.rs` + `archive_links_tick`, over
// the core chunk's `WasmArchiveLinks` (`libs/fauna-wasm/src/archive_import.rs`).
//
// `wireArchiveLinks` registers the account's links as the conversations
// manager's archive-signal sink (`#lib/rpc`'s `registerArchiveLinks`), drives
// them once, and starts a one-second tick that drives again whenever the sink
// left work owed (`workDue`) and re-reads the views whenever it captured a
// change (`captures`) — both atomic loads, so the folders are read only then.
// The Contacts page paints `archiveLinkViews`; the archive-import page attaches
// `archiveLinksHandle()` to its machine before a Start or Resume, so a run that
// completes runs the announce round itself. Nothing here computes a state or a
// word: views and composed text come from the shared crate.

import { writable, type Writable } from 'svelte/store';
import type { WasmArchiveLinks } from '../../static/fauna_wasm.js';
import { registerActorScopedReset } from './actorScope';
import type { ArchiveLinkView } from './archive-link-rows';
import { resolveComposed, type ComposedText } from './i18n/localized';
import { registerArchiveLinks } from './rpc';
import { logMessage, wasmCoreModule } from './wasm';
import type { WasmConversationsManager } from '../../static/fauna_wasm.js';

const TICK_MS = 1000;
const LOG = 'fauna_web::archive_links';

/** The pairs' views (`ArchiveLinks::link_views`), re-read when the sink moved. */
export const archiveLinkViews: Writable<ArchiveLinkView[]> = writable([]);

let links: WasmArchiveLinks | null = null;
let tick: ReturnType<typeof setInterval> | null = null;
let driving = false;
let seenCaptures = -1;

/** The account's links, or `null` before the login wiring ran (the import
 *  machine then runs without them, as on a build with no phase two). */
export function archiveLinksHandle(): WasmArchiveLinks | null {
  return links;
}

/** Register, drive once, start the tick. Called once per conversations
 *  manager build, at the later of the build and the account runtime's start
 *  (the archive folders' keys rest in the runtime). */
export async function wireArchiveLinks(
  secretHex: string,
  manager: WasmConversationsManager,
  deviceIdHex: string,
): Promise<void> {
  stopArchiveLinks();
  const built = await registerArchiveLinks(secretHex, manager, deviceIdHex);
  links = built;
  await drive(built);
  await refreshArchiveLinkViews();
  tick = setInterval(() => {
    if (links !== built) return;
    if (built.workDue()) void drive(built);
    if (built.captures() !== seenCaptures) void refreshArchiveLinkViews();
  }, TICK_MS);
}

/** One drive pass; a pass in flight absorbs the request (the next tick
 *  re-checks `workDue`). */
async function drive(l: WasmArchiveLinks): Promise<void> {
  if (driving) return;
  driving = true;
  try {
    await l.drive();
  } catch (e) {
    logMessage('warn', LOG, `drive: ${e}`);
  } finally {
    driving = false;
  }
  if (links === l) void refreshArchiveLinkViews();
}

/** Re-read the views — the Contacts page calls it with its roster read. */
export async function refreshArchiveLinkViews(): Promise<void> {
  const l = links;
  if (!l) return;
  const captures = l.captures();
  try {
    const views = (await l.linkViews()) as ArchiveLinkView[];
    if (links !== l) return;
    seenCaptures = captures;
    archiveLinkViews.set(views);
  } catch (e) {
    logMessage('warn', LOG, `views: ${e}`);
  }
}

/** `contact-archive-link-yes` / `-no`. Rejects with the reason for the page's
 *  error line. */
export async function answerArchiveLink(view: ArchiveLinkView, yes: boolean): Promise<void> {
  const l = links;
  if (!l) return;
  await l.answerLink(view.contact, view.platform, yes);
  await refreshArchiveLinkViews();
}

/** `contact-archive-unlink`. */
export async function unlinkArchive(view: ArchiveLinkView): Promise<void> {
  const l = links;
  if (!l) return;
  await l.unlink(view.contact, view.platform);
  await refreshArchiveLinkViews();
}

/** `contact-archive-link-prompt`'s words — `text::link_prompt`. */
export function archiveLinkPrompt(view: ArchiveLinkView, contactName: string): string {
  const ct = JSON.parse(wasmCoreModule().archiveLinkPromptJson(view, contactName)) as ComposedText;
  return resolveComposed(ct);
}

/** `contact-archive-linked`'s words — `text::linked_label`. */
export function archiveLinkedLabel(view: ArchiveLinkView): string {
  const ct = JSON.parse(wasmCoreModule().archiveLinkedLabelJson(view)) as ComposedText;
  return resolveComposed(ct);
}

/** The departing account's links: stop the tick and drop the handle (the
 *  backend it was registered on goes with the manager). */
export function stopArchiveLinks(): void {
  if (tick !== null) clearInterval(tick);
  tick = null;
  links = null;
  seenCaptures = -1;
  archiveLinkViews.set([]);
}

registerActorScopedReset(stopArchiveLinks);
