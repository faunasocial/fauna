// The passkey wrap's WebAuthn half — `docs/goal/architecture/apps/web.md`
// § Identity store at rest → *A passkey wrap*. A credential created in the page
// with the PRF extension; its PRF output (over a per-wrap 32-byte salt) is the
// input the wasm core runs through BLAKE3 `derive_key` to wrap the same store
// key the passphrase wraps. The nest takes no part: the challenge is never
// verified by anyone — only the PRF output matters, and it never leaves the
// page.
//
// Errors reject with stable code strings, like the identity-store doors:
// `passkey_unavailable` (no WebAuthn, no secure named host, or the
// authenticator offers no PRF), `passkey_cancelled` (the person or the browser
// declined the ceremony), and the doors' own (`passkey_did_not_open`, …).

import {
  identityStoreAddPasskeyWrap,
  identityStoreNewPasskeySalt,
  identityStorePasskeyWraps,
  identityStoreRemovePasskeyWrap,
  identityStoreUnlockWithPasskeyOutput,
  type PasskeyWrapInfo,
} from '#lib/wasm-launch';

// The PRF extension's shapes, which the DOM typings do not carry yet.
interface PrfValues {
  first: BufferSource;
}
interface PrfInputs {
  eval?: PrfValues;
  evalByCredential?: Record<string, PrfValues>;
}
interface PrfOutputs {
  prf?: { enabled?: boolean; results?: { first?: ArrayBuffer } };
}

/** Whether this page can offer a passkey at all: WebAuthn exists, the page is
 *  a secure context, and its host is a NAME — WebAuthn refuses an IP-literal
 *  relying-party id (`common.md` § Credential storage: "only in a secure
 *  context, on a named host"). Whether the authenticator offers PRF is learnt
 *  only from the ceremony itself. */
export function passkeysPossible(): boolean {
  if (typeof window === 'undefined' || !window.isSecureContext) return false;
  if (typeof PublicKeyCredential === 'undefined' || !navigator.credentials) return false;
  const host = location.hostname;
  if (host.startsWith('[') || /^\d{1,3}(?:\.\d{1,3}){3}$/.test(host)) return false;
  return true;
}

function base64url(bytes: Uint8Array): string {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function randomBytes(n: number): Uint8Array<ArrayBuffer> {
  return crypto.getRandomValues(new Uint8Array(n));
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

function ceremonyError(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof DOMException && e.name === 'NotAllowedError') return 'passkey_cancelled';
  if (e instanceof DOMException && (e.name === 'SecurityError' || e.name === 'NotSupportedError')) {
    return 'passkey_unavailable';
  }
  return e instanceof Error ? e.message : String(e);
}

async function getAssertion(
  wraps: PasskeyWrapInfo[],
): Promise<{ credentialId: Uint8Array<ArrayBuffer>; prfOutput: Uint8Array<ArrayBuffer> }> {
  const evalByCredential: Record<string, PrfValues> = {};
  for (const w of wraps) evalByCredential[base64url(w.credentialId)] = { first: w.prfSalt };
  const prf: PrfInputs = { evalByCredential };
  const cred = (await navigator.credentials.get({
    publicKey: {
      challenge: randomBytes(32),
      allowCredentials: wraps.map((w) => ({ type: 'public-key' as const, id: w.credentialId })),
      userVerification: 'required',
      extensions: { prf } as AuthenticationExtensionsClientInputs,
    },
  })) as PublicKeyCredential | null;
  if (!cred) throw 'passkey_cancelled';
  const first = (cred.getClientExtensionResults() as PrfOutputs).prf?.results?.first;
  if (!first) throw 'passkey_unavailable';
  return { credentialId: new Uint8Array(cred.rawId), prfOutput: new Uint8Array(first) };
}

/** Whether the seal header holds a passkey wrap. */
export async function hasPasskeyWrap(): Promise<boolean> {
  return (await identityStorePasskeyWraps()).length > 0;
}

/** Unlock with one of the header's passkeys. The authenticator picks the
 *  credential; the PRF output it returns for that credential's salt opens its
 *  wrap. */
export async function unlockWithPasskey(): Promise<void> {
  const wraps = await identityStorePasskeyWraps();
  if (!wraps.length || !passkeysPossible()) throw 'passkey_unavailable';
  let assertion;
  try {
    assertion = await getAssertion(wraps);
  } catch (e) {
    throw ceremonyError(e);
  }
  await identityStoreUnlockWithPasskeyOutput(assertion.credentialId, assertion.prfOutput);
}

/** Create a passkey and add its wrap of the held store key (the store must be
 *  unlocked). An authenticator that returns no PRF result at creation is asked
 *  once more with an assertion; one that offers no PRF at all writes nothing. */
export async function addPasskeyWrap(): Promise<void> {
  if (!passkeysPossible()) throw 'passkey_unavailable';
  const salt = await identityStoreNewPasskeySalt();
  let credentialId: Uint8Array<ArrayBuffer>;
  let prfOutput: Uint8Array<ArrayBuffer>;
  try {
    const prf: PrfInputs = { eval: { first: salt } };
    const cred = (await navigator.credentials.create({
      publicKey: {
        challenge: randomBytes(32),
        rp: { name: 'Fauna' },
        user: { id: randomBytes(16), name: location.host, displayName: location.host },
        pubKeyCredParams: [
          { type: 'public-key', alg: -7 },
          { type: 'public-key', alg: -257 },
        ],
        authenticatorSelection: { residentKey: 'preferred', userVerification: 'required' },
        extensions: { prf } as AuthenticationExtensionsClientInputs,
      },
    })) as PublicKeyCredential | null;
    if (!cred) throw 'passkey_cancelled';
    credentialId = new Uint8Array(cred.rawId);
    const out = (cred.getClientExtensionResults() as PrfOutputs).prf;
    if (!out?.enabled && !out?.results?.first) throw 'passkey_unavailable';
    if (out.results?.first) {
      prfOutput = new Uint8Array(out.results.first);
    } else {
      const assertion = await getAssertion([{ credentialId, prfSalt: salt }]);
      if (!sameBytes(assertion.credentialId, credentialId)) throw 'passkey_unavailable';
      prfOutput = assertion.prfOutput;
    }
  } catch (e) {
    throw ceremonyError(e);
  }
  await identityStoreAddPasskeyWrap(credentialId, salt, prfOutput);
}

/** Remove every passkey wrap — the page offers one passkey, so this is "remove
 *  the passkey". The passphrase still opens the store. */
export async function removePasskeyWraps(): Promise<void> {
  for (const w of await identityStorePasskeyWraps()) {
    await identityStoreRemovePasskeyWrap(w.credentialId);
  }
}
