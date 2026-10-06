// deno-sandbox-preload.mjs — run by scripts/deno-sandbox.ts (`deno run --preload`)
// before every build tool it launches (build-system.md § The Deno build sandbox).
//
// Sorted directory listings. A directory's raw entry order is the filesystem's
// (ZFS salts each directory's hash, ext4's htree seeds it per filesystem), so a
// tool that numbers what it finds in that order builds different bytes from the
// same commit in two checkouts — SvelteKit numbers its route nodes in the order
// `src/routes` lists, and every hashed `_app/immutable/*` name follows
// (release-integrity.md § Release signing → Web-app verifiability, piece 1: the
// BUILD sorts its directory listings, never an environment rule).
//
// What it reaches: `node:fs`'s default export (`fs.readdirSync`, `fs.readdir`),
// `fs.promises` (the same object as `node:fs/promises`'s default export) and every
// `require("fs")`. What it cannot: a NAMED import (`import { readdirSync } from
// "node:fs"`) binds the polyfill's own function, and Deno's
// `module.syncBuiltinESMExports` throws "not implemented" (measured on Deno 2.7) —
// so the proof that it reaches what a build reads is the build itself
// (byte-identical trees from two checkouts with different directory orders), never
// this file's coverage.

import { Buffer } from "node:buffer";
import fs from "node:fs";

const name = (e) => (typeof e === "string" ? e : e instanceof Uint8Array ? e : e.name);

function compare(a, b) {
  const x = name(a);
  const y = name(b);
  if (typeof x === "string" && typeof y === "string") return x < y ? -1 : x > y ? 1 : 0;
  return Buffer.compare(Buffer.from(x), Buffer.from(y));
}

const sorted = (entries) => (Array.isArray(entries) ? entries.sort(compare) : entries);

const readdirSync = fs.readdirSync;
fs.readdirSync = function (...args) {
  return sorted(readdirSync.apply(this, args));
};

const readdir = fs.readdir;
fs.readdir = function (...args) {
  const done = args.pop();
  if (typeof done !== "function") return readdir.apply(this, [...args, done]);
  return readdir.apply(this, [...args, (err, entries) => done(err, err ? entries : sorted(entries))]);
};

const readdirPromise = fs.promises.readdir;
fs.promises.readdir = async function (...args) {
  return sorted(await readdirPromise.apply(this, args));
};
