// The private share link viewer page's entry (`share-viewer/share-viewer.html`,
// the SPA build's second entry — built outside the app shell's root layout so
// nothing of the app runs here). It loads the share wasm chunk alone, runs
// `runViewer` against the address bar, and renders each state with
// `textContent` and object URLs only: the decrypted file is never rendered as
// a document on this origin (rule 2), and the address is never written (rule 4).

import './share-viewer.css';
import init, * as share from '../../../static/fauna_wasm_share.js';
import { runViewer, sameOriginFetcher, type ShareWasm, type ViewerState } from './viewer';

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  return node;
}

/** The object URLs this page made — released when the page goes. */
const objectUrls: string[] = [];

function objectUrl(bytes: Uint8Array, type: string): string {
  const url = URL.createObjectURL(new Blob([bytes as BlobPart], { type }));
  objectUrls.push(url);
  return url;
}

function preview(state: Extract<ViewerState, { kind: 'ready' }>): HTMLElement | null {
  switch (state.preview) {
    case 'image': {
      const img = el('img');
      img.alt = state.filename;
      img.src = objectUrl(state.bytes, state.contentType);
      return img;
    }
    case 'audio': {
      const audio = el('audio');
      audio.controls = true;
      audio.src = objectUrl(state.bytes, state.contentType);
      return audio;
    }
    case 'video': {
      const video = el('video');
      video.controls = true;
      video.src = objectUrl(state.bytes, state.contentType);
      return video;
    }
    case 'text':
      // Text in a text element: the bytes are characters, never markup.
      return el('pre', new TextDecoder().decode(state.bytes));
    default:
      return null;
  }
}

function render(root: HTMLElement, text: ReturnType<ShareWasm['viewerText']>, state: ViewerState) {
  root.replaceChildren();
  root.dataset.state = state.kind;
  root.append(el('h1', text.title));
  switch (state.kind) {
    case 'generic':
      root.append(el('p', text.genericBody));
      return;
    case 'loading':
      root.append(el('p', text.loading));
      return;
    case 'failed': {
      const p = el('p', state.message);
      p.setAttribute('role', 'alert');
      root.append(p);
      return;
    }
    case 'ready': {
      root.append(el('h2', state.filename), el('p', state.sizeText));
      // The download is the reassembled bytes as an opaque file: a browser
      // saves it, never opens it here.
      const a = el('a', text.download);
      a.href = objectUrl(state.bytes, 'application/octet-stream');
      a.download = state.filename;
      a.className = 'download';
      root.append(a);
      const shown = preview(state);
      if (shown) {
        const figure = el('figure');
        figure.append(shown);
        root.append(figure);
      }
      root.append(el('p', text.keepNote));
      return;
    }
  }
}

async function main() {
  const root = document.getElementById('share-viewer');
  if (!root) return;
  // No argument: the glue's own `new URL('…_bg.wasm', import.meta.url)`,
  // which the viewer build emits as a hashed asset beside this script.
  await init();
  const wasm: ShareWasm = share;
  const text = wasm.viewerText();
  await runViewer(
    wasm,
    { pathname: location.pathname, hash: location.hash },
    sameOriginFetcher(fetch.bind(globalThis)),
    (state) => render(root, text, state),
  );
}

addEventListener('pagehide', () => objectUrls.forEach((u) => URL.revokeObjectURL(u)));
void main();
