//! Run the dev fleet's leftover test-root sweep once (`fauna_cfapi::test_roots`)
//! and print one `cfapi test-root sweep:` line — what `just cfapi-test-root-sweep`
//! runs for the win merge-gate check's pass. The one argument is how many
//! seconds to wait for a live test's lock (default 10). Exit 1 only when a test
//! root's unregister failed or the registry could not be read; a held lock is a
//! skip, not a failure.

#[cfg(all(windows, any(debug_assertions, feature = "test-helpers")))]
fn main() -> std::process::ExitCode {
    use fauna_cfapi::test_roots::{Sweep, sweep_leftover_test_roots};

    let wait = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10);
    match sweep_leftover_test_roots(std::time::Duration::from_secs(wait)) {
        Ok(sweep) => {
            println!("cfapi test-root sweep: {sweep}");
            match sweep {
                Sweep::Swept(r) if !r.failed.is_empty() => std::process::ExitCode::FAILURE,
                _ => std::process::ExitCode::SUCCESS,
            }
        }
        Err(e) => {
            println!("cfapi test-root sweep: error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(all(windows, any(debug_assertions, feature = "test-helpers"))))]
fn main() {
    println!("cfapi test-root sweep: skipped — windows debug builds only");
}
