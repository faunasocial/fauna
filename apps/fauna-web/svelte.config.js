import adapter from '@sveltejs/adapter-static';
import { gitSha } from './build-id.js';

// The commit this build is of, when known — see build-id.js for why the
// default (`Date.now()`) makes every build unique and why an unknown commit
// deliberately keeps that default.
const buildSha = gitSha();

/** @type {import('@sveltejs/kit').Config} */
const config = {
  kit: {
    adapter: adapter({
      pages: 'build',
      assets: 'build',
      fallback: 'index.html',
    }),
    paths: {
      base: '/app',
    },
    ...(buildSha ? { version: { name: buildSha } } : {}),
    // (2026-06-23 isolation & client-attack-surface review;
    // docs/goal/behavior/web-content-hosting.md § Same-origin security model,
    // invariant #5). The SPA holds the user's raw Ed25519 master secret in
    // `localStorage` (`fauna_secret`), so any inline-script injection on this
    // origin is permanent identity theft — the CSP is the defense-in-depth that
    // contains it. SvelteKit emits a bootstrap `<script>` inline (render.js
    // builds `<script>${init_app}</script>`); `mode: 'hash'` makes it add that
    // script's sha256 to `script-src` automatically and survive every rebuild,
    // so `script-src 'self'` holds WITHOUT `'unsafe-inline'` (nonce mode is
    // unavailable — it throws under the static adapter's prerender).
    //
    // This CSP is delivered as a `<meta>` tag in the built index.html. A meta CSP
    // CANNOT carry `frame-ancestors` (browsers ignore it there), so the
    // clickjacking lock (`frame-ancestors 'none'` + `X-Frame-Options: DENY`) is
    // set as an HTTP header by the nest on `/app` responses
    // (bins/fauna-nest/src/lib.rs) — the two halves together fulfil invariant #5.
    csp: {
      mode: 'hash',
      directives: {
        'default-src': ['self'],
        // 'wasm-unsafe-eval': the SPA instantiates several WebAssembly modules
        // (fauna_wasm{,_onboarding,_folders,_content_index} + c2pa). No
        // eval()/new Function() is used, so 'unsafe-eval' is deliberately absent.
        'script-src': ['self', 'wasm-unsafe-eval'],
        // 'unsafe-inline': Svelte components set computed inline `style="…"`
        // attributes (event position/colour, etc.). Style injection can't read
        // localStorage, so this is the accepted residual; scripts stay locked.
        'style-src': ['self', 'unsafe-inline'],
        // blob:/data: for URL.createObjectURL media + inline data images; https:
        // for revealed remote post images / avatars (images can't execute).
        'img-src': ['self', 'data:', 'blob:', 'https:'],
        'font-src': ['self', 'data:'],
        'media-src': ['self', 'blob:'],
        // The web app connects to ARBITRARY nests (cross-nest messaging /
        // federation — nodeUrl() is user-overridable), so connect-src can't be
        // pinned to 'self'. https/wss for production peers; http/ws for localhost
        // + LAN self-host. Any XSS this would exfiltrate over is already blocked
        // by script-src 'self'.
        'connect-src': ['self', 'https:', 'http:', 'wss:', 'ws:'],
        // 'self' for the push service worker (/app/service-worker.js); 'blob:'
        // because the c2pa-web WASM SDK may instantiate its parser in a blob:
        // worker. A worker can only run script the page itself spawned, which
        // script-src 'self' already gates, so this doesn't weaken the core lock.
        'worker-src': ['self', 'blob:'],
        'object-src': ['none'],
        'base-uri': ['self'],
        'form-action': ['self'],
      },
    },
  },
};

export default config;
