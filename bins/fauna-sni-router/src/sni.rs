//! Extract the SNI host_name from a TLS ClientHello, for L4 routing only.
//!
//! This parser reads untrusted bytes but makes **no trust decision** — it only
//! returns a hostname used to pick a backend, which then performs full TLS
//! termination + auth itself. A wrong/None result merely routes to the default
//! backend (whose handshake then fails if the client meant elsewhere). So the
//! parser's only safety obligation is to never panic and never read out of
//! bounds; it deliberately does the minimum to find the first
//! `server_name` of type `host_name`.
//!
//! Wire layout it walks (RFC 8446 / RFC 6066), all lengths big-endian:
//!   TLS record header (5):  type=0x16(handshake)  version(2)  length(2)
//!   Handshake header (4):   msg_type=0x01(ClientHello)  length(3)
//!   ClientHello body:       client_version(2)  random(32)
//!                           session_id:        len(1) + bytes
//!                           cipher_suites:     len(2) + bytes
//!                           compression:       len(1) + bytes
//!                           extensions:        len(2) + [ ext ... ]
//!   Extension:              type(2)  length(2)  data(length)
//!     SNI (type 0x0000) data:
//!                           server_name_list:  len(2) + [ entry ... ]
//!     Entry:                name_type(1)  name_len(2)  name(name_len)
//!       name_type 0x00 = host_name.

/// A forward-only cursor over a byte slice with checked reads (no panics).
struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cursor { b, i: 0 }
    }

    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.i)?;
        self.i += 1;
        Some(v)
    }

    fn u16(&mut self) -> Option<usize> {
        let hi = self.u8()? as usize;
        let lo = self.u8()? as usize;
        Some((hi << 8) | lo)
    }

    fn u24(&mut self) -> Option<usize> {
        let a = self.u8()? as usize;
        let b = self.u8()? as usize;
        let c = self.u8()? as usize;
        Some((a << 16) | (b << 8) | c)
    }

    /// Advance by `n`, failing if it would overrun.
    fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.i.checked_add(n)?;
        if end > self.b.len() {
            return None;
        }
        self.i = end;
        Some(())
    }

    /// Borrow the next `n` bytes, failing if it would overrun.
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.i.checked_add(n)?;
        let s = self.b.get(self.i..end)?;
        self.i = end;
        Some(s)
    }
}

/// The number of leading bytes that definitely belong to the first TLS record,
/// so the caller knows when it has buffered enough to parse, or `None` if `buf`
/// is too short to even contain a record header. The returned value is
/// `5 + record_length`; the caller should keep reading until it has that many.
pub fn first_record_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 5 {
        return None;
    }
    // Must be a handshake record (0x16). Anything else: not a ClientHello.
    if buf[0] != 0x16 {
        return Some(5); // header only — caller will parse, find no SNI, use default
    }
    let len = ((buf[3] as usize) << 8) | (buf[4] as usize);
    Some(5 + len)
}

/// Parse the SNI host_name from a buffer that contains (at least) the first TLS
/// record carrying the ClientHello. Returns the lowercased hostname, or `None`
/// if absent/unparyseable (caller routes to the default backend).
pub fn parse_sni(buf: &[u8]) -> Option<String> {
    let mut c = Cursor::new(buf);

    // TLS record header.
    if c.u8()? != 0x16 {
        return None; // not handshake
    }
    let _ver = c.u16()?;
    let rec_len = c.u16()?;
    // Constrain the rest of parsing to this record's body.
    let body = c.take(rec_len)?;
    let mut c = Cursor::new(body);

    // Handshake header.
    if c.u8()? != 0x01 {
        return None; // not ClientHello
    }
    let hs_len = c.u24()?;
    let hs = c.take(hs_len.min(body.len()))?;
    let mut c = Cursor::new(hs);

    // ClientHello body.
    c.skip(2)?; // client_version
    c.skip(32)?; // random
    let sid_len = c.u8()? as usize;
    c.skip(sid_len)?; // session_id
    let cs_len = c.u16()?;
    c.skip(cs_len)?; // cipher_suites
    let comp_len = c.u8()? as usize;
    c.skip(comp_len)?; // compression_methods

    // Extensions (absent in very old hellos → no SNI).
    let ext_total = c.u16()?;
    let exts = c.take(ext_total)?;
    let mut e = Cursor::new(exts);
    while e.i < exts.len() {
        let ext_type = e.u16()?;
        let ext_len = e.u16()?;
        let ext_data = e.take(ext_len)?;
        if ext_type == 0x0000 {
            return parse_server_name_list(ext_data);
        }
    }
    None
}

fn parse_server_name_list(data: &[u8]) -> Option<String> {
    let mut c = Cursor::new(data);
    let list_len = c.u16()?;
    let list = c.take(list_len.min(data.len().saturating_sub(2)))?;
    let mut e = Cursor::new(list);
    while e.i < list.len() {
        let name_type = e.u8()?;
        let name_len = e.u16()?;
        let name = e.take(name_len)?;
        if name_type == 0x00 {
            // host_name; ASCII per RFC 6066. Reject non-UTF8 / control chars.
            let s = std::str::from_utf8(name).ok()?;
            if s.is_empty() || s.bytes().any(|b| b < 0x20 || b == 0x7f) {
                return None;
            }
            return Some(s.to_ascii_lowercase());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal but well-formed ClientHello record carrying one SNI host.
    fn client_hello_with_sni(host: &str) -> Vec<u8> {
        // server_name entry: name_type(0x00) + name_len(2) + name
        let mut entry = vec![0x00];
        entry.extend_from_slice(&(host.len() as u16).to_be_bytes());
        entry.extend_from_slice(host.as_bytes());
        // server_name_list: list_len(2) + entry
        let mut snl = (entry.len() as u16).to_be_bytes().to_vec();
        snl.extend_from_slice(&entry);
        // SNI extension: type(0x0000) + len(2) + snl
        let mut ext = vec![0x00, 0x00];
        ext.extend_from_slice(&(snl.len() as u16).to_be_bytes());
        ext.extend_from_slice(&snl);
        // extensions block: total_len(2) + ext
        let mut exts = (ext.len() as u16).to_be_bytes().to_vec();
        exts.extend_from_slice(&ext);

        // ClientHello body.
        let mut body = Vec::new();
        body.extend_from_slice(&[0x03, 0x03]); // client_version TLS1.2
        body.extend_from_slice(&[0u8; 32]); // random
        body.push(0x00); // session_id len = 0
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // cipher_suites: len2 + one suite
        body.extend_from_slice(&[0x01, 0x00]); // compression: len1 + null
        body.extend_from_slice(&exts);

        // Handshake header: msg_type(0x01) + len(3).
        let mut hs = vec![0x01];
        hs.extend_from_slice(&[
            ((body.len() >> 16) & 0xff) as u8,
            ((body.len() >> 8) & 0xff) as u8,
            (body.len() & 0xff) as u8,
        ]);
        hs.extend_from_slice(&body);

        // TLS record header: type(0x16) + version(2) + len(2).
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn extracts_sni_host() {
        let rec = client_hello_with_sni("mail.example.com");
        assert_eq!(parse_sni(&rec).as_deref(), Some("mail.example.com"));
    }

    #[test]
    fn lowercases_sni() {
        let rec = client_hello_with_sni("Mail.example.com");
        assert_eq!(parse_sni(&rec).as_deref(), Some("mail.example.com"));
    }

    #[test]
    fn first_record_len_reports_full_record() {
        let rec = client_hello_with_sni("example.com");
        assert_eq!(first_record_len(&rec), Some(rec.len()));
    }

    #[test]
    fn no_sni_extension_returns_none() {
        // A ClientHello with an empty extensions block.
        let mut body = Vec::new();
        body.extend_from_slice(&[0x03, 0x03]);
        body.extend_from_slice(&[0u8; 32]);
        body.push(0x00);
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]);
        body.extend_from_slice(&[0x01, 0x00]);
        body.extend_from_slice(&[0x00, 0x00]); // extensions total len = 0
        let mut hs = vec![0x01];
        hs.extend_from_slice(&[0, 0, body.len() as u8]);
        hs.extend_from_slice(&body);
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        assert_eq!(parse_sni(&rec), None);
    }

    #[test]
    fn truncated_buffers_never_panic() {
        let rec = client_hello_with_sni("mail.example.com");
        for n in 0..rec.len() {
            // Any prefix must return Some/None, never panic.
            let _ = parse_sni(&rec[..n]);
            let _ = first_record_len(&rec[..n]);
        }
    }

    #[test]
    fn non_handshake_record_is_none() {
        let buf = [0x17, 0x03, 0x03, 0x00, 0x01, 0xff];
        assert_eq!(parse_sni(&buf), None);
    }
}
