// A QR's dark modules, pre-offset by the quiet zone and keyed for Svelte's
// {#each} — shared by every page that draws a secret as a QR (Settings'
// identity export and recovery-kit display, onboarding's recovery-kit offer:
// "the same artifact, same shape"), each drawn as one <svg> with one <rect>
// per dark module rather than pulling a JS QR library (priorities #1/#2). The
// grid itself is shared Rust (`fauna_core::qr_matrix`, over wasm).
import { qrMatrix, qrQuietZoneModules } from '$lib/wasm';

export interface QrDisplay {
  side: number;
  darkModules: { key: number; x: number; y: number }[];
}

export function qrDisplayFromUri(uri: string): QrDisplay {
  const m = qrMatrix(uri);
  // The matrix carries no quiet zone — the renderer pads all four sides, or
  // scanners refuse the code. Don't hard-code 4; that is what this shared
  // export is for.
  const quiet = qrQuietZoneModules();
  const darkModules: { key: number; x: number; y: number }[] = [];
  for (let y = 0; y < m.size; y++) {
    for (let x = 0; x < m.size; x++) {
      if (m.modules[y * m.size + x]) {
        darkModules.push({ key: y * m.size + x, x: x + quiet, y: y + quiet });
      }
    }
  }
  return { side: m.size + 2 * quiet, darkModules };
}
