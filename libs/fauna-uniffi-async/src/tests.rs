//! The expansion of `#[fauna_uniffi_async::export]`, driven the way a foreign
//! executor drives it: `Runtime::block_on` polls the exported future on the
//! calling thread with the runtime context entered — exactly what UniFFI's
//! `async_runtime = "tokio"` gives a Swift cooperative-pool thread. The work
//! must run elsewhere (deterministic: a thread identity, no stack threshold).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};

#[derive(uniffi::Object)]
pub struct Probe {
    ran_on: Mutex<Option<ThreadId>>,
}

#[fauna_uniffi_async::export]
impl Probe {
    #[uniffi::constructor]
    pub async fn open(label: &str) -> Arc<Self> {
        assert_eq!(label, "probe");
        Arc::new(Self {
            ran_on: Mutex::new(Some(thread::current().id())),
        })
    }

    /// Synchronous items stay exported unchanged.
    pub fn ran_on(&self) -> Option<String> {
        self.ran_on.lock().unwrap().map(|id| format!("{id:?}"))
    }

    pub async fn work(&self) -> u32 {
        *self.ran_on.lock().unwrap() = Some(thread::current().id());
        tokio::task::yield_now().await;
        7
    }

    pub async fn echo(
        self: Arc<Self>,
        who: &str,
        bytes: &[u8],
        nick: Option<&str>,
        other: &Probe,
    ) -> String {
        let _ = other.ran_on();
        format!("{who}:{}:{}", bytes.len(), nick.unwrap_or("-"))
    }

    /// A record lent by reference is lifted owned (`T`, not `Arc<T>`).
    pub async fn tag(&self, label: &Label, maybe: Option<&Label>) -> String {
        format!("{}/{}", label.text, maybe.map_or("-", |l| l.text.as_str()))
    }

    #[uniffi::method(name = "renamed")]
    pub async fn named_differently(&self) -> u32 {
        1
    }

    pub async fn boom(&self) {
        panic!("boom from the worker");
    }

    pub async fn park(&self) {
        struct Flag;
        impl Drop for Flag {
            fn drop(&mut self) {
                PARK_DROPPED.store(true, Ordering::SeqCst);
            }
        }
        let _flag = Flag;
        std::future::pending::<()>().await;
    }
}

#[fauna_uniffi_async::export]
pub async fn free_work() -> String {
    format!("{:?}", thread::current().id())
}

/// A free function's attribute arguments reach `uniffi::export`.
#[fauna_uniffi_async::export(default(nick = None))]
pub async fn greet(nick: Option<String>) -> String {
    nick.unwrap_or_else(|| "-".to_owned())
}

#[derive(uniffi::Record)]
pub struct Label {
    text: String,
}

static PARK_DROPPED: AtomicBool = AtomicBool::new(false);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn probe() -> Arc<Probe> {
    Arc::new(Probe {
        ran_on: Mutex::new(None),
    })
}

#[test]
fn the_exported_twin_runs_the_method_on_a_runtime_worker() {
    let rt = runtime();
    let p = probe();
    let caller = thread::current().id();
    assert_eq!(rt.block_on(Arc::clone(&p).__uniffi_async_work()), 7);
    let ran_on = p.ran_on.lock().unwrap().expect("the method ran");
    assert_ne!(ran_on, caller, "the work was polled on the foreign thread");
}

#[test]
fn the_inline_method_keeps_running_on_its_caller() {
    let rt = runtime();
    let p = probe();
    let caller = thread::current().id();
    assert_eq!(rt.block_on(p.work()), 7);
    assert_eq!(p.ran_on.lock().unwrap().unwrap(), caller);
}

#[test]
fn constructors_and_free_functions_run_on_a_worker_too() {
    let rt = runtime();
    let caller = format!("{:?}", thread::current().id());
    let p = rt.block_on(Probe::__uniffi_async_open("probe".to_owned()));
    assert_ne!(p.ran_on().unwrap(), caller);
    assert_ne!(rt.block_on(__uniffi_async_free_work()), caller);
    assert_eq!(rt.block_on(__uniffi_async_greet(Some("a".to_owned()))), "a");
}

#[test]
fn borrowed_arguments_are_taken_owned_and_lent_to_the_method() {
    let rt = runtime();
    let p = probe();
    let out = rt.block_on(Arc::clone(&p).__uniffi_async_echo(
        "ada".to_owned(),
        vec![1, 2, 3],
        Some("a".to_owned()),
        probe(),
    ));
    assert_eq!(out, "ada:3:a");
    let out =
        rt.block_on(Arc::clone(&p).__uniffi_async_echo(String::new(), Vec::new(), None, probe()));
    assert_eq!(out, ":0:-");
    let label = |t: &str| Label { text: t.to_owned() };
    let out = rt.block_on(p.__uniffi_async_tag(label("a"), Some(label("b"))));
    assert_eq!(out, "a/b");
}

#[test]
fn an_explicit_foreign_name_is_kept() {
    let rt = runtime();
    assert_eq!(rt.block_on(probe().__uniffi_async_named_differently()), 1);
}

#[test]
fn a_panic_in_the_work_reaches_the_caller() {
    let rt = runtime();
    let p = probe();
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(p.__uniffi_async_boom())
    }))
    .expect_err("the panic is re-raised on the polling thread");
    assert_eq!(err.downcast_ref::<&str>(), Some(&"boom from the worker"));
}

#[test]
fn dropping_the_exported_future_aborts_the_work() {
    let rt = runtime();
    let call = probe().__uniffi_async_park();
    // Poll long enough for the task to start and park, then give up on it —
    // what a cancelled foreign Task does to the future.
    let timed_out = rt
        .block_on(async { tokio::time::timeout(std::time::Duration::from_millis(50), call).await });
    assert!(timed_out.is_err());
    // The abort lands at the task's next scheduling; wait for it boundedly.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !PARK_DROPPED.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "the spawned work outlived the dropped export"
        );
        thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Every UniFFI async export in the workspace goes through [`export`]: a bare
/// `uniffi::export(async_runtime = …)` would be polled on the foreign
/// executor's small stack again — the crash class this crate closes. A scan,
/// not a per-crate test, so an export added anywhere later is covered too.
#[test]
fn no_async_export_bypasses_the_attribute() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut bare = Vec::new();
    for top in ["libs", "bins", "apps"] {
        scan(&workspace.join(top), &mut bare);
    }
    assert!(
        bare.is_empty(),
        "these async exports bypass `#[fauna_uniffi_async::export]` — use it \
         instead of `uniffi::export(async_runtime = \"tokio\")` \
         (docs/goal/architecture/apps/native-async-execution.md § The rule):\n{}",
        bare.join("\n")
    );
}

fn scan(dir: &std::path::Path, bare: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            // Build output, vendored code, and this crate pair (whose expansion
            // is the one sanctioned spelling).
            if !matches!(
                &*name,
                "target"
                    | "node_modules"
                    | "vendor"
                    | "fauna-uniffi-async"
                    | "fauna-uniffi-async-macros"
            ) && !name.starts_with('.')
            {
                scan(&path, bare);
            }
        } else if name.ends_with(".rs") {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or_default();
                if code.contains("uniffi::export(async_runtime") {
                    bare.push(format!("{}:{}", path.display(), i + 1));
                }
            }
        }
    }
}
