// The private contact overlay in the SPA (`docs/goal/ui/contacts.md` § The
// private overlay; the edit surface `docs/goal/ui/profile.md` § The private
// section) — the glue between the pages and the wasm face
// (`libs/fauna-wasm/src/contact_overlays.rs`). Every name, label line, filter
// match and refusal is the face's; this module only picks which face a tab
// reads and turns a Save's answer into what the page shows; the "names may
// have moved" revision is `#lib/conversations`'s `contactsRevision`, set at
// every conversations re-read.
//
// A tab that hosts no runtime (it lost the MLS-writing role to another tab of
// the account — `#lib/conversations`'s `engineRoleDenied`) builds no manager,
// so it reads the runtime-less face: every name is the public label, and its
// Save is refused on `error-message`, never silently
// (`account-client-lifecycle.md` § The client-side lifecycle → *Bounds,
// stated*).

import { resolveLocalized, type LocalizedText } from './i18n/localized.ts';

/** `fauna_core::format::PeerLabel`: the one line every surface shows, and the
 *  public name it replaced when a nickname supplied it (`null` otherwise —
 *  the secondary line is then absent). */
export interface PeerLabel {
  primary: string;
  public: string | null;
}

/** The private section's fields. */
export interface OverlayForm {
  nickname: string;
  notes: string;
  labels: string[];
}

/** What a Save answered; every kind but `saved` carries its finished text. */
export interface SaveAnswer {
  kind: 'saved' | 'refused' | 'notReady' | 'failed';
  text: LocalizedText | null;
}

/** The private section's staging for one person (`WasmOverlayEditor`). */
export interface OverlayEditorFace {
  form(): OverlayForm;
  isStaged(): boolean;
  setNickname(value: string): void;
  setNotes(value: string): void;
  addLabel(raw: string): LocalizedText | undefined;
  removeLabel(index: number): void;
  reset(): void;
  save(): Promise<SaveAnswer>;
}

/** The projection's reads (`WasmContactOverlays`). */
export interface OverlayFace {
  revision(): number;
  peerLabel(
    displayName: string | null | undefined,
    handle: string | null | undefined,
    actorId: string,
  ): PeerLabel;
  labelsLine(actorId: string): string | undefined;
  matchesFilter(
    query: string,
    handle: string | null | undefined,
    domain: string | null | undefined,
    actorId: string,
  ): boolean;
  subscriptionAuthorLabel(handle: string | null | undefined, authorId: Uint8Array): string;
  /** The completion under the label add field: vocabulary labels `typed`
   *  starts, minus the `staged` ones (`label_suggestions` in shared Rust). */
  labelSuggestions(typed: string, staged: string[]): string[];
  editor(actorId: string): OverlayEditorFace;
}

/** One face per manager — or the runtime-less one while the tab has none —
 *  built on first read and reused, so a page re-reading names on every
 *  revision does not mint a wasm object per read. */
export function overlayFaces<M>(
  build: (manager: M) => OverlayFace,
  withoutRuntime: () => OverlayFace,
): (manager: M | null) => OverlayFace {
  let held: { manager: M | null; face: OverlayFace } | null = null;
  return (manager) => {
    if (held === null || held.manager !== manager) {
      held = { manager, face: manager === null ? withoutRuntime() : build(manager) };
    }
    return held.face;
  };
}

/** The Save, as the page shows it: `null` once it saved (the staging is
 *  dropped and the fields read live again), else the error line — never an
 *  empty one, so a refusal is always visible. The staged edits stay on any
 *  answer but `saved`. */
export async function savePrivateSection(editor: OverlayEditorFace): Promise<string | null> {
  let answer: SaveAnswer;
  try {
    answer = await editor.save();
  } catch (e) {
    return resolveLocalized({
      key: 'profile.private_save_failed',
      args: { reason: String(e) },
    });
  }
  if (answer.kind === 'saved') return null;
  return (
    resolveLocalized(answer.text) ||
    resolveLocalized({ key: 'profile.private_save_failed', args: { reason: answer.kind } })
  );
}
