//! The **`argv` shell** — the same catalog at a terminal, for a human or a
//! script (`apps/cli.md` § One catalog, many front-ends → *The `argv` shell*).
//!
//! The command tree is **derived** from [`CATALOG`], never authored beside it:
//! the tool `post_create` is the command `post create` (the name's first
//! segment the noun, the rest the verb), each property of its input schema a
//! `--kebab-case` flag whose help is the schema's description, the tool's
//! description the command's `--help`. A tool's required properties may also
//! be given positionally, in the schema's order — which is what makes
//! `connect <handle>` a derived command rather than a special case. A command
//! the catalog has no tool for does not exist.
//!
//! This module is the parser, the help, the terminal rendering and the run
//! (exit code, output choice, the destructive-tool confirmation); the shell
//! that owns a real terminal supplies [`Terminal`] and the question's answer,
//! and runs [`execute`] over its own [`Host`] — the same principal, the same
//! arms, the same ceiling as its MCP shell.

use serde_json::{Map, Value};

use crate::catalog::{CATALOG, Tool};
use crate::tools::{self, Host};

/// The command ran and the tool answered.
pub const EXIT_OK: i32 = 0;
/// The tool answered an error (the manager's, verbatim on stderr) — or the
/// human declined a destructive command when asked.
pub const EXIT_TOOL_ERROR: i32 = 1;
/// The command line was wrong, or a destructive command was run off a
/// terminal without `--yes`.
pub const EXIT_USAGE: i32 = 2;

/// The flags every command takes beside its schema's — no catalog property
/// may carry one of these names.
pub const GLOBAL_FLAGS: &[&str] = &["json", "yes", "help"];

/// A wrong command line: the message, for stderr, with [`EXIT_USAGE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One command line, resolved to a tool call.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub tool: &'static Tool,
    /// The tool's input, exactly as an MCP host would send it.
    pub arguments: Value,
    /// `--json`: the result's JSON even on a terminal.
    pub json: bool,
    /// `--yes`: run a destructive command without asking.
    pub yes: bool,
}

/// What a command line asks for.
#[derive(Debug, Clone)]
pub enum Parsed {
    /// The list of commands — `asked` when `--help` asked for it, not when
    /// the command line was empty.
    Overview {
        asked: bool,
    },
    /// One command's `--help`.
    Help(&'static Tool),
    Call(Invocation),
}

/// The command words of `tool`: its name split at `_`.
pub fn command_words(tool: &Tool) -> Vec<&'static str> {
    tool.name.split('_').collect()
}

/// The flag of a schema property: `content_warning` → `--content-warning`.
pub fn flag_name(property: &str) -> String {
    format!("--{}", property.replace('_', "-"))
}

fn schema_types(property: &Value) -> Vec<&str> {
    match &property["type"] {
        Value::String(t) => vec![t.as_str()],
        Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

fn is_boolean(property: &Value) -> bool {
    schema_types(property) == ["boolean"]
}

/// The command-line spelling of a property's value, as its help shows it.
fn placeholder(property: &Value) -> String {
    if let Some(choices) = property["enum"].as_array() {
        let names: Vec<&str> = choices.iter().filter_map(Value::as_str).collect();
        return names.join("|");
    }
    // An array or object is given as its JSON (`coerce`).
    let types: Vec<&str> = schema_types(property)
        .into_iter()
        .filter(|t| *t != "null")
        .map(|t| match t {
            "array" => "JSON array",
            "object" => "JSON object",
            other => other,
        })
        .collect();
    let mut shown = types.join("|");
    if schema_types(property).contains(&"null") {
        shown.push_str("|null");
    }
    shown
}

/// Turn one command-line word into the JSON value `property`'s schema wants.
fn coerce(name: &str, property: &Value, word: &str) -> Result<Value, UsageError> {
    let types = schema_types(property);
    let bad = |why: &str| UsageError(format!("{}: {why}", flag_name(name)));
    if word == "null" && types.contains(&"null") {
        return Ok(Value::Null);
    }
    let value = match types.iter().find(|t| **t != "null").copied() {
        Some("string") => Value::String(word.to_owned()),
        Some("boolean") => match word {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => return Err(bad("takes true or false")),
        },
        Some("integer") => {
            let n: i64 = word.parse().map_err(|_| bad("takes a whole number"))?;
            if property["minimum"].as_i64().is_some_and(|min| n < min) {
                return Err(bad(&format!("is at least {}", property["minimum"])));
            }
            if property["maximum"].as_i64().is_some_and(|max| n > max) {
                return Err(bad(&format!("is at most {}", property["maximum"])));
            }
            Value::from(n)
        }
        Some("number") => word
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| bad("takes a number"))?,
        // An array or object property is given as its JSON.
        _ => serde_json::from_str(word).map_err(|_| bad("takes JSON"))?,
    };
    if let Some(choices) = property["enum"].as_array()
        && !choices.contains(&value)
    {
        return Err(bad(&format!("takes one of {}", placeholder(property))));
    }
    Ok(value)
}

/// Resolve a command line (the words after the binary's name).
pub fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    let leading: Vec<&str> = args
        .iter()
        .take_while(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect();
    // The longest run of leading words that names a tool is the command.
    let found = (1..=leading.len())
        .rev()
        .find_map(|k| crate::catalog::tool(&leading[..k].join("_")).map(|t| (t, k)));
    let Some((tool, consumed)) = found else {
        if leading.is_empty() {
            let asked = args.iter().any(|a| a == "--help" || a == "-h");
            if asked || args.is_empty() {
                return Ok(Parsed::Overview { asked });
            }
            return Err(UsageError(format!("{} is not a command", args[0])));
        }
        return Err(UsageError(format!(
            "`{}` is not a command",
            leading.join(" ")
        )));
    };
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Parsed::Help(tool));
    }

    let schema = (tool.input_schema)();
    let empty = Map::new();
    let properties = schema["properties"].as_object().unwrap_or(&empty);
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut arguments = Map::new();
    let mut positionals = Vec::new();
    let (mut json, mut yes) = (false, false);
    let mut rest = args[consumed..].iter();
    let mut only_positionals = false;
    while let Some(word) = rest.next() {
        if only_positionals || !word.starts_with("--") {
            positionals.push(word.as_str());
            continue;
        }
        if word == "--" {
            only_positionals = true;
            continue;
        }
        let (flag, inline) = match word.split_once('=') {
            Some((f, v)) => (&f[2..], Some(v)),
            None => (&word[2..], None),
        };
        match (flag, inline) {
            ("json", None) => json = true,
            ("yes", None) => yes = true,
            ("json" | "yes", Some(_)) => {
                return Err(UsageError(format!("--{flag} takes no value")));
            }
            _ => {
                let name = flag.replace('-', "_");
                let Some(property) = properties.get(&name) else {
                    return Err(UsageError(format!(
                        "`{}` has no flag --{flag}",
                        command_words(tool).join(" ")
                    )));
                };
                let given = match inline {
                    Some(v) => coerce(&name, property, v)?,
                    None if is_boolean(property) => Value::Bool(true),
                    None => {
                        let v = rest.next().ok_or_else(|| {
                            UsageError(format!(
                                "--{flag} needs a value ({})",
                                placeholder(property)
                            ))
                        })?;
                        coerce(&name, property, v)?
                    }
                };
                if arguments.insert(name, given).is_some() {
                    return Err(UsageError(format!("--{flag} is given twice")));
                }
            }
        }
    }
    if positionals.len() > required.len() {
        return Err(UsageError(format!(
            "`{}` does not take `{}`",
            command_words(tool).join(" "),
            positionals[required.len()..].join(" ")
        )));
    }
    for (name, word) in required.iter().zip(&positionals) {
        let value = coerce(name, &properties[*name], word)?;
        if arguments.insert((*name).to_owned(), value).is_some() {
            return Err(UsageError(format!("{} is given twice", flag_name(name))));
        }
    }
    if let Some(missing) = required.iter().find(|n| !arguments.contains_key(**n)) {
        return Err(UsageError(format!(
            "`{}` needs {}",
            command_words(tool).join(" "),
            flag_name(missing)
        )));
    }
    Ok(Parsed::Call(Invocation {
        tool,
        arguments: Value::Object(arguments),
        json,
        yes,
    }))
}

/// One command's `--help`: its synopsis, the tool's description, and each
/// flag with the schema's own description.
pub fn help(tool: &Tool) -> String {
    let schema = (tool.input_schema)();
    let empty = Map::new();
    let properties = schema["properties"].as_object().unwrap_or(&empty);
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let command = command_words(tool).join(" ");
    let mut synopsis = format!("fauna-cli {command}");
    for name in &required {
        synopsis.push_str(&format!(" <{name}>"));
    }
    if properties.keys().any(|k| !required.contains(&k.as_str())) {
        synopsis.push_str(" [flags]");
    }
    synopsis.push_str(" [--json]");
    if tool.annotations.destructive {
        synopsis.push_str(" [--yes]");
    }
    let mut out = format!(
        "{} — {}\n\n{}\n\nusage: {synopsis}\n",
        command, tool.title, tool.description
    );
    if !properties.is_empty() {
        out.push_str("\nflags:\n");
    }
    for (name, property) in properties {
        let mut line = format!("  {}", flag_name(name));
        if !is_boolean(property) {
            line.push_str(&format!(" <{}>", placeholder(property)));
        }
        if required.contains(&name.as_str()) {
            line.push_str("  (required; may be given as <");
            line.push_str(name);
            line.push_str(">)");
        }
        out.push_str(&line);
        out.push('\n');
        if let Some(description) = property["description"].as_str() {
            out.push_str(&format!("      {description}\n"));
        }
    }
    out.push_str(
        "\n  --json\n      Print the result as JSON (always, when stdout is not a terminal).\n",
    );
    if tool.annotations.destructive {
        out.push_str(
            "  --yes\n      Run without asking. Off a terminal this command refuses without it.\n",
        );
    }
    out
}

/// The list of commands, each with its tool's title.
pub fn overview() -> String {
    let width = CATALOG
        .iter()
        .map(|t| command_words(t).join(" ").len())
        .max()
        .unwrap_or(0);
    let mut out = String::from(
        "usage: fauna-cli <command> [flags] [--json]\n       fauna-cli mcp [--listen]\n\n\
         `fauna-cli mcp` serves these same commands to an LLM host over MCP.\n\
         `fauna-cli <command> --help` describes one.\n\ncommands:\n",
    );
    for tool in CATALOG {
        out.push_str(&format!(
            "  {:width$}  {}\n",
            command_words(tool).join(" "),
            tool.title
        ));
    }
    out
}

/// A tool's result rendered for a person at a terminal: nested keys
/// indented, lists as `-` items, strings bare.
pub fn render(value: &Value) -> String {
    fn scalar(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Null => Some("-".into()),
            Value::Bool(_) | Value::Number(_) => Some(v.to_string()),
            Value::Array(a) if a.is_empty() => Some("(none)".into()),
            Value::Object(o) if o.is_empty() => Some("(none)".into()),
            _ => None,
        }
    }
    fn walk(v: &Value, indent: usize, out: &mut String) {
        let pad = " ".repeat(indent);
        match v {
            Value::Object(map) => {
                for (k, v) in map {
                    match scalar(v) {
                        Some(s) => out.push_str(&format!("{pad}{k}: {s}\n")),
                        None => {
                            out.push_str(&format!("{pad}{k}:\n"));
                            walk(v, indent + 2, out);
                        }
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    match scalar(item) {
                        Some(s) => out.push_str(&format!("{pad}- {s}\n")),
                        None => {
                            out.push_str(&format!("{pad}-\n"));
                            walk(item, indent + 2, out);
                        }
                    }
                }
            }
            other => out.push_str(&format!("{pad}{}\n", scalar(other).unwrap_or_default())),
        }
    }
    let mut out = String::new();
    walk(value, 0, &mut out);
    out
}

/// Where the shell's streams go: what decides the output form and whether a
/// destructive command may ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminal {
    pub stdout_tty: bool,
    /// The question is answered on stdin, so it is asked only when stdin is a
    /// terminal.
    pub stdin_tty: bool,
}

/// What a command needs before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Run,
    Ask,
    Refuse,
}

/// The annotations govern the terminal as they govern a host: a destructive
/// tool asks on a terminal and refuses off one without `--yes`; every other
/// tool just runs.
pub fn confirmation(tool: &Tool, yes: bool, interactive: bool) -> Confirm {
    match (tool.annotations.destructive, yes, interactive) {
        (false, _, _) | (true, true, _) => Confirm::Run,
        (true, false, true) => Confirm::Ask,
        (true, false, false) => Confirm::Refuse,
    }
}

/// What one run printed and how it exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run one resolved command over `host`. `ask` puts the destructive
/// command's question to the human and answers whether they said yes.
pub async fn execute<H: Host>(
    host: &H,
    inv: &Invocation,
    terminal: Terminal,
    ask: impl FnOnce(&str) -> bool,
) -> Outcome {
    let command = command_words(inv.tool).join(" ");
    let fail = |code, stderr: String| Outcome {
        code,
        stdout: String::new(),
        stderr,
    };
    match confirmation(inv.tool, inv.yes, terminal.stdin_tty) {
        Confirm::Run => {}
        Confirm::Refuse => {
            return fail(
                EXIT_USAGE,
                format!(
                    "fauna-cli: `{command}` cannot be undone; off a terminal it runs only with --yes\n"
                ),
            );
        }
        Confirm::Ask => {
            if !ask(&format!("`{command}` cannot be undone. Run it? [y/N] ")) {
                return fail(
                    EXIT_TOOL_ERROR,
                    "fauna-cli: not confirmed; nothing was done\n".into(),
                );
            }
        }
    }
    match tools::call(host, inv.tool, &inv.arguments).await {
        Ok(data) => Outcome {
            code: EXIT_OK,
            stdout: if inv.json || !terminal.stdout_tty {
                format!("{data}\n")
            } else {
                render(&data)
            },
            stderr: String::new(),
        },
        Err(e) => fail(EXIT_TOOL_ERROR, format!("{}\n", e.text())),
    }
}
