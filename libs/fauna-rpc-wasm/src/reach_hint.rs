//! The page's **reach table**: the wasm twin of
//! `fauna_ws_substrate::reach_hint`, the session connector's half of the reach
//! hint's dial rule (`docs/goal/behavior/onboarding-provisioning.md` § Reach hint
//! → *The web session's half*).
//!
//! The launch machine dials a freshly-provisioned box by its reach hint when
//! the account's domain does not resolve yet. This table carries that decision
//! to the session: the launch installs the hint only when it carried the launch
//! to `Success`, and clears the domain's entry at the start of every launch
//! phase; the reconnect loop and the bearer mint read it after a failed domain
//! dial; the first bearer socket open over the domain spends it.
//!
//! **Why a page global and not a Rust static.** Native's table is one per
//! process because the launch machine and the session share a process. Here
//! they do not share a linear memory: the machine runs in the launch chunk and
//! the session client in the core chunk, each its own instantiated module
//! (`shared_port.rs` has the same reason), so a static would be two tables and
//! the session would read an empty one. So the table is a JS `Map` on
//! `globalThis`, reached only through this module, which is compiled into both
//! chunks: one page, one table, one definition of its shape. It dies with the
//! page, and a reload re-decides it from the persisted slot.
//!
//! **The hint is stored bare and the URL built per dial**, so a client whose
//! `nest_url` moved to another port (the SRV self-heal) dials the hint there.
//! The key is the domain, lowercased, port-free, exactly as natively
//! ([`fauna_core::web::reach_host_key`]).
//!
//! **Trust: one pinned proof per bearer.** A browser cannot pin a bearer
//! socket's TLS key, so unlike the native trusted dial nothing refuses a stale
//! hint at the session socket's own handshake (`onboarding-provisioning.md`
//! § Reach hint → *The web session's half* → *Trust*). In its place each entry
//! carries a one-shot **proof**: ARMED by a pinned ceremony over the hint on
//! this page ([`install`], which the launch calls only after its challenge over
//! the hint reached `Success`, and [`prove`], which the mint calls after its
//! own ceremony over the hint succeeded), SPENT by the first bearer dial over
//! the hint ([`spend_bearer_dial`]), and LAPSED on its own after
//! [`PROOF_LAPSE`]. An entry with no live proof is no fallback for a bearer, so
//! the window a stale hint leaves is the seconds between a ceremony and the one
//! bearer it vouches for, whatever the reconnect loop's control flow does.
//! The mint reads the entry without a proof ([`hint_dial`]): its ceremony is
//! the pin check, judged on the domain before anything is signed.

use std::cell::OnceCell;
use std::time::Duration;

use fauna_core::web::ReachDial;
use fauna_protocol::reconnect::ESTABLISH_PROBE;
use js_sys::{Array, Function, Map};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// The one `globalThis` property the table lives under.
const TABLE_GLOBAL: &str = "__faunaReachTable";

/// How long a proof stays live once armed. The mint and the bearer dial it
/// feeds are one attempt, and a hung domain dial costs one establish probe
/// before the hint turn, so a few probes cover the attempt with room for the
/// page's own scheduling; a proof older than that vouches for nothing.
pub const PROOF_LAPSE: Duration = Duration::from_secs(ESTABLISH_PROBE.as_secs() * 5);

/// The entry's shape, the one definition both chunks compile: `[reach_ipv4,
/// deleter, proof]`, `proof` the `Date.now()` milliseconds it lapses at, or
/// `undefined` once spent.
const ENTRY_HINT: u32 = 0;
const ENTRY_DELETER: u32 = 1;
const ENTRY_PROOF: u32 = 2;

fn proof_deadline() -> JsValue {
    JsValue::from_f64(js_sys::Date::now() + PROOF_LAPSE.as_millis() as f64)
}

fn entry_of(nest_url: &str) -> Option<(String, Array)> {
    let host = fauna_core::web::reach_host_key(nest_url)?;
    let entry = table().get(&JsValue::from_str(&host)).dyn_into().ok()?;
    Some((host, entry))
}

fn table() -> Map {
    thread_local! {
        static TABLE: OnceCell<Map> = const { OnceCell::new() };
    }
    TABLE.with(|cached| {
        cached
            .get_or_init(|| {
                let global = js_sys::global();
                let key = JsValue::from_str(TABLE_GLOBAL);
                if let Some(map) = js_sys::Reflect::get(&global, &key)
                    .ok()
                    .and_then(|v| v.dyn_into::<Map>().ok())
                {
                    return map;
                }
                let map = Map::new();
                // `Reflect::set` on the page's global object cannot fail.
                let _ = js_sys::Reflect::set(&global, &key, &map);
                map
            })
            .clone()
    })
}

/// Install `reach_ipv4` as the reach hint for `nest_url`'s domain, replacing any
/// entry it had, its proof armed: the caller has just run a pinned ceremony
/// over this address (the launch's silent challenge, on `Success`).
/// `on_domain_reached` runs once, when a bearer socket opens over the domain
/// ([`domain_reached`]); it may belong to another chunk than the one that runs
/// it, which is why it crosses as a JS function. A hint that names no dial
/// (`fauna_core::web::reach_authority_url`) installs nothing.
pub fn install(nest_url: &str, reach_ipv4: &str, on_domain_reached: impl FnOnce() + 'static) {
    let Some(host) = fauna_core::web::reach_host_key(nest_url) else {
        return;
    };
    if fauna_core::web::reach_authority_url(nest_url, reach_ipv4).is_none() {
        return;
    }
    let entry = Array::of3(
        &JsValue::from_str(reach_ipv4),
        &Closure::once_into_js(on_domain_reached),
        &proof_deadline(),
    );
    table().set(&JsValue::from_str(&host), &entry);
}

/// Forget the domain's hint without running its deleter: the launch phase's
/// opening move, so its own domain dial takes no fallback from here.
pub fn clear(nest_url: &str) {
    if let Some(host) = fauna_core::web::reach_host_key(nest_url) {
        table().delete(&JsValue::from_str(&host));
    }
}

/// The dial `nest_url` falls back to after failing to reach anything, proof or
/// none: `nest_url`'s authority moved to the hint's address, port and scheme
/// kept, the pin key the domain. For the mint, whose ceremony over it is the
/// pin check, and for "is there a hint at all". A bearer never rides on this:
/// [`spend_bearer_dial`].
pub fn hint_dial(nest_url: &str) -> Option<ReachDial> {
    let (_, entry) = entry_of(nest_url)?;
    ReachDial::over_hint(nest_url, &entry.get(ENTRY_HINT).as_string()?)
}

/// Arm the entry's proof: a pinned ceremony over `dial` just succeeded on this
/// page (the mint's hint fallback). Arms only the address the ceremony went
/// over: an entry the launch replaced or cleared meanwhile is left as it is.
pub fn prove(dial: &ReachDial) {
    if !dial.is_over_hint() {
        return;
    }
    let Some((_, entry)) = entry_of(dial.pin_key()) else {
        return;
    };
    let current = entry
        .get(ENTRY_HINT)
        .as_string()
        .and_then(|ip| ReachDial::over_hint(dial.pin_key(), &ip));
    if current.as_ref() == Some(dial) {
        entry.set(ENTRY_PROOF, proof_deadline());
    }
}

/// The dial a BEARER may take over the hint: `Some` only while the entry holds
/// a live proof, which this consumes, so one pinned ceremony vouches for one
/// bearer dial. `None` for no entry, a spent proof or a lapsed one, and the
/// caller then has no fallback until a ceremony re-arms it.
pub fn spend_bearer_dial(nest_url: &str) -> Option<ReachDial> {
    let (_, entry) = entry_of(nest_url)?;
    let deadline = entry.get(ENTRY_PROOF).as_f64();
    entry.set(ENTRY_PROOF, JsValue::UNDEFINED);
    if !deadline.is_some_and(|d| js_sys::Date::now() < d) {
        return None;
    }
    ReachDial::over_hint(nest_url, &entry.get(ENTRY_HINT).as_string()?)
}

/// Age the entry's proof past its lapse, as [`PROOF_LAPSE`] elapsing would.
#[cfg(any(test, feature = "test-helpers"))]
pub fn lapse_proof_for_test(nest_url: &str) {
    if let Some((_, entry)) = entry_of(nest_url)
        && !entry.get(ENTRY_PROOF).is_undefined()
    {
        entry.set(ENTRY_PROOF, JsValue::from_f64(js_sys::Date::now() - 1.0));
    }
}

/// A bearer socket opened over `nest_url`'s domain itself: the hint is spent.
/// Removes the entry and runs its deleter. A domain with no entry is the common
/// case and costs a map lookup.
pub fn domain_reached(nest_url: &str) {
    let Some((host, entry)) = entry_of(nest_url) else {
        return;
    };
    table().delete(&JsValue::from_str(&host));
    tracing::info!(
        host,
        "the domain reached the nest; the reach hint is deleted"
    );
    if let Ok(deleter) = entry.get(ENTRY_DELETER).dyn_into::<Function>()
        && let Err(e) = deleter.call0(&JsValue::NULL)
    {
        tracing::warn!(error = ?e, "the reach hint's deleter threw");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// A chunk reading the table holds none of this crate's statics, so the
    /// reader here is JS over the page global, as the other chunk's copy of this
    /// module is in effect: it finds the entry and can run the deleter.
    #[wasm_bindgen(inline_js = r#"
        export function page_table_hint(name, host) {
            const entry = globalThis[name]?.get(host);
            return entry ? entry[0] : undefined;
        }
        export function run_page_table_deleter(name, host) {
            globalThis[name].get(host)[1]();
        }
    "#)]
    extern "C" {
        fn page_table_hint(name: &str, host: &str) -> Option<String>;
        fn run_page_table_deleter(name: &str, host: &str);
    }

    fn counter() -> (Rc<Cell<u32>>, impl FnOnce() + 'static) {
        let runs = Rc::new(Cell::new(0));
        let bump = Rc::clone(&runs);
        (runs, move || bump.set(bump.get() + 1))
    }

    /// **The launch chunk's write reaches the core chunk.** This is the property
    /// a Rust static would have lacked, and the deleter crossing with it is what
    /// lets the core chunk delete a slot through the launch chunk's persistence.
    #[wasm_bindgen_test]
    fn the_table_lives_on_the_page_not_in_the_chunk() {
        let (runs, deleter) = counter();
        install("https://Cross.Chunk.Example:8443", "203.0.113.7", deleter);
        assert_eq!(
            page_table_hint(TABLE_GLOBAL, "cross.chunk.example").as_deref(),
            Some("203.0.113.7"),
            "keyed on the domain, lowercased and port-free"
        );
        run_page_table_deleter(TABLE_GLOBAL, "cross.chunk.example");
        assert_eq!(runs.get(), 1);
        clear("https://cross.chunk.example");
    }

    fn hint_url(nest_url: &str) -> Option<String> {
        hint_dial(nest_url).map(|d| d.dial_url().to_owned())
    }

    #[wasm_bindgen_test]
    fn the_hint_url_keeps_the_dialled_port_and_clear_runs_no_deleter() {
        let (runs, deleter) = counter();
        install("https://kept.example", "203.0.113.8", deleter);
        assert_eq!(
            hint_url("https://kept.example:8443").as_deref(),
            Some("https://203.0.113.8:8443"),
            "the port on a dial is the dial's own"
        );
        assert_eq!(
            hint_dial("https://kept.example:8443").map(|d| d.pin_key().to_owned()),
            Some("https://kept.example:8443".to_owned()),
            "the hint's dial is judged on the domain"
        );
        assert_eq!(hint_url("https://bystander.example"), None);
        clear("https://KEPT.example");
        assert_eq!(hint_url("https://kept.example"), None);
        assert_eq!(runs.get(), 0, "clearing forgets; only the domain spends");
    }

    #[wasm_bindgen_test]
    fn a_malformed_hint_installs_nothing() {
        let (_runs, deleter) = counter();
        install("https://malformed.example", "not-an-ip", deleter);
        assert_eq!(hint_url("https://malformed.example"), None);
    }

    #[wasm_bindgen_test]
    fn the_domain_spends_a_hint_once() {
        let (runs, deleter) = counter();
        install("https://spent.example", "203.0.113.9", deleter);
        domain_reached("https://spent.example:443");
        domain_reached("https://spent.example");
        assert_eq!(runs.get(), 1);
        assert_eq!(hint_url("https://spent.example"), None);
    }

    /// **The launch's install arms one bearer dial, and only one.** The second
    /// read finds the entry still there for the mint, but no proof for a
    /// bearer; spending touches neither the hint nor its deleter.
    #[wasm_bindgen_test]
    fn install_arms_a_proof_that_one_bearer_dial_spends() {
        let (runs, deleter) = counter();
        install("https://armed.example", "203.0.113.10", deleter);
        let dial = spend_bearer_dial("https://armed.example:8443")
            .expect("the launch's ceremony vouches for one bearer dial");
        assert_eq!(dial.dial_url(), "https://203.0.113.10:8443");
        assert_eq!(dial.pin_key(), "https://armed.example:8443");
        assert_eq!(spend_bearer_dial("https://armed.example"), None, "spent");
        assert!(
            hint_url("https://armed.example").is_some(),
            "the hint stays"
        );
        assert_eq!(runs.get(), 0);
        clear("https://armed.example");
    }

    /// **The mint's ceremony over the hint re-arms it** — over the address the
    /// ceremony went to, and no other: a proof for a replaced entry's old
    /// address, or for the domain's own dial, arms nothing.
    #[wasm_bindgen_test]
    fn prove_re_arms_only_the_address_the_ceremony_went_over() {
        let (_runs, deleter) = counter();
        install("https://reproved.example", "203.0.113.11", deleter);
        let _ = spend_bearer_dial("https://reproved.example");

        prove(&ReachDial::domain("https://reproved.example"));
        assert_eq!(spend_bearer_dial("https://reproved.example"), None);
        let stale = ReachDial::over_hint("https://reproved.example", "203.0.113.99").unwrap();
        prove(&stale);
        assert_eq!(spend_bearer_dial("https://reproved.example"), None);

        let dial = hint_dial("https://reproved.example").expect("the entry stands");
        prove(&dial);
        assert_eq!(spend_bearer_dial("https://reproved.example"), Some(dial));
        assert_eq!(spend_bearer_dial("https://reproved.example"), None);

        clear("https://reproved.example");
        prove(&ReachDial::over_hint("https://reproved.example", "203.0.113.11").unwrap());
        assert_eq!(hint_url("https://reproved.example"), None, "nothing to arm");
    }

    /// **A proof lapses on its own**, whatever reads it afterwards.
    #[wasm_bindgen_test]
    fn a_lapsed_proof_vouches_for_nothing() {
        let (_runs, deleter) = counter();
        install("https://lapsed.example", "203.0.113.12", deleter);
        lapse_proof_for_test("https://lapsed.example");
        assert_eq!(spend_bearer_dial("https://lapsed.example"), None);
        assert!(hint_url("https://lapsed.example").is_some());
        clear("https://lapsed.example");
    }
}
