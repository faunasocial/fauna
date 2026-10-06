# fauna-tui — the terminal app

A Rust TUI app — the seventh Fauna app, and the **lead app**: every new UI
feature lands here first. It reached its feature-parity milestone on
2026-07-19 alongside the six others (web, Linux, macOS, iOS, Android,
Windows), and shares the same core crates, string table, and UI element IDs as
the rest. Its remaining gaps are *declared platform absences*, listed in the
architecture doc below.

```sh
just tui-debug        # or: cargo build -p fauna-tui
cargo test -p fauna-tui
```

Roadmap and parity milestones:
[`docs/goal/architecture/apps/tui.md`](../../docs/goal/architecture/apps/tui.md).
