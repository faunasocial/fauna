//! `fauna-e2e-stall-capture --pid <pid> --tid <tid>`: print what holds one
//! thread of another process — the windows app's UI thread, for the FlaUI
//! bridge.
//!
//! The bridge judges a stall of the windows app from outside (the app is a C#
//! process that runs no agent of ours), and on the app-side verdict alone it
//! runs this binary and appends what it prints to the failing op's error
//! (`e2e-conventions.md` convention 11). The capture itself is
//! `fauna_e2e_agent::capture_thread_of_process`, the same one tui's agent
//! uses on its own UI thread, so the walk is never written twice.
//!
//! Test-only: built by the e2e helper-binary registry
//! (`tests/e2e-unified/helpers/helper_bins.py`), never shipped. A build without
//! the agent surface (a plain `--release`) compiles to a refusal.
//!
//! Exit 0 with the report on stdout whenever the arguments parse — a capture
//! that could not be taken says why in the report, which is what the bridge
//! shows. Exit 2 on bad arguments or a build without the surface.

fn parse(args: &[String]) -> Option<(u32, u32)> {
    let mut pid = None;
    let mut tid = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next()?.parse::<u32>().ok()?;
        match flag.as_str() {
            "--pid" => pid = Some(value),
            "--tid" => tid = Some(value),
            _ => return None,
        }
    }
    Some((pid?, tid?))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((pid, tid)) = parse(&args) else {
        eprintln!("usage: fauna-e2e-stall-capture --pid <pid> --tid <tid>");
        std::process::exit(2);
    };
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    {
        println!("{}", fauna_e2e_agent::capture_thread_of_process(pid, tid));
    }
    #[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
    {
        let _ = (pid, tid);
        eprintln!("the stall capture is compiled out of this build (convention 15)");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn both_ids_parse_in_either_order() {
        assert_eq!(
            parse(&args(&["--pid", "12", "--tid", "34"])),
            Some((12, 34))
        );
        assert_eq!(
            parse(&args(&["--tid", "34", "--pid", "12"])),
            Some((12, 34))
        );
    }

    #[test]
    fn a_missing_or_malformed_id_is_refused() {
        assert_eq!(parse(&args(&["--pid", "12"])), None);
        assert_eq!(parse(&args(&["--pid", "x", "--tid", "1"])), None);
        assert_eq!(parse(&args(&["--pid", "1", "--tid"])), None);
        assert_eq!(parse(&args(&["--who", "1", "--tid", "1"])), None);
    }
}
