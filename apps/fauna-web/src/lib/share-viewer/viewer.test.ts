// The share viewer's flow (`viewer.ts`), driven with a fake wasm and a fake
// fetcher: which states it reports, what it fetches, and that it never fetches
// anything carrying the key (share-links.md § The private-file extension —
// rule 1; catalog outcomes `share-links` 20, 23, 25, 27). The wasm's own
// decisions (open, verify, the preview allow-list, the sentences) are tested in
// shared Rust (`fauna-client-share` `viewer` tests).

import {
  type Fetched,
  type OpenedLink,
  previewKind,
  runViewer,
  sameOriginFetcher,
  type ShareWasm,
  type ViewerState,
} from './viewer.ts';

const FILE = new TextEncoder().encode('the file');

function fakeWasm(over: Partial<ShareWasm> = {}): ShareWasm {
  const opened: OpenedLink = {
    filename: 'notes.txt',
    sizeText: '8 B',
    chunkCount: 2,
    contentType: 'text/plain',
    preview: 'text',
    assemble: (chunks) => {
      if (chunks.length !== 2) throw 'DAMAGED';
      return FILE;
    },
  };
  return {
    viewerStart: (pathname, hash) => {
      const token = pathname.startsWith('/share/') ? pathname.slice(7) : '';
      const fragment = hash.replace(/^#/, '');
      return token && fragment ? { token, fragment } : undefined;
    },
    manifestPath: (token) => `/share/${token}/manifest`,
    chunkPath: (token, i) => `/share/${token}/chunk/${i}`,
    statusText: (status) => `STATUS ${status}`,
    openShare: (_t, fragment) => {
      if (fragment !== 'KEY') throw 'DAMAGED';
      return opened;
    },
    viewerText: () => ({
      title: 'T',
      genericBody: 'G',
      loading: 'L',
      download: 'D',
      keepNote: 'K',
    }),
    ...over,
  };
}

async function run(
  address: { pathname: string; hash: string },
  answer: (path: string) => Fetched = () => ({ ok: true, bytes: new Uint8Array([1]) }),
  wasm: ShareWasm = fakeWasm(),
): Promise<{ states: ViewerState[]; fetched: string[] }> {
  const states: ViewerState[] = [];
  const fetched: string[] = [];
  await runViewer(
    wasm,
    address,
    (path) => {
      fetched.push(path);
      return Promise.resolve(answer(path));
    },
    (s) => states.push(s),
  );
  return { states, fetched };
}

function assertEq(actual: unknown, expected: unknown, what: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

Deno.test('a link with its key opens: manifest, every chunk, then the verified file', async () => {
  const { states, fetched } = await run({ pathname: '/share/TOK', hash: '#KEY' });
  assertEq(
    fetched,
    ['/share/TOK/manifest', '/share/TOK/chunk/0', '/share/TOK/chunk/1'],
    'fetched paths',
  );
  assertEq(states.map((s) => s.kind), ['loading', 'ready'], 'states');
  const ready = states[1] as Extract<ViewerState, { kind: 'ready' }>;
  assertEq(
    [ready.filename, ready.sizeText, ready.preview, ready.contentType],
    ['notes.txt', '8 B', 'text', 'text/plain'],
    'ready fields',
  );
  if (ready.bytes !== FILE) throw new Error('the ready bytes are not the assembled file');
});

Deno.test('the key is never part of any request the viewer makes', async () => {
  const { fetched } = await run({ pathname: '/share/TOK', hash: '#KEY' });
  for (const path of fetched) {
    if (path.includes('KEY') || path.includes('#')) {
      throw new Error(`a request carried the key: ${path}`);
    }
  }
});

Deno.test('no fragment — an unfurler or a pre-fetch — gets the generic page and no fetch', async () => {
  const { states, fetched } = await run({ pathname: '/share/TOK', hash: '' });
  assertEq(states, [{ kind: 'generic' }], 'states');
  assertEq(fetched, [], 'fetched');
});

for (const status of [410, 451, 404, 500]) {
  Deno.test(`a ${status} on the manifest is said plainly, once, with no retry`, async () => {
    const { states, fetched } = await run({ pathname: '/share/TOK', hash: '#KEY' }, () => ({
      ok: false,
      status,
    }));
    assertEq(fetched, ['/share/TOK/manifest'], 'fetched once');
    assertEq(states, [{ kind: 'loading' }, { kind: 'failed', message: `STATUS ${status}` }], 'states');
  });
}

Deno.test('a link revoked mid-download stops at the refused chunk', async () => {
  const { states, fetched } = await run({ pathname: '/share/TOK', hash: '#KEY' }, (path) =>
    path.endsWith('/chunk/1') ? { ok: false, status: 410 } : { ok: true, bytes: new Uint8Array([1]) },
  );
  assertEq(fetched.length, 3, 'fetched until the refusal');
  assertEq(states.at(-1), { kind: 'failed', message: 'STATUS 410' }, 'last state');
});

Deno.test('a wrong key says the wasm sentence and fetches no chunk', async () => {
  const { states, fetched } = await run({ pathname: '/share/TOK', hash: '#WRONG' });
  assertEq(fetched, ['/share/TOK/manifest'], 'fetched');
  assertEq(states.at(-1), { kind: 'failed', message: 'DAMAGED' }, 'last state');
});

Deno.test('a failed verification never reaches the ready state', async () => {
  const wasm = fakeWasm();
  const opened = wasm.openShare('t', 'KEY', new Uint8Array());
  const failing = fakeWasm({
    openShare: () => ({
      ...opened,
      assemble: () => {
        throw 'DAMAGED';
      },
    }),
  });
  const { states } = await run({ pathname: '/share/TOK', hash: '#KEY' }, undefined, failing);
  assertEq(states.map((s) => s.kind), ['loading', 'failed'], 'states');
});

Deno.test('no answer at all is the plain try-again sentence', async () => {
  const states: ViewerState[] = [];
  await runViewer(
    fakeWasm(),
    { pathname: '/share/TOK', hash: '#KEY' },
    () => Promise.reject(new TypeError('network')),
    (s) => states.push(s),
  );
  assertEq(states.at(-1), { kind: 'failed', message: 'STATUS 0' }, 'last state');
});

Deno.test('an unknown preview kind from the wasm is download-only', () => {
  for (const k of ['image', 'audio', 'video', 'text', 'none']) assertEq(previewKind(k), k, k);
  for (const k of ['html', 'svg', 'document', '']) assertEq(previewKind(k), 'none', k);
});

Deno.test('the fetcher sends no cookie and no referrer, and refuses any other origin', async () => {
  const calls: [string, RequestInit | undefined][] = [];
  const fake = ((url: string, init?: RequestInit) => {
    calls.push([url, init]);
    return Promise.resolve(new Response(new Uint8Array([7]), { status: 200 }));
  }) as typeof fetch;
  const fetchBytes = sameOriginFetcher(fake);
  const got = await fetchBytes('/share/TOK/manifest');
  assertEq(got.ok, true, 'ok');
  assertEq(
    calls[0][1],
    { credentials: 'omit', referrerPolicy: 'no-referrer', cache: 'no-store', redirect: 'error' },
    'init',
  );
  for (const bad of ['https://elsewhere.example/x', '//elsewhere.example/x', 'relative', '/share/T#KEY']) {
    let refused = false;
    try {
      await fetchBytes(bad);
    } catch {
      refused = true;
    }
    if (!refused) throw new Error(`the fetcher let ${bad} through`);
  }
  assertEq(calls.length, 1, 'no refused path reached fetch');
  const gone = await sameOriginFetcher(
    (() => Promise.resolve(new Response(null, { status: 410 }))) as unknown as typeof fetch,
  )('/share/TOK/manifest');
  assertEq(gone, { ok: false, status: 410 }, 'a refusal');
});
