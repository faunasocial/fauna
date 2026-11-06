//! `fauna-reports-pass` — the admin's headless reports pass
//! (`docs/goal/architecture/org-nests.md` § fauna.build → *Disposable*).
//!
//! ```text
//! fauna-reports-pass --nest <https-url> list
//! fauna-reports-pass --nest <https-url> resolve <report-id> <acted|dismissed>
//! ```
//!
//! The admin's Ed25519 seed is read from stdin (one line of 64 hex digits),
//! never from the command line. It signs in over the silent challenge, opens one
//! authed WS-RPC connection, makes the one call, and exits. Whether it may run
//! against a guarded box at all is the caller's decision
//! (`tests/e2e-unified/helpers/staging_box.py::admit_complex_connection`, the
//! reports-pass exemption); this binary only keeps to the bounds.
//!
//! Exit codes: 0 done, 1 the nest refused or could not be reached, 2 usage.

use std::io::Read;
use std::process::ExitCode;

use ed25519_dalek::SigningKey;
use fauna_anon_client::{TokenNestClient, mint_bearer_over_silent_challenge};
use fauna_client_admin::AdminClient;
use fauna_reports_pass::{parse_outcome, parse_seed, render_entry, untrusted, valid_report_id};

const USAGE: &str = "usage: fauna-reports-pass --nest <url> list\n       \
                     fauna-reports-pass --nest <url> resolve <report-id> <acted|dismissed>\n\
                     The admin's seed is read from stdin (64 hex digits).";

enum Verb {
    List,
    Resolve(String, fauna_client_admin::moderation::AbuseReportOutcome),
}

fn parse_args(args: &[String]) -> Result<(String, Verb), String> {
    match args {
        [flag, nest, verb, rest @ ..] if flag == "--nest" => {
            if !(nest.starts_with("https://") || nest.starts_with("http://")) {
                return Err(format!("--nest takes an http(s):// URL, not {nest:?}"));
            }
            let verb = match (verb.as_str(), rest) {
                ("list", []) => Verb::List,
                ("resolve", [id, outcome]) => {
                    if !valid_report_id(id) {
                        return Err(format!("{id:?} is not a report id (hex)"));
                    }
                    let outcome = parse_outcome(outcome)
                        .ok_or_else(|| format!("outcome {outcome:?} is not acted or dismissed"))?;
                    Verb::Resolve(id.to_ascii_lowercase(), outcome)
                }
                _ => return Err(USAGE.to_string()),
            };
            Ok((nest.trim_end_matches('/').to_string(), verb))
        }
        _ => Err(USAGE.to_string()),
    }
}

async fn run(nest: &str, verb: Verb, key: SigningKey) -> Result<(), String> {
    let actor_hex = hex::encode(key.verifying_key().to_bytes());
    let minted = mint_bearer_over_silent_challenge(nest, &key)
        .await
        .map_err(|e| format!("sign-in refused: {}", untrusted(&e.to_string(), 500)))?;
    let conn = TokenNestClient::connect(nest, &actor_hex, &minted.token)
        .await
        .map_err(|e| format!("connection refused: {}", untrusted(&e.to_string(), 500)))?;
    let admin = AdminClient::new(conn);
    match verb {
        Verb::List => {
            let reply = admin
                .abuse_report_queue()
                .await
                .map_err(|e| format!("listing refused: {}", untrusted(&e.to_string(), 500)))?;
            for entry in &reply.reports {
                println!("{}", render_entry(entry));
            }
            println!("open reports: {}", reply.reports.len());
        }
        Verb::Resolve(id, outcome) => {
            admin
                .abuse_report_resolve(id.clone(), outcome)
                .await
                .map_err(|e| format!("resolve refused: {}", untrusted(&e.to_string(), 500)))?;
            println!("resolved {id}: {}", outcome.token());
        }
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (nest, verb) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(why) => {
            eprintln!("{why}");
            return ExitCode::from(2);
        }
    };
    let mut input = String::new();
    if std::io::stdin()
        .take(4096)
        .read_to_string(&mut input)
        .is_err()
    {
        eprintln!("cannot read the seed from stdin");
        return ExitCode::from(2);
    }
    let seed = match parse_seed(&input) {
        Ok(seed) => seed,
        Err(why) => {
            eprintln!("{why}");
            return ExitCode::from(2);
        }
    };
    match run(&nest, verb, SigningKey::from_bytes(&seed)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("fauna-reports-pass: {why}");
            ExitCode::from(1)
        }
    }
}
