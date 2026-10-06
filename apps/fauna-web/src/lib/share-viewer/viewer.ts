// The private share link viewer's controller (share-links.md § The
// private-file extension → *The viewer is a browser page and needs no
// account*). It is the whole flow of the page, with the page's three outside
// powers injected — the shared wasm (`libs/fauna-wasm-share`, every decision
// and every string), the address, and a same-origin byte fetcher — so the
// flow is unit-tested without a browser (`viewer.test.ts`). The DOM half is
// `main.ts`.
//
// This module, and everything the viewer page imports, stays outside the app
// shell: no identity store, no account runtime, no socket, no storage (rule 3;
// `share-viewer-contract.test.ts` pins it). It reads the address and never
// writes it (rule 4).

/** The page's fixed text (wasm `viewerText()`). */
export interface ViewerText {
  readonly title: string;
  readonly genericBody: string;
  readonly loading: string;
  readonly download: string;
  readonly keepNote: string;
}

/** An opened link (wasm `OpenedLink`). */
export interface OpenedLink {
  readonly filename: string;
  readonly sizeText: string;
  readonly chunkCount: number;
  readonly contentType: string;
  readonly preview: string;
  /** The verified file; throws the sentence to show on any mismatch. */
  assemble(chunks: Uint8Array[]): Uint8Array;
}

/** The shared wasm surface the page calls (`fauna_wasm_share.js`). */
export interface ShareWasm {
  viewerStart(pathname: string, hash: string): { token: string; fragment: string } | undefined;
  manifestPath(token: string): string;
  chunkPath(token: string, index: number): string;
  statusText(status: number): string;
  /** Throws the sentence to show when the link does not open. */
  openShare(token: string, fragment: string, manifest: Uint8Array): OpenedLink;
  viewerText(): ViewerText;
}

/** What a fetch of one of the viewer's own paths answered. */
export type Fetched = { ok: true; bytes: Uint8Array } | { ok: false; status: number };

/** Fetch a same-origin path. Never handed anything but a path the wasm built. */
export type FetchBytes = (path: string) => Promise<Fetched>;

/** Rule 2's preview classes — the wasm decides which one a file gets. */
export type PreviewKind = 'image' | 'audio' | 'video' | 'text' | 'none';

export type ViewerState =
  | { kind: 'generic' }
  | { kind: 'loading' }
  | { kind: 'failed'; message: string }
  | {
      kind: 'ready';
      filename: string;
      sizeText: string;
      contentType: string;
      preview: PreviewKind;
      bytes: Uint8Array;
    };

const PREVIEW_KINDS: readonly PreviewKind[] = ['image', 'audio', 'video', 'text', 'none'];

/** Anything the wasm calls a preview kind that this page does not know is
 *  download-only. */
export function previewKind(name: string): PreviewKind {
  return (PREVIEW_KINDS as readonly string[]).includes(name) ? (name as PreviewKind) : 'none';
}

/** A thrown wasm error is the sentence to show; anything else is not, and the
 *  page says the link could not be opened in the wasm's words instead. */
function sentence(e: unknown, fallback: string): string {
  return typeof e === 'string' && e.length > 0 ? e : fallback;
}

/**
 * Run the viewer: read the address, fetch the manifest and every chunk, open
 * and verify through the wasm, and report each state. One pass, no retry
 * loop — a refusal is said plainly and the page stops.
 */
export async function runViewer(
  wasm: ShareWasm,
  address: { pathname: string; hash: string },
  fetchBytes: FetchBytes,
  onState: (state: ViewerState) => void,
): Promise<void> {
  const link = wasm.viewerStart(address.pathname, address.hash);
  if (!link) {
    onState({ kind: 'generic' });
    return;
  }
  onState({ kind: 'loading' });
  const unavailable = wasm.statusText(0);
  try {
    const manifest = await fetchBytes(wasm.manifestPath(link.token));
    if (!manifest.ok) {
      onState({ kind: 'failed', message: wasm.statusText(manifest.status) });
      return;
    }
    let opened: OpenedLink;
    try {
      opened = wasm.openShare(link.token, link.fragment, manifest.bytes);
    } catch (e) {
      onState({ kind: 'failed', message: sentence(e, unavailable) });
      return;
    }
    const chunks: Uint8Array[] = [];
    for (let i = 0; i < opened.chunkCount; i++) {
      const chunk = await fetchBytes(wasm.chunkPath(link.token, i));
      if (!chunk.ok) {
        onState({ kind: 'failed', message: wasm.statusText(chunk.status) });
        return;
      }
      chunks.push(chunk.bytes);
    }
    let bytes: Uint8Array;
    try {
      bytes = opened.assemble(chunks);
    } catch (e) {
      onState({ kind: 'failed', message: sentence(e, unavailable) });
      return;
    }
    onState({
      kind: 'ready',
      filename: opened.filename,
      sizeText: opened.sizeText,
      contentType: opened.contentType,
      preview: previewKind(opened.preview),
      bytes,
    });
  } catch {
    // A network failure (no answer at all): the plain "try again later".
    onState({ kind: 'failed', message: unavailable });
  }
}

/**
 * The page's one fetcher: a same-origin GET of a path, sending no cookie and
 * no referrer, cached nowhere. Refuses anything that is not a plain absolute
 * path on this origin — the wasm builds every path the page fetches, and none
 * carries the fragment (rule 1).
 */
export function sameOriginFetcher(fetchImpl: typeof fetch): FetchBytes {
  return async (path: string) => {
    if (!path.startsWith('/') || path.startsWith('//') || path.includes('#')) {
      throw new Error('the share viewer fetches only its own same-origin paths');
    }
    const resp = await fetchImpl(path, {
      credentials: 'omit',
      referrerPolicy: 'no-referrer',
      cache: 'no-store',
      redirect: 'error',
    });
    if (!resp.ok) return { ok: false, status: resp.status };
    return { ok: true, bytes: new Uint8Array(await resp.arrayBuffer()) };
  };
}
