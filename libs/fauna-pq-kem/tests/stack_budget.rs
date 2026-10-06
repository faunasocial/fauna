//! How much **stack** each public X-Wing / ML-KEM-768 entry point needs — measured,
//! then asserted, so a libcrux bump that grows the frame chain fails here instead of
//! killing a native app with no panic and no log.
//!
//! ## Why this test exists
//!
//! `docs/goal/architecture/apps/native-async-execution.md` § The second measured
//! incident: a `fauna-ffi` export polled on a Swift concurrency cooperative-pool
//! thread (**544 KB** stack, read from the crash report's own `vmRegionInfo`) died on
//! the guard page inside ML-KEM-768. The frame chain is **synchronous**, so the
//! doc's usual `Box::pin` remedy cannot reach it — there is no future to box — and
//! nothing had ever measured what these leaves actually cost.
//!
//! Until this test, the only number anyone had was ≈548 KB for one whole
//! export-to-leaf chain, inferred from a stack pointer 3456 bytes past a guard page.
//! That number could not answer the question every caller actually has: *how much
//! stack does the crypto itself want, independent of whatever sits above it?*
//!
//! ## How it measures
//!
//! A stack overflow aborts the process — it cannot be caught and asserted on
//! in-process. So each probe runs in a **child process** (this same test binary,
//! re-entered through two env vars) on a thread with an exact `stack_size`. The
//! child survives or dies on the guard page; the parent reads the exit status.
//! Bisecting that yields each leaf's true requirement.
//!
//! ## What it measured (2026-08-25, aarch64-apple-darwin, libcrux-ml-kem 0.0.8)
//!
//! | entry point                        | dev, unoptimized dep | dev, `opt-level = 2` | release |
//! |------------------------------------|---------------------:|---------------------:|--------:|
//! | `derive_mlkem768_keypair_from_ikm` |               227 KB |                32 KB |  ≤16 KB |
//! | `derive_keypair`                   |               243 KB |                32 KB |  ≤16 KB |
//! | `derive_keypair_from_ikm`          |               243 KB |                32 KB |  ≤16 KB |
//! | `encapsulate`                      |               259 KB |                32 KB |  ≤16 KB |
//! | `decapsulate`                      |               290 KB |                32 KB |  ≤16 KB |
//!
//! (≤16 KB is macOS's minimum thread stack — the release figure is at the floor the
//! platform will allocate, not a measurement of the leaf.)
//!
//! **The optimization level of the dependency, not the call site, is what decided
//! the crash.** A shipped app was never near the guard page: 16 KB against Swift's
//! 544 KB. `just mac-debug` / `just swift-test` build `fauna-ffi` unoptimized, so a
//! journey run put 290 KB of crypto under whatever chain the export already had —
//! and the 2026-08-23 report's stack pointer sat 3456 bytes past the guard page.
//! The workspace now pins `libcrux-ml-kem` to `opt-level = 2` in `dev`
//! (root `Cargo.toml`), which is what holds the middle column at 32 KB.
//!
//! ## What `BUDGET` asserts
//!
//! Each leaf must run inside `BUDGET` in whatever profile the test itself is built
//! for. The margin over the 32 KB measurement is deliberate: other targets' codegen
//! differs, and this test runs on every dev machine. It is still a real ratchet —
//! **drop the `Cargo.toml` override and this test goes red**, because the leaves
//! jump back to 227–290 KB. That is the point: the override is load-bearing for
//! app stability, and nothing else would notice if it were deleted.
//!
//! A red here is not a flake. It means the crypto's stack appetite moved, and every
//! foreign-executor thread in the fleet — Swift's 544 KB cooperative pool being the
//! smallest known — just got closer to the guard page.

use std::env;
use std::process::Command;
use std::thread;

use fauna_pq_kem::{
    decapsulate, derive_keypair, derive_keypair_from_ikm, derive_mlkem768_keypair_from_ikm,
    encapsulate,
};
use rand_core::RngCore;

const ENV_OP: &str = "FAUNA_PQ_STACK_PROBE_OP";
const ENV_BYTES: &str = "FAUNA_PQ_STACK_PROBE_BYTES";

/// 4x the 32 KB every entry point measured at, and well under the 227 KB the
/// cheapest of them costs with the `Cargo.toml` optimization override removed.
const BUDGET: usize = 128 * 1024;

/// The five public entry points.
///
/// Keep this list identical to `fauna-pq-kem`'s public surface: a new entry point
/// with no row here is a leaf nobody has measured, which is exactly the state that
/// produced the 2026-08-23 crash.
const LEAVES: &[&str] = &[
    "derive_mlkem768_keypair_from_ikm",
    "derive_keypair",
    "derive_keypair_from_ikm",
    "encapsulate",
    "decapsulate",
];

/// A deterministic RNG so a probe run is reproducible; `encapsulate` only needs
/// `RngCore + CryptoRng`, and what it consumes is 64 bytes of coins.
struct FixedRng(u64);

impl RngCore for FixedRng {
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.0 >> 32) as u32
    }
    fn next_u64(&mut self) -> u64 {
        u64::from(self.next_u32()) << 32 | u64::from(self.next_u32())
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let v = self.next_u32().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }
}

impl rand_core::CryptoRng for FixedRng {}

/// Inputs an op needs, built on the child's *main* thread so the sized probe thread
/// measures the operation under test and nothing else. Without this split,
/// `decapsulate`'s number would really be "the deepest of derive, encapsulate and
/// decapsulate run back to back", which is not what any caller wants to know.
struct Inputs {
    keypair: fauna_pq_kem::XWingKeyPair,
    ciphertext: fauna_pq_kem::XWingCiphertext,
}

fn build_inputs() -> Box<Inputs> {
    let keypair = derive_keypair(&[0x5au8; 32], "fauna.test.stack-probe.v1", &[0x11u8; 32]);
    let mut rng = FixedRng(0x1234_5678_9abc_def0);
    let (ciphertext, _) = encapsulate(&keypair.public, &mut rng).expect("probe input encapsulate");
    Box::new(Inputs {
        keypair,
        ciphertext,
    })
}

/// Run one leaf, and only that leaf.
fn run_op(op: &str, inputs: &Inputs) {
    let ikm = [0x5au8; 32];
    let x25519_secret = [0x11u8; 32];
    match op {
        "derive_mlkem768_keypair_from_ikm" => {
            let (dk, ek) = derive_mlkem768_keypair_from_ikm(&ikm, "fauna.test.stack-probe.v1");
            std::hint::black_box((dk[0], ek[0]));
        }
        "derive_keypair" => {
            let kp = derive_keypair(&ikm, "fauna.test.stack-probe.v1", &x25519_secret);
            std::hint::black_box(kp.public.mlkem_encaps_key()[0]);
        }
        "derive_keypair_from_ikm" => {
            let kp = derive_keypair_from_ikm(
                &ikm,
                "fauna.test.stack-probe.v1",
                "fauna.test.stack-probe.x25519.v1",
            );
            std::hint::black_box(kp.public.mlkem_encaps_key()[0]);
        }
        "encapsulate" => {
            let mut rng = FixedRng(0x1234_5678_9abc_def0);
            let (ct, ss) = encapsulate(&inputs.keypair.public, &mut rng).expect("encapsulate");
            std::hint::black_box((ct.as_bytes()[0], ss[0]));
        }
        "decapsulate" => {
            let ss = decapsulate(&inputs.keypair.secret, &inputs.ciphertext);
            std::hint::black_box(ss[0]);
        }
        other => panic!("unknown probe op {other}"),
    }
}

/// True when the op completes on a thread with exactly `bytes` of stack.
///
/// The child dies on the guard page when it does not, so this is a process exit
/// status, never a caught error.
fn survives(op: &str, bytes: usize) -> bool {
    let exe = env::current_exe().expect("test binary path");
    let status = Command::new(exe)
        .args([
            "--exact",
            "each_pq_leaf_runs_inside_its_measured_stack_budget",
        ])
        .env(ENV_OP, op)
        .env(ENV_BYTES, bytes.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("spawn stack probe child");
    status.success()
}

#[test]
fn each_pq_leaf_runs_inside_its_measured_stack_budget() {
    // Child half: run the named op on a thread of the named size and exit. A
    // guard-page hit here is the measurement, so nothing is caught.
    if let Ok(op) = env::var(ENV_OP) {
        let bytes: usize = env::var(ENV_BYTES)
            .expect("probe bytes")
            .parse()
            .expect("probe bytes parse");
        let inputs = build_inputs();
        thread::Builder::new()
            .stack_size(bytes)
            .spawn(move || run_op(&op, &inputs))
            .expect("spawn probe thread")
            .join()
            .expect("probe thread");
        return;
    }

    for op in LEAVES {
        assert!(
            survives(op, BUDGET),
            "{op} no longer runs inside {} KB of stack (it measured 32 KB). Every \
             foreign-executor thread that reaches it just got closer to its guard page \
             — Swift's cooperative pool (544 KB, the smallest known) first, where this \
             is a crash with no Rust panic and nothing on any sink.\n\
             FIRST SUSPECT: the root Cargo.toml's `[profile.dev.package.libcrux-ml-kem] \
             opt-level = 2` override. Without it these leaves need 227-290 KB, and \
             `just mac-debug` / `just swift-test` build fauna-ffi in that profile.\n\
             Otherwise the crypto's own frames grew: re-measure by bisecting \
             {ENV_BYTES} (this test's module docs describe it) and fix the caller side \
             before raising the budget. Owner: \
             docs/goal/architecture/apps/native-async-execution.md § The rule.",
            BUDGET / 1024,
        );
    }
}
