//! The launcher tests' stand-in for kache. It prints, as one JSON object on
//! stdout, the argv it was started with (its own path left out) and, for each
//! argument starting with `@`, that file's lines as rustc reads an argfile
//! (`null` when the file cannot be read); writes one line to stderr; and exits
//! with the code `FAKE_TAIL_EXIT` names (0 when unset).

use std::fmt::Write as _;

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_list<'a>(out: &mut String, items: impl Iterator<Item = &'a str>) {
    out.push('[');
    for (i, item) in items.enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_str(out, item);
    }
    out.push(']');
}

fn main() {
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut out = String::from("{\"argv\":");
    json_list(&mut out, argv.iter().map(String::as_str));
    out.push_str(",\"files\":[");
    let files: Vec<&str> = argv.iter().filter_map(|a| a.strip_prefix('@')).collect();
    for (i, path) in files.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"path\":");
        json_str(&mut out, path);
        out.push_str(",\"lines\":");
        // rustc's own argfile expansion: the whole file as UTF-8, split by `lines()`
        match std::fs::read_to_string(path) {
            Ok(text) => json_list(&mut out, text.lines()),
            Err(_) => out.push_str("null"),
        }
        out.push('}');
    }
    out.push_str("]}");
    println!("{out}");
    eprintln!("fake-tail: this line is the tail's stderr");
    let code = std::env::var("FAKE_TAIL_EXIT")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    std::process::exit(code);
}
