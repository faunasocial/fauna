//! The tar-entry-header and stream-finish ceremony shared by every nest
//! data-export route that writes a zstd-compressed tar (admin export, the
//! logical dump) — these call sites hand-copied the same
//! standard-GNU-header-plus-checksum entry write and the same
//! into_inner-then-finish close. Format-specific content (NDJSON tables)
//! stays with its own module; this owns only the container mechanics.
//! (A third consumer, a nest-side Maildir/mbox email serializer, was removed
//! as I6 mail-bridge-cutover residue — email
//! export runs client-side, per `docs/goal/behavior/mail-export.md` §
//! Implementation status today.)

use std::io::Write;

use anyhow::Result;

/// Append one entry: a standard GNU header (mode `0o644`, computed checksum)
/// plus `data`.
pub fn append_entry(tar: &mut tar::Builder<impl Write>, path: &str, data: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, path, data)?;
    Ok(())
}

/// Close the tar (writes the end-of-archive marker) then finalize the zstd
/// frame `open` started.
pub fn finish<W: Write>(tar: tar::Builder<zstd::stream::Encoder<'static, W>>) -> Result<()> {
    let encoder = tar.into_inner()?;
    encoder.finish()?;
    Ok(())
}
