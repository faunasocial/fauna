//! MCP's stdio transport: one JSON-RPC message per line on stdin, one per line
//! on stdout, nothing else on stdout ever (logs go to stderr). The server ends
//! when stdin closes — the host closing the pipe is how a stdio server is told
//! to stop.
//!
//! A server built [`Server::announcing`] also gets
//! `notifications/tools/list_changed` written whenever what `tools/list`
//! answers changes — a connection landing, failing, revoked or added —
//! whether or not a request is in flight (`apps/cli.md` § A tool that cannot
//! run, rule 2).

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::mcp::Server;
use crate::tools::Host;

/// Serve `server` over `reader`/`writer` until `reader` ends.
pub async fn serve<H, R, W>(server: &Server<H>, reader: R, mut writer: W) -> std::io::Result<()>
where
    H: Host,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = reader.lines();
    let mut listed: Vec<&'static str> = Vec::new();
    if server.announces() {
        server.list_changed(&mut listed).await;
    }
    loop {
        // `next_line` is cancel-safe, so a reach change between two lines
        // loses no input.
        let line = tokio::select! {
            line = lines.next_line() => line?,
            () = server.host().reach_changed(), if server.announces() => {
                if let Some(note) = server.list_changed(&mut listed).await {
                    write(&mut writer, &note).await?;
                }
                continue;
            }
        };
        let Some(line) = line else { return Ok(()) };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = server.handle(&line).await {
            write(&mut writer, &answer).await?;
        }
        if server.announces()
            && let Some(note) = server.list_changed(&mut listed).await
        {
            write(&mut writer, &note).await?;
        }
    }
}

async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &serde_json::Value,
) -> std::io::Result<()> {
    // `Value`'s Display is compact JSON: no newline can appear inside.
    let mut out = message.to_string();
    out.push('\n');
    writer.write_all(out.as_bytes()).await?;
    writer.flush().await
}
