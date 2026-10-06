import { base } from "$app/paths";
import { toArrayBufferView } from "./bytes";

export interface C2paResult {
  signer: string;
  signingDate: string;
  claimGenerator: string;
  isValid: boolean;
}

type C2paSdk = { reader: { fromBlob(type: string, blob: Blob): Promise<any> } };
let c2paInstance: C2paSdk | null = null;
const cache = new Map<string, C2paResult | null>();

/**
 * Every read below resolves to `null` on ANY failure — SDK load, WASM fetch, a
 * malformed manifest — because a missing badge must never break the page. That
 * is right for the product and terrible for diagnosis: an absent badge and an
 * absent SDK look identical from the DOM, which is how the upload-side
 * `has_c2pa` regression stayed "root cause undiagnosed" across three sessions
 * (`media.md` § C2PA provenance). The distinguishing detail exists only inside
 * these catch blocks, so say it out loud — `console` is what
 * `driver.console_log()` reads in an e2e.
 */
function trace(what: string): void {
  console.debug(`fauna_web::c2pa: ${what}`);
}

async function getC2pa(): Promise<C2paSdk> {
  if (!c2paInstance) {
    const { createC2pa } = await import("@contentauth/c2pa-web");
    c2paInstance = await createC2pa({
      wasmSrc: `${base}/c2pa.wasm`,
    });
  }
  return c2paInstance;
}

export async function readProvenanceFromUrl(
  url: string,
): Promise<C2paResult | null> {
  if (cache.has(url)) return cache.get(url)!;

  try {
    const response = await fetch(url);

    // Server-side cache: skip WASM parsing if server confirms no C2PA
    const c2paHeader = response.headers.get('x-c2pa');
    if (c2paHeader === 'false') {
      cache.set(url, null);
      return null;
    }

    const sdk = await getC2pa();
    const blob = await response.blob();
    const reader = await sdk.reader.fromBlob(blob.type, blob);
    if (!reader) {
      trace(`no manifest in ${url} (header=${c2paHeader ?? "absent"})`);
      cache.set(url, null);
      return null;
    }
    const result = await extractResult(reader);
    await reader.free();
    cache.set(url, result);
    return result;
  } catch (e) {
    trace(`read from ${url} failed: ${e}`);
    cache.set(url, null);
    return null;
  }
}

export async function readProvenanceFromBytes(
  data: Uint8Array,
  mimeType: string,
): Promise<C2paResult | null> {
  const mid = Math.floor(data.length / 2);
  const sample = [data[0], data[mid], data[data.length - 1], data[Math.floor(mid / 2)]].join(",");
  const key = `bytes:${data.length}:${sample}`;
  if (cache.has(key)) return cache.get(key)!;

  try {
    const sdk = await getC2pa();
    const blob = new Blob([toArrayBufferView(data)], { type: mimeType });
    const reader = await sdk.reader.fromBlob(mimeType, blob);
    if (!reader) {
      trace(`no manifest in ${data.length}B ${mimeType || "(no mime)"} buffer`);
      cache.set(key, null);
      return null;
    }
    const result = await extractResult(reader);
    await reader.free();
    cache.set(key, result);
    return result;
  } catch (e) {
    trace(`read from ${data.length}B ${mimeType || "(no mime)"} buffer failed: ${e}`);
    cache.set(key, null);
    return null;
  }
}

async function extractResult(reader: { activeManifest: () => Promise<any>; manifestStore: () => Promise<any> }): Promise<C2paResult | null> {
  const manifest = await reader.activeManifest();
  if (!manifest) return null;

  const store = await reader.manifestStore();

  const signer =
    manifest.signature_info?.issuer ?? manifest.signature_info?.common_name ?? manifest.claim_generator ?? "Unknown";
  const signingDate = manifest.signature_info?.time ?? "";
  const claimGenerator = manifest.claim_generator ?? "Unknown";

  // validation_state is the simplest check: "Valid", "Trusted", or "Invalid"
  const validationState = store?.validation_state;
  const isValid = validationState !== "Invalid";

  return { signer, signingDate, claimGenerator, isValid };
}
