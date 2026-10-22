//! `fauna-cli` — the cli app's **device shell** (`docs/goal/architecture/apps/cli.md`
//! § The device shell): Fauna operated by an LLM host that launches a local
//! MCP server, or by a human or a script at the terminal.
//!
//! `fauna-cli mcp` serves MCP over stdio; `fauna-cli mcp --listen` serves it as
//! streamable HTTP on a free loopback port and prints the URL once on stdout
//! for the launching host. They are IPC wiring the host's server configuration
//! sets (bucket 1 of the configuration invariant), never a preference — no nest
//! URL, no account, no token on the command line or in a file the user edits;
//! the account comes from the consent ceremony (`connect`).
//!
//! Every other command line is the **`argv` shell** (§ One catalog, many
//! front-ends → *The `argv` shell*): `fauna-cli <noun> <verb> [--flag …]`,
//! derived from the catalog by [`fauna_cli::argv`], over the same [`DeviceHost`]
//! — the same principal, the same credential namespace, the same arms.
//! `fauna-cli connect <handle>` prints the code for *Connect an app* and waits
//! for the approval, so the next command finds the stored connection.
//!
//! Over stdio, stdout carries nothing but MCP messages; every log line goes to
//! stderr.

mod host;

use std::io::{IsTerminal, Write};
use std::time::Duration;

use fauna_cli::argv::{self, EXIT_OK, EXIT_TOOL_ERROR, EXIT_USAGE, Parsed, Terminal};
use fauna_cli::{Host, SessionUnavailable};
use host::DeviceHost;
use tokio::io::BufReader;

/// How often `connect` looks whether the background poll has settled — the
/// shell's own state, never the nest (the poll keeps the nest's interval).
const SETTLE_CHECK: Duration = Duration::from_millis(500);

fn runtime() -> tokio::runtime::Runtime {
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fauna-cli: could not start: {e}");
            std::process::exit(EXIT_TOOL_ERROR)
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let code = match args.first().map(String::as_str) {
        Some("mcp") => serve_mcp(&args[1..]),
        _ => run_argv(&args),
    };
    std::process::exit(code)
}

fn serve_mcp(flags: &[String]) -> i32 {
    let listen = match flags {
        [] => false,
        [flag] if flag == "--listen" => true,
        _ => {
            eprintln!(
                "usage: fauna-cli mcp [--listen]\n\nServes Fauna's tool catalog over MCP on stdin/stdout, for an LLM host to launch;\n--listen serves it as streamable HTTP on a free loopback port instead and prints its URL."
            );
            return EXIT_USAGE;
        }
    };
    let served = runtime().block_on(async {
        let server = fauna_cli::Server::new(DeviceHost::new());
        if listen {
            let listener = fauna_cli::http::bind().await?;
            println!("{}", fauna_cli::http::url(&listener)?);
            return fauna_cli::http::serve(&server, listener).await;
        }
        fauna_cli::stdio::serve(
            &server,
            BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
        )
        .await
    });
    match served {
        Ok(()) => EXIT_OK,
        Err(e) => {
            eprintln!("fauna-cli: the host's pipe failed: {e}");
            EXIT_TOOL_ERROR
        }
    }
}

/// The destructive command's question, on stderr, answered on stdin.
fn ask(question: &str) -> bool {
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn run_argv(args: &[String]) -> i32 {
    let inv = match argv::parse(args) {
        Ok(Parsed::Call(inv)) => inv,
        Ok(Parsed::Help(tool)) => {
            print!("{}", argv::help(tool));
            return EXIT_OK;
        }
        Ok(Parsed::Overview { asked }) => {
            if asked {
                print!("{}", argv::overview());
                return EXIT_OK;
            }
            eprint!("{}", argv::overview());
            return EXIT_USAGE;
        }
        Err(e) => {
            eprintln!("fauna-cli: {e}\n(`fauna-cli --help` lists the commands)");
            return EXIT_USAGE;
        }
    };
    let terminal = Terminal {
        stdout_tty: std::io::stdout().is_terminal(),
        stdin_tty: std::io::stdin().is_terminal(),
    };
    runtime().block_on(async {
        let host = DeviceHost::new();
        let out = argv::execute(&host, &inv, terminal, ask).await;
        print!("{}", out.stdout);
        eprint!("{}", out.stderr);
        let _ = std::io::stdout().flush();
        if out.code != EXIT_OK || inv.tool.id != fauna_cli::ToolId::Connect {
            return out.code;
        }
        let handle = inv.arguments["handle"].as_str().unwrap_or_default();
        await_approval(&host, handle).await
    })
}

/// `connect`: the code is printed; stay until the human has approved it (or
/// declined, or it expired), so the connection the background poll dials is
/// stored for the next command.
async fn await_approval(host: &DeviceHost, handle: &str) -> i32 {
    // public-text: cli-answer-strings-public
    eprintln!(
        "Type the code into Connect an app in one of your own Fauna apps and approve it. Waiting…"
    );
    // public-text-end: cli-answer-strings-public
    loop {
        match host.session(Some(handle)).await {
            Ok(_) => {
                eprintln!("Connected.");
                return EXIT_OK;
            }
            Err(SessionUnavailable::Pending { .. }) => tokio::time::sleep(SETTLE_CHECK).await,
            Err(why) => {
                eprintln!("{}", fauna_cli::ToolError::from(why).text());
                return EXIT_TOOL_ERROR;
            }
        }
    }
}
