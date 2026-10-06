// The browser half of mailbox export's download (docs/goal/behavior/
// mail-export.md § Download flow, step 5's web sentence): the two platform ends
// the shared `MailExportMachine` downloads through — a streamed GET of the
// session's sealed blob, and the place the recovered archive is written.
//
// NO export logic lives here (priority #2). Unwrapping the key, opening the
// frames in order and refusing a truncated or still-running archive are shared
// Rust (`MailExportMachine::download`); the wasm side
// (`libs/fauna-wasm/src/mail_export_delivery.rs`) supplies the bearer and
// calls `abort()` on any sink it drops before `finish()`.
//
// Why a temporary file and not a save-file picker or a Blob:
//  - An archive may be 10 GiB (§ Quota composition), so nothing may hold it
//    whole in memory — which rules out collecting a `Blob` from chunks.
//  - A refused archive must leave nothing that reads as a mailbox. The bytes
//    therefore accumulate in the origin-private file system, which the user
//    cannot see, and the browser's own download is started only by `finish()`
//    — after the shared opener has checked the terminator. `abort()` deletes
//    the temporary file.
//  - The save-file picker exists in one browser family only and must be opened
//    before the asynchronous unwrap spends the click. The origin-private file
//    system plus an ordinary download is the one shape every browser runs.

/** What the wasm delivery pulls the sealed blob through. */
export interface MailExportBlobReader {
  /** The next slice of the body, or `null` at its end. */
  next(): Promise<Uint8Array | null>;
  /** Stop the transfer — the opener refused, nothing more will be read. */
  cancel(): void;
}

/** What the wasm delivery writes the recovered archive into. */
export interface MailExportArchiveSink {
  write(bytes: Uint8Array): Promise<void>;
  /** Close the file and hand it to the browser's download. Resolves the name
   *  the archive was saved under. */
  finish(): Promise<string>;
  /** Discard everything written. Called for a sink dropped unfinished. */
  abort(): void;
}

export interface MailExportSavePort {
  openDownload(downloadUrl: string, bearer: string): Promise<MailExportBlobReader>;
  createArchive(fileName: string): Promise<MailExportArchiveSink>;
}

/** The origin-private directory the temporary archives live in. */
const TEMP_DIR = 'mail-export';

/** A temporary archive older than this is litter. The browser reads the file
 *  from disk while it copies it to the user's downloads, and nothing tells the
 *  page when that copy ends — so a finished archive's temporary file is removed
 *  by the next sweep rather than at once, and an hour is far longer than a
 *  disk-to-disk copy of the largest archive takes. */
const STALE_AFTER_MS = 60 * 60 * 1000;

/** How long the object URL outlives the click that started the download. The
 *  browser holds its own reference once the download has begun. */
const REVOKE_AFTER_MS = 60 * 1000;

async function tempDir(): Promise<FileSystemDirectoryHandle> {
  if (!navigator.storage?.getDirectory) {
    throw new Error('this browser has no private file storage to save an export through');
  }
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(TEMP_DIR, { create: true });
}

/** Remove temporary archives no download can still be reading. Best-effort:
 *  a leftover is at worst litter in storage the user never sees, and it never
 *  carries an archive's name. */
export async function sweepStaleExportArchives(now: number = Date.now()): Promise<void> {
  try {
    const dir = await tempDir();
    // `entries()` is an async iterator the DOM lib types do not carry yet.
    const entries = (dir as unknown as {
      entries(): AsyncIterable<[string, FileSystemHandle]>;
    }).entries();
    for await (const [name, handle] of entries) {
      if (handle.kind !== 'file') continue;
      const file = await (handle as FileSystemFileHandle).getFile();
      if (now - file.lastModified > STALE_AFTER_MS) {
        await dir.removeEntry(name).catch(() => {});
      }
    }
  } catch {
    // No private storage, or it is unavailable (a private window): nothing to sweep.
  }
}

/** Build the save port. `baseUrl` is the session's nest — `downloadUrl` is the
 *  nest-minted path from the session row, never one the page composes. */
export function mailExportSavePort(baseUrl: () => string): MailExportSavePort {
  return {
    async openDownload(downloadUrl: string, bearer: string): Promise<MailExportBlobReader> {
      const resp = await fetch(baseUrl().replace(/\/+$/, '') + downloadUrl, {
        headers: { Authorization: `Bearer ${bearer}` },
      });
      if (!resp.ok || !resp.body) {
        throw new Error(`HTTP ${resp.status}`);
      }
      // One slice at a time off the body stream — never `arrayBuffer()`.
      const reader = resp.body.getReader();
      return {
        async next(): Promise<Uint8Array | null> {
          const { done, value } = await reader.read();
          return done ? null : value ?? new Uint8Array(0);
        },
        cancel(): void {
          reader.cancel().catch(() => {});
        },
      };
    },

    async createArchive(fileName: string): Promise<MailExportArchiveSink> {
      await sweepStaleExportArchives();
      const dir = await tempDir();
      // Unique per download, so two downloads of one export never share a file
      // and the temporary name is never the archive's.
      const tempName = `${crypto.randomUUID()}.part`;
      const handle = await dir.getFileHandle(tempName, { create: true });
      if (typeof handle.createWritable !== 'function') {
        await dir.removeEntry(tempName).catch(() => {});
        throw new Error('this browser cannot write an export archive to private file storage');
      }
      const writable = await handle.createWritable();
      let closed = false;
      return {
        async write(bytes: Uint8Array): Promise<void> {
          await writable.write(bytes as unknown as BufferSource);
        },
        async finish(): Promise<string> {
          await writable.close();
          closed = true;
          // A disk-backed `File`: the browser streams it from disk into the
          // user's downloads, so the archive is still never held in memory.
          const file = await handle.getFile();
          const url = URL.createObjectURL(file);
          const a = document.createElement('a');
          a.href = url;
          a.download = fileName;
          a.style.display = 'none';
          document.body.appendChild(a);
          a.click();
          a.remove();
          setTimeout(() => URL.revokeObjectURL(url), REVOKE_AFTER_MS);
          return fileName;
        },
        abort(): void {
          if (closed) return;
          closed = true;
          writable
            .abort()
            .catch(() => {})
            .then(() => dir.removeEntry(tempName))
            .catch(() => {});
        },
      };
    },
  };
}
