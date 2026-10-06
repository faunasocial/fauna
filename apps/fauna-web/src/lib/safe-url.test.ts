import { isSafeNavUrl, isSafeNodeUrl } from './safe-url.ts';

function ok(actual: boolean, expected: boolean, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${actual}, want ${expected}`);
  }
}

// the SPA only follows an https: nest-supplied redirect/payment URL.
Deno.test('isSafeNavUrl — accepts well-formed https URLs', () => {
  ok(isSafeNavUrl('https://example.com/oauth/callback'), true, 'plain https');
  ok(isSafeNavUrl('https://pay.example.com:8443/checkout?x=1#frag'), true, 'https with port/query/frag');
});

Deno.test('isSafeNavUrl — rejects non-https schemes (http, javascript, data, file)', () => {
  ok(isSafeNavUrl('http://example.com'), false, 'http');
  ok(isSafeNavUrl('javascript:alert(1)'), false, 'javascript:');
  ok(isSafeNavUrl('data:text/html,<script>alert(1)</script>'), false, 'data:');
  ok(isSafeNavUrl('file:///etc/passwd'), false, 'file:');
});

Deno.test('isSafeNavUrl — rejects malformed / relative / empty / nullish values', () => {
  ok(isSafeNavUrl('not a url'), false, 'not a url');
  ok(isSafeNavUrl('/relative/path'), false, 'relative path');
  ok(isSafeNavUrl('example.com'), false, 'no scheme');
  ok(isSafeNavUrl(''), false, 'empty');
  ok(isSafeNavUrl(null), false, 'null');
  ok(isSafeNavUrl(undefined), false, 'undefined');
});

// the stored nest URL is only honoured
// when it is https: or http: to a localhost-family host (the browser
// secure-context dev/e2e carve-out — `just web-dev` serves the nest at
// http://127.0.0.1:3000). Anything else is ignored so `nodeUrl()` falls back to
// `window.location.origin` rather than letting a spoofed value point the client
// at an attacker-controlled origin.
Deno.test('isSafeNodeUrl — accepts well-formed https URLs (production / LAN-TLS nests)', () => {
  ok(isSafeNodeUrl('https://nest.example.test'), true, 'https nest');
  ok(isSafeNodeUrl('https://192.168.1.57:8443'), true, 'https LAN IP');
  ok(isSafeNodeUrl('https://pi.local'), true, 'https .local');
});

Deno.test('isSafeNodeUrl — accepts http: to localhost-family hosts (the dev/e2e carve-out)', () => {
  ok(isSafeNodeUrl('http://127.0.0.1:3000'), true, 'web-dev / e2e nest');
  ok(isSafeNodeUrl('http://localhost:8080'), true, 'localhost');
  ok(isSafeNodeUrl('http://app.localhost'), true, 'app.localhost');
  ok(isSafeNodeUrl('http://127.5.6.7'), true, '127.0.0.0/8 loopback');
  ok(isSafeNodeUrl('http://[::1]:3000'), true, 'IPv6 loopback');
});

Deno.test('isSafeNodeUrl — rejects http: to non-localhost hosts (the phishing/MITM-relay residual)', () => {
  ok(isSafeNodeUrl('http://evil.example.com'), false, 'http non-localhost');
  ok(isSafeNodeUrl('http://192.168.1.57:8443'), false, 'http LAN IP');
  ok(isSafeNodeUrl('http://nest.example.test'), false, 'http nest');
  ok(isSafeNodeUrl('http://0.0.0.0:3000'), false, 'bind addr, not a loopback locator');
});

Deno.test('isSafeNodeUrl — rejects dangerous / malformed / relative / empty / nullish values', () => {
  ok(isSafeNodeUrl('javascript:alert(1)'), false, 'javascript:');
  ok(isSafeNodeUrl('data:text/html,<script>alert(1)</script>'), false, 'data:');
  ok(isSafeNodeUrl('file:///etc/passwd'), false, 'file:');
  ok(isSafeNodeUrl('not a url'), false, 'not a url');
  ok(isSafeNodeUrl('/relative/path'), false, 'relative path');
  ok(isSafeNodeUrl('127.0.0.1:3000'), false, 'no scheme');
  ok(isSafeNodeUrl(''), false, 'empty');
  ok(isSafeNodeUrl(null), false, 'null');
  ok(isSafeNodeUrl(undefined), false, 'undefined');
});
