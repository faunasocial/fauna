"""Seed and read the web SPA's account registry straight through
``localStorage`` — the browser's ``LocalStorageSecretStore`` keys every registry
logical key verbatim, so a plain ``setItem`` per key is byte-for-byte what the
registry reads at launch (``libs/fauna-client-accounts/src/web_store.rs``).

This is the ONLY identity store web has (2026-09-24,
``long-term-store.md`` § Downgrade mirror + abandoned-append recovery): the
pre-registry ``fauna_secret`` / ``fauna_node_url`` / ``fauna_pending_invite_*``
/ ``fauna_awaiting_dns_*`` keys are neither written nor read by the SPA any
more, so a test that seeds an identity or a wizard-resume slot writes the
registry shape from ``common.accounts`` and reads the per-actor rows back.
"""
from __future__ import annotations

import json

from common.accounts import actor_id_hex, single_account_seed


def seed_js(fields: dict[str, str]) -> str:
    """A one-expression script writing every ``{logical_key: value}`` pair."""
    return "(() => { " + " ".join(
        f"localStorage.setItem({json.dumps(k)}, {json.dumps(v)});" for k, v in fields.items()
    ) + " return true; })()"


def seed(driver, fields: dict[str, str]) -> None:
    driver.eval_js(seed_js(fields))


def seed_identity(
    driver,
    secret_hex: str,
    *,
    nest_url: str | None = None,
    handle: str | None = None,
    domain: str | None = None,
    device_id: str | None = None,
) -> str:
    """One signed-in (or mid-onboarding, when ``nest_url`` is omitted) identity
    in the registry shape; returns its actor id."""
    seed(
        driver,
        single_account_seed(
            secret_hex, nest_url=nest_url, handle=handle, domain=domain, device_id=device_id
        ),
    )
    return actor_id_hex(secret_hex)


def pending_invite_record(
    *, nest_url: str, handle: str, request_id: str, status_json: str
) -> str:
    """The per-actor ``fauna/{actor}/pending_invite`` value —
    ``fauna_launch_machine::PendingInviteRecord`` as the registry stores it."""
    return json.dumps(
        {"nest_url": nest_url, "handle": handle, "request_id": request_id, "status_json": status_json}
    )


def awaiting_dns_record(
    *, nest_url: str, handle: str, dns_records_json: str, claim_code: str
) -> str:
    """The per-actor ``fauna/{actor}/awaiting_dns`` value —
    ``fauna_launch_machine::AwaitingDnsRecord`` as the registry stores it."""
    return json.dumps(
        {
            "nest_url": nest_url,
            "handle": handle,
            "dns_records_json": dns_records_json,
            "claim_code": claim_code,
        }
    )


def read_key(driver, key: str):
    return driver.eval_js(f"localStorage.getItem({json.dumps(key)})")


def read_index(driver) -> dict | None:
    raw = read_key(driver, "fauna/index")
    return json.loads(raw) if raw else None


def active_actor(driver) -> str | None:
    idx = read_index(driver)
    return idx.get("active") if idx else None


def read_slot(driver, actor: str, slot: str):
    """One per-actor row (``secret`` / ``nest_url`` / ``device_id`` /
    ``pending_invite`` / ``awaiting_dns`` / ``pending_factory_reset``), or None."""
    return read_key(driver, f"fauna/{actor}/{slot}")


def read_record(driver, actor: str, slot: str) -> dict | None:
    raw = read_slot(driver, actor, slot)
    return json.loads(raw) if raw else None


def active_secret(driver):
    """The ACTIVE account's secret — the origin-shared identity every unpinned
    tab resolves to (a pinned tab resolves its own account instead)."""
    actor = active_actor(driver)
    return read_slot(driver, actor, "secret") if actor else None


#: The root every account store's name sits under
#: (`fauna_account_store::root::PLATFORM_ROOT`): each account's IndexedDB
#: database and OPFS directory is `fauna-account-store/<actor>`.
ACCOUNT_STORE_ROOT = "fauna-account-store"


def account_store_names(driver) -> dict:
    """Every account store this origin holds, by home: the IndexedDB databases
    and the OPFS root's directories under the account-store root. The OPFS
    directory is the database name folded to one path component, so both are
    matched on the root prefix."""
    return driver.eval_js(
        "(async () => {"
        " const dbs = (await indexedDB.databases()).map(d => d.name);"
        " const dirs = [];"
        " try {"
        "  const root = await navigator.storage.getDirectory();"
        "  for await (const name of root.keys()) dirs.push(name);"
        " } catch (e) { dirs.push('opfs-unreadable: ' + e); }"
        f" const ours = n => String(n).startsWith({json.dumps(ACCOUNT_STORE_ROOT)})"
        " || String(n).startsWith('opfs-unreadable');"
        " return { indexeddb: dbs.filter(ours).sort(), opfs: dirs.filter(ours).sort() };"
        "})()"
    )


#: Walks every place this origin keeps bytes a copy of the browser profile
#: carries — `localStorage`, `sessionStorage`, every IndexedDB database (each
#: store's keys and values, typed arrays as their bytes, objects walked) and
#: every OPFS file — and reports, per needle (hex of its bytes), the places
#: whose bytes contain it. Matching is on byte boundaries (an even hex offset).
_ORIGIN_HOLDINGS_JS = r"""
(async (needles) => {
  const enc = new TextEncoder();
  const hex = (u8) => Array.from(u8, b => b.toString(16).padStart(2, '0')).join('');
  const holds = (h, n) => {
    for (let i = h.indexOf(n); i !== -1; i = h.indexOf(n, i + 1)) if (i % 2 === 0) return true;
    return false;
  };
  const places = [];
  const add = (where, u8) => places.push([where, hex(u8)]);
  const walk = (where, v, depth) => {
    if (v == null || depth > 8) return;
    if (typeof v === 'string') return add(where, enc.encode(v));
    if (v instanceof ArrayBuffer) return add(where, new Uint8Array(v));
    if (ArrayBuffer.isView(v)) return add(where, new Uint8Array(v.buffer, v.byteOffset, v.byteLength));
    if (Array.isArray(v)) return v.forEach((x, i) => walk(where + '[' + i + ']', x, depth + 1));
    if (typeof v === 'object') for (const k of Object.keys(v)) walk(where + '.' + k, v[k], depth + 1);
  };
  for (const [name, st] of [['localStorage', localStorage], ['sessionStorage', sessionStorage]])
    for (let i = 0; i < st.length; i++) { const k = st.key(i); add(name + ':' + k, enc.encode(k + '=' + st.getItem(k))); }
  let idbRead = 0;
  for (const d of await indexedDB.databases()) {
    const db = await new Promise((ok, ko) => { const r = indexedDB.open(d.name); r.onsuccess = () => ok(r.result); r.onerror = () => ko(r.error); });
    for (const s of Array.from(db.objectStoreNames)) {
      const [keys, vals] = await new Promise((ok, ko) => {
        const t = db.transaction(s, 'readonly').objectStore(s);
        const rk = t.getAllKeys(); const rv = t.getAll();
        rv.onsuccess = () => ok([rk.result, rv.result]); rv.onerror = () => ko(rv.error);
      });
      keys.forEach((k, i) => { walk('idb:' + d.name + '/' + s + '#' + i + ':key', k, 0); walk('idb:' + d.name + '/' + s + '#' + i, vals[i], 0); });
      idbRead += vals.length;
    }
    db.close();
  }
  let opfsFiles = 0;
  try {
    const dir = async (h, path) => {
      for await (const [name, e] of h.entries()) {
        if (e.kind === 'directory') await dir(e, path + name + '/');
        else { add('opfs:' + path + name, new Uint8Array(await (await e.getFile()).arrayBuffer())); opfsFiles++; }
      }
    };
    await dir(await navigator.storage.getDirectory(), '');
  } catch (e) { places.push(['opfs-unreadable: ' + e, '']); }
  const found = {};
  for (const n of needles) found[n] = places.filter(([, h]) => holds(h, n)).map(([w]) => w);
  return { found, places: places.length, idb_values: idbRead, opfs_files: opfsFiles };
})
"""


def origin_holdings(driver, needles: list[bytes]) -> dict:
    """Where this origin's stored bytes contain each needle: ``{"found":
    {needle_hex: [place, …]}, "places": n, "idb_values": n, "opfs_files": n}``.
    The counts are the self-diagnosing half of an absence (convention 6): a
    search that read nothing finds nothing."""
    arg = json.dumps([n.hex() for n in needles])
    return driver.eval_js(f"({_ORIGIN_HOLDINGS_JS.strip()})({arg})")


#: The install-scoped sign-out record (`fauna_client_accounts::sign_out_record`)
#: — outside the `fauna/` prefix the registry and the e2e agent sweep.
SIGN_OUT_RECORD_KEY = "fauna_sign_out_record"

#: The property a test's own holding connection is parked on.
_HELD_STORE = "__fauna_e2e_held_account_store"


def hold_account_store(driver, name: str) -> None:
    """Open a second connection to the database `name` in `driver`'s page and
    keep it — the web store fault injection. It registers no `versionchange`
    handler, so it never closes for a delete: the browser answers the delete
    request `blocked` and leaves it waiting, which is what a connection another
    tab holds does. The waiting request runs once the holder lets go
    (`release_account_store`)."""
    opened = driver.eval_js(
        "(async () => {"
        f" window[{json.dumps(_HELD_STORE)}] = await new Promise((resolve, reject) => {{"
        f"  const request = indexedDB.open({json.dumps(name)});"
        "  request.onsuccess = () => resolve(request.result);"
        "  request.onerror = () => reject(request.error);"
        " });"
        f" return window[{json.dumps(_HELD_STORE)}].name;"
        "})()"
    )
    assert opened == name, f"the holding connection opened {opened!r}, expected {name!r}"


def release_account_store(driver) -> None:
    driver.eval_js(
        f"(() => {{ window[{json.dumps(_HELD_STORE)}]?.close();"
        f" delete window[{json.dumps(_HELD_STORE)}]; return true; }})()"
    )


def account_store_counts(driver, name: str) -> dict | None:
    """Each object store of the database `name` → its record count, or None
    when the origin holds no such database. Opened only when listed, so the
    read never creates the store it inspects."""
    return driver.eval_js(
        "(async (name) => {"
        " if (!(await indexedDB.databases()).some(d => d.name === name)) return null;"
        " const db = await new Promise((ok, ko) => { const r = indexedDB.open(name);"
        "  r.onsuccess = () => ok(r.result); r.onerror = () => ko(r.error); });"
        " const out = {};"
        " for (const s of Array.from(db.objectStoreNames)) {"
        "  out[s] = await new Promise((ok, ko) => {"
        "   const r = db.transaction(s, 'readonly').objectStore(s).count();"
        "   r.onsuccess = () => ok(r.result); r.onerror = () => ko(r.error); });"
        " }"
        " db.close();"
        " return out;"
        f"}})({json.dumps(name)})"
    )
