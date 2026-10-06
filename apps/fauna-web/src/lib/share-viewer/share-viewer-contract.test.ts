// The private share link viewer's four rules as SOURCE contracts
// (share-links.md § The private-file extension → *The viewer is a browser page
// and needs no account*; testing.md convention 17). They read the viewer's own
// files as text, so a later edit that quietly breaks a rule reds here even
// where no runtime test happens to exercise it. Witnesses for the catalog's
// `share-links` outcomes 23 (safe preview only), 24 (the key goes to no
// server), 25 (a signed-in owner is a stranger), 26 (the address bar is left
// alone) and 27 (an unfurler learns nothing).

import { stripComments } from '../source-contract.ts';

const DIR = new URL('./', import.meta.url);
const read = (rel: string, base: URL = DIR) => Deno.readTextFileSync(new URL(rel, base));

/** Every script the viewer page runs, comments stripped. */
const SCRIPTS = ['viewer.ts', 'main.ts'].map((f) => [f, stripComments(read(f))] as const);
const HTML = read('../../../share-viewer/share-viewer.html');
const VITE = read('../../../vite.share-viewer.config.ts');

function fail(msg: string): never {
  throw new Error(msg);
}

Deno.test('rule 3: the viewer imports nothing of the app — no identity, account, store or socket', () => {
  const allowed = new Set(['./viewer', './viewer.ts', './share-viewer.css', '../../../static/fauna_wasm_share.js']);
  for (const [file, src] of SCRIPTS) {
    const specs = [...src.matchAll(/\bfrom\s+['"]([^'"]+)['"]|\bimport\s+['"]([^'"]+)['"]/g)].map(
      (m) => m[1] ?? m[2],
    );
    for (const spec of specs) {
      if (!allowed.has(spec)) {
        fail(
          `${file} imports ${spec}: the viewer page loads only its controller, its ` +
            'stylesheet and the share wasm chunk — anything of the app shell could ' +
            'touch an identity slot or account state (rule 3).',
        );
      }
    }
    if (/\bimport\s*\(/.test(src)) fail(`${file} has a dynamic import(): the viewer's graph is fixed`);
  }
});

Deno.test('rule 3: no storage of any kind — a signed-in owner is treated like a stranger', () => {
  for (const [file, src] of SCRIPTS) {
    for (const banned of ['localStorage', 'sessionStorage', 'indexedDB', 'document.cookie', 'caches.', 'BroadcastChannel']) {
      if (src.includes(banned)) fail(`${file} touches ${banned}`);
    }
  }
});

Deno.test('rule 4: the viewer never writes the address — the link is the only key there is', () => {
  for (const [file, src] of SCRIPTS) {
    for (const banned of [
      'history.',
      'replaceState',
      'pushState',
      'location.replace',
      'location.assign',
      'location.reload',
    ]) {
      if (src.includes(banned)) fail(`${file} uses ${banned}`);
    }
    if (/location(\.\w+)?\s*=[^=]/.test(src)) fail(`${file} assigns to location`);
  }
});

Deno.test('rule 1: one fetch, of same-origin paths, and no other way out of the page', () => {
  for (const [file, src] of SCRIPTS) {
    for (const banned of ['XMLHttpRequest', 'WebSocket', 'EventSource', 'sendBeacon', 'window.open', 'postMessage']) {
      if (src.includes(banned)) fail(`${file} uses ${banned}`);
    }
    if (/https?:\/\//.test(src)) fail(`${file} names an absolute URL`);
  }
  const viewer = SCRIPTS.find(([f]) => f === 'viewer.ts')![1];
  const main = SCRIPTS.find(([f]) => f === 'main.ts')![1];
  // The only network call is the guarded fetcher's; main hands it the global.
  if ((viewer.match(/\bfetchImpl\(/g) ?? []).length !== 1) fail('viewer.ts must call fetch exactly once, in sameOriginFetcher');
  if (/\bfetch\(/.test(viewer)) fail('viewer.ts calls the global fetch directly');
  if (/\bfetch\(/.test(main)) fail('main.ts calls fetch directly instead of through sameOriginFetcher');
  if (!/sameOriginFetcher\(fetch\.bind/.test(main)) fail('main.ts no longer fetches through sameOriginFetcher');
  if (!viewer.includes("credentials: 'omit'") || !viewer.includes("referrerPolicy: 'no-referrer'")) {
    fail('the fetcher no longer omits credentials and the referrer');
  }
});

Deno.test('rule 1: the page itself can reach nothing but its own origin', () => {
  const csp = HTML.match(/http-equiv="Content-Security-Policy"\s+content="([^"]+)"/)?.[1] ?? fail('no CSP meta');
  for (const directive of [
    "default-src 'none'",
    "connect-src 'self'",
    "img-src blob:",
    "media-src blob:",
    "object-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
  ]) {
    if (!csp.includes(directive)) fail(`the viewer CSP lacks ${directive}`);
  }
  if (/https?:|\*|unsafe-inline|unsafe-eval'/.test(csp.replace("'wasm-unsafe-eval'", ''))) {
    fail(`the viewer CSP admits more than its own origin: ${csp}`);
  }
  if (!HTML.includes('<meta name="referrer" content="no-referrer"')) fail('no referrer meta');
  if (/\b(src|href)="https?:/.test(HTML)) fail('the viewer HTML names an external resource');
});

Deno.test('rule 2: the decrypted file is never rendered as a document', () => {
  for (const [file, src] of SCRIPTS) {
    for (const banned of ['innerHTML', 'outerHTML', 'insertAdjacentHTML', 'document.write', 'srcdoc', 'DOMParser', 'createContextualFragment', 'eval(', 'new Function']) {
      if (src.includes(banned)) fail(`${file} uses ${banned}`);
    }
  }
  const main = SCRIPTS.find(([f]) => f === 'main.ts')![1];
  const tags = [...main.matchAll(/\bel\(\s*'([a-z0-9]+)'/g)].map((m) => m[1]);
  const allowed = new Set(['h1', 'h2', 'p', 'a', 'img', 'audio', 'video', 'pre', 'figure']);
  for (const tag of tags) if (!allowed.has(tag)) fail(`main.ts creates <${tag}>, which may render a document`);
  if (/createElement\(\s*['"](?!['"])/.test(main.replace(/document\.createElement\(tag\)/, ''))) {
    fail('main.ts creates elements outside the el() helper');
  }
  // The download is opaque bytes: the browser saves it, never opens it here.
  if (!/objectUrl\(state\.bytes,\s*'application\/octet-stream'\)/.test(main)) {
    fail('the download no longer carries the opaque octet-stream type');
  }
});

Deno.test('rule 2: the inline preview is exactly image, audio, video and plain text', () => {
  const main = SCRIPTS.find(([f]) => f === 'main.ts')![1];
  const viewer = SCRIPTS.find(([f]) => f === 'viewer.ts')![1];
  const cases = [...main.matchAll(/case '([a-z]+)':/g)].map((m) => m[1]);
  const previewCases = cases.filter((c) => !['generic', 'loading', 'failed', 'ready'].includes(c)).sort();
  if (JSON.stringify(previewCases) !== JSON.stringify(['audio', 'image', 'text', 'video'])) {
    fail(`main.ts previews ${previewCases.join(', ')} — the allow-list is image, audio, video, text`);
  }
  const kinds = viewer.match(/PREVIEW_KINDS[^=]*=\s*\[([^\]]*)\]/)?.[1] ?? fail('no PREVIEW_KINDS');
  if (kinds.replace(/\s/g, '') !== "'image','audio','video','text','none'") fail(`PREVIEW_KINDS is ${kinds}`);
  // The type oracle is shared Rust's; the page keeps no extension list of its own.
  for (const [file, src] of SCRIPTS) {
    if (/\.(png|jpe?g|gif|svg|mp4|mp3|txt|html?)\b/i.test(src)) fail(`${file} names a file extension`);
  }
});

Deno.test('outcome 27: an unfurler gets the generic title and nothing about the file', () => {
  if (!HTML.includes('<title>%SHARE_VIEWER_TITLE%</title>')) fail('the static title is not the generic one');
  if (!VITE.includes('t.share_viewer.title') || !VITE.includes('t.share_viewer.generic_body')) {
    fail('the static text no longer comes from the shared strings');
  }
  const main = SCRIPTS.find(([f]) => f === 'main.ts')![1];
  if (/document\.title\s*=/.test(main)) fail('the page retitles itself (history keeps titles)');
});
