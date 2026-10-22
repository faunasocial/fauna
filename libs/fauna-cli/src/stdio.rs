//! MCP's stdio transport: one JSON-RPC message per line on stdin, one per line
//! on stdout, nothing else on stdout ever (logs go to stderr). The server ends
//! when stdin closes — the host closing the pipe is how a stdio server is told
//! to stop.

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
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = server.handle(&line).await {
            // `Value`'s Display is compact JSON: no newline can appear inside.
            let mut out = answer.to_string();
            out.push('\n');
            writer.write_all(out.as_bytes()).await?;
            writer.flush().await?;
        }
    }
    Ok(())
}
