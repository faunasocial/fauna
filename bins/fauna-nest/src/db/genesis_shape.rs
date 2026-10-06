//! The genesis schema's pinned shape — the test half of `migrations.rs` §
//! Genesis.
//!
//! [`shape`] reduces a database's schema to text that two schemas agree on
//! exactly when they are the same schema: every table's columns in declaration
//! order (`PRAGMA table_xinfo`), its foreign keys, its column and table
//! constraints (the `CREATE TABLE` body split at top-level commas, comments
//! stripped, sorted so an appended column and a hand-placed one compare
//! equal), and every index, trigger and view's normalized SQL. Comments and
//! whitespace never reach it, so rewording a column comment is not a schema
//! change and does not touch the pin.
//!
//! `genesis_shape.txt` beside this file is [`shape`] of a fresh genesis. It was
//! derived once, from the 78-step history the first genesis replaced, before
//! that history was deleted, and it held across the second collapse (the
//! one-shot steps of schemas 79..=124 were deleted with the pin unmoved but for
//! the two columns that collapse tightened) — which is what makes the pin a
//! proof that neither collapse lost anything. It is the schema's snapshot: a
//! change to the genesis changes this file in the same commit, rewritten by
//! running `genesis_matches_its_pinned_shape` with
//! `FAUNA_BLESS_GENESIS_SHAPE=1`.

use rusqlite::Connection;

/// The pinned snapshot, compared by [`tests::genesis_matches_its_pinned_shape`].
pub(crate) const PINNED: &str = include_str!("genesis_shape.txt");

/// Strip `--` comments (outside string literals), `IF NOT EXISTS`, identifier
/// quotes, and collapse whitespace.
fn normalize(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\'' {
                in_str = false;
            }
            continue;
        }
        match c {
            '\'' => {
                in_str = true;
                out.push(c);
            }
            '-' if chars.peek() == Some(&'-') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
                out.push(' ');
            }
            '"' | '`' => {}
            _ => out.push(c),
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let collapsed = collapsed
        .replace("IF NOT EXISTS ", "")
        .replace("if not exists ", "")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(" ,", ",");
    collapsed.trim().to_string()
}

/// The top-level, comma-separated items of the outermost parenthesised body.
fn body_items(normalized: &str) -> Vec<String> {
    let Some(open) = normalized.find('(') else {
        return Vec::new();
    };
    let Some(close) = normalized.rfind(')') else {
        return Vec::new();
    };
    let body = &normalized[open + 1..close];
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut cur = String::new();
    for c in body.chars() {
        if in_str {
            cur.push(c);
            if c == '\'' {
                in_str = false;
            }
            continue;
        }
        match c {
            '\'' => {
                in_str = true;
                cur.push(c);
            }
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                items.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    items.push(cur.trim().to_string());
    items.sort();
    items
}

fn rows<T, F>(conn: &Connection, sql: &str, f: F) -> Vec<T>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    let mut stmt = conn.prepare(sql).expect("prepare shape query");
    stmt.query_map([], f)
        .expect("run shape query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read shape rows")
}

/// The schema of `conn`, as the pinned text.
pub(crate) fn shape(conn: &Connection) -> String {
    let mut out = String::new();
    let tables: Vec<(String, String)> = rows(
        conn,
        "SELECT name, sql FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        |r| Ok((r.get(0)?, r.get(1)?)),
    );
    for (table, sql) in tables {
        out.push_str(&format!("table {table}\n"));
        let cols: Vec<String> = rows(conn, &format!("PRAGMA table_xinfo(\"{table}\")"), |r| {
            Ok(format!(
                "  col {} {} notnull={} default={} pk={} hidden={}",
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?.unwrap_or_else(|| "-".into()),
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        });
        for c in cols {
            out.push_str(&c);
            out.push('\n');
        }
        let fks: Vec<String> = rows(
            conn,
            &format!("PRAGMA foreign_key_list(\"{table}\")"),
            |r| {
                Ok(format!(
                    "  fk {} -> {}.{} update={} delete={}",
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(4)?.unwrap_or_else(|| "-".into()),
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        );
        let mut fks = fks;
        fks.sort();
        for f in fks {
            out.push_str(&f);
            out.push('\n');
        }
        for item in body_items(&normalize(&sql)) {
            out.push_str(&format!("  item {item}\n"));
        }
    }
    let objects: Vec<(String, String, String)> = rows(
        conn,
        "SELECT type, name, sql FROM sqlite_master \
         WHERE type IN ('index', 'trigger', 'view') AND sql IS NOT NULL ORDER BY name",
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    );
    for (ty, name, sql) in objects {
        out.push_str(&format!("{ty} {name}: {}\n", normalize(&sql)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh genesis has exactly the pinned shape (`migrations.rs` §
    /// Genesis). Set `FAUNA_BLESS_GENESIS_SHAPE=1` to rewrite the pin after a
    /// deliberate schema change — and review the diff it leaves.
    #[test]
    fn genesis_matches_its_pinned_shape() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run_migrations(&conn).unwrap();
        let actual = shape(&conn);
        if std::env::var_os("FAUNA_BLESS_GENESIS_SHAPE").is_some() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/db/genesis_shape.txt");
            std::fs::write(path, &actual).unwrap();
            return;
        }
        if actual != PINNED {
            let diff: Vec<String> = {
                let pinned: std::collections::BTreeSet<&str> = PINNED.lines().collect();
                let now: std::collections::BTreeSet<&str> = actual.lines().collect();
                pinned
                    .difference(&now)
                    .map(|l| format!("- {l}"))
                    .chain(now.difference(&pinned).map(|l| format!("+ {l}")))
                    .collect()
            };
            panic!(
                "the genesis schema no longer matches genesis_shape.txt \
                 ({} line(s) differ; rerun with FAUNA_BLESS_GENESIS_SHAPE=1 after a \
                 deliberate change):\n{}",
                diff.len(),
                diff.join("\n")
            );
        }
    }

    #[test]
    fn normalize_strips_comments_quotes_and_if_not_exists() {
        assert_eq!(
            normalize(
                "CREATE TABLE IF NOT EXISTS \"t\" (\n  a TEXT DEFAULT '--x', -- note, with a comma\n  b INTEGER\n)"
            ),
            "CREATE TABLE t (a TEXT DEFAULT '--x', b INTEGER)"
        );
        assert_eq!(
            body_items("CREATE TABLE t (b INT, a TEXT DEFAULT 'x,y', PRIMARY KEY (a, b))"),
            vec!["PRIMARY KEY (a, b)", "a TEXT DEFAULT 'x,y'", "b INT"]
        );
    }
}
