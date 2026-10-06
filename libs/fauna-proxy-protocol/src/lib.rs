//! Minimal [PROXY protocol v2] codec — encode (router side) and parse
//! (nest side) — shared by `fauna-sni-router` and `fauna-nest`.
//!
//! # Why this exists
//!
//! On the single-box deployment, container `:443` is owned by `fauna-sni-router`,
//! an L4 SNI-passthrough splicer that terminates no TLS and forwards the raw
//! byte stream to a loopback backend (`127.0.0.1:3000` nest, `127.0.0.1:8444`
//! MDA). A plain TCP splice makes the backend see the *router's* loopback
//! address as the peer, not the real internet client — which silently defeats
//! nest's per-source rate limiting and the loopback-only trust gate on
//! `fauna.bridges.request_enrollment` (the real in-container mail bridge also
//! dials `127.0.0.1:3000`, so the backend cannot otherwise tell an external
//! client from the local bridge). Security review tracked internally.
//!
//! The router prepends a PROXY-v2 header conveying the real client address;
//! nest parses it on its TLS accept path (only when the immediate TCP peer is
//! loopback — i.e. the in-container router) and uses the conveyed address as
//! the connection source. A connection with **no** header (the bridge dialing
//! nest directly, whose first byte is the TLS handshake type `0x16`) keeps its
//! genuine loopback peer, so the loopback gate still recognises it.
//!
//! Only the subset of the spec the router emits is supported: the v2 `PROXY`
//! command over `AF_INET`/`AF_INET6` + `STREAM`. `LOCAL`, `AF_UNIX`, and
//! `DGRAM` are parsed as "no usable source address" (the caller falls back to
//! the TCP peer).
//!
//! [PROXY protocol v2]: https://www.haproxy.org/download/1.8/doc/proxy-protocol.txt

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

/// The 12-byte PROXY-v2 signature that prefixes every header.
///
/// `\r\n\r\n\0\r\nQUIT\n` — deliberately chosen by the spec so it cannot be
/// mistaken for an HTTP request, an SMTP command, or a TLS ClientHello (which
/// starts with the handshake content-type byte `0x16`). The first byte (`0x0D`)
/// is enough to cheaply distinguish "PROXY header" from "TLS ClientHello".
pub const SIGNATURE: [u8; 12] = [
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

/// Length of the fixed header prefix: 12-byte signature + 1 version/command
/// byte + 1 family/transport byte + 2-byte big-endian address-block length.
pub const PREFIX_LEN: usize = 16;

const VER_CMD_PROXY: u8 = 0x21; // version 2 (high nibble), PROXY command (low nibble)
const FAM_TCP4: u8 = 0x11; // AF_INET + STREAM
const FAM_TCP6: u8 = 0x21; // AF_INET6 + STREAM
const TCP4_BLOCK_LEN: usize = 12; // src(4) + dst(4) + sport(2) + dport(2)
const TCP6_BLOCK_LEN: usize = 36; // src(16) + dst(16) + sport(2) + dport(2)

/// PROXY-v2 TLV type carrying the router→nest authentication secret.
///
/// The 2-byte address block is, per the v2 spec, followed by an optional run of
/// `type(1) || length(2 BE) || value` TLVs, all counted in the header's `len`
/// field (`PrefixInfo::addr_len`). We use one custom TLV (type in the spec's
/// `0xE0..=0xEF` private range) to carry a deployment secret that proves the
/// header was written by the SNI router and not a co-resident process forging a
/// loopback PROXY header to spoof a source IP (
/// `docs/goal/architecture/security.md` § Co-resident process trust boundary).
///
/// A spec-compliant downstream that does not understand the TLV (the Go MDA's
/// `internal/proxyproto`) simply skips it — it reads the address block from the
/// front and discards the whole `len`-byte payload — so emitting the TLV on
/// every header is backward-compatible.
pub const TLV_TYPE_ROUTER_AUTH: u8 = 0xE0;

/// Bytes a single TLV adds beyond its value: `type(1) + length(2)`.
const TLV_HEADER_LEN: usize = 3;

/// Parsed fixed prefix of a PROXY-v2 header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixInfo {
    /// The family/transport byte (`0x11` = TCP/IPv4, `0x21` = TCP/IPv6, …).
    pub fam_proto: u8,
    /// Length of the address block that follows the prefix, from the header.
    pub addr_len: usize,
    /// `true` for the `PROXY` command, `false` for `LOCAL`.
    pub is_proxy_command: bool,
}

/// Encode a full PROXY-v2 `PROXY`-command header for a TCP connection from
/// `src` to `dst`, with no TLVs. Thin wrapper over [`encode_v2_authed`] with no
/// auth secret — kept for callers (and tests) that don't authenticate.
pub fn encode_v2(src: SocketAddr, dst: SocketAddr) -> Vec<u8> {
    encode_v2_authed(src, dst, None)
}

/// Encode a full PROXY-v2 `PROXY`-command header for a TCP connection from
/// `src` to `dst`, optionally appending the router-auth TLV (`auth`). `src` is
/// what the parser reads back as the connection source; `dst` is required by
/// the wire format but nest ignores it.
///
/// When `auth` is `Some`, a [`TLV_TYPE_ROUTER_AUTH`] TLV carrying the secret is
/// appended after the address block and counted in the header's `len` field, so
/// nest can verify the header came from the SNI router. When `None`, the
/// output is byte-identical to a plain v2 header (backward-compatible).
///
/// If `src` and `dst` disagree on address family (not possible for the two
/// ends of one TCP connection, but handled defensively), the header is emitted
/// in `src`'s family with a zeroed destination.
pub fn encode_v2_authed(src: SocketAddr, dst: SocketAddr, auth: Option<&[u8]>) -> Vec<u8> {
    // The trailing auth TLV (if any) is part of the v2 `len` field along with
    // the address block, so compute the total payload length up front.
    let tlv_len = auth.map_or(0, |a| TLV_HEADER_LEN + a.len());
    let block_len = if src.is_ipv4() {
        TCP4_BLOCK_LEN
    } else {
        TCP6_BLOCK_LEN
    };
    let payload_len = (block_len + tlv_len) as u16;

    let mut h = Vec::with_capacity(PREFIX_LEN + block_len + tlv_len);
    h.extend_from_slice(&SIGNATURE);
    h.push(VER_CMD_PROXY);
    match src {
        SocketAddr::V4(s) => {
            let d = match dst {
                SocketAddr::V4(d) => *d.ip(),
                SocketAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
            };
            let dport = if matches!(dst, SocketAddr::V4(_)) {
                dst.port()
            } else {
                0
            };
            h.push(FAM_TCP4);
            h.extend_from_slice(&payload_len.to_be_bytes());
            h.extend_from_slice(&s.ip().octets());
            h.extend_from_slice(&d.octets());
            h.extend_from_slice(&s.port().to_be_bytes());
            h.extend_from_slice(&dport.to_be_bytes());
        }
        SocketAddr::V6(s) => {
            let d = match dst {
                SocketAddr::V6(d) => *d.ip(),
                SocketAddr::V4(_) => Ipv6Addr::UNSPECIFIED,
            };
            let dport = if matches!(dst, SocketAddr::V6(_)) {
                dst.port()
            } else {
                0
            };
            h.push(FAM_TCP6);
            h.extend_from_slice(&payload_len.to_be_bytes());
            h.extend_from_slice(&s.ip().octets());
            h.extend_from_slice(&d.octets());
            h.extend_from_slice(&s.port().to_be_bytes());
            h.extend_from_slice(&dport.to_be_bytes());
        }
    }
    if let Some(a) = auth {
        h.push(TLV_TYPE_ROUTER_AUTH);
        h.extend_from_slice(&(a.len() as u16).to_be_bytes());
        h.extend_from_slice(a);
    }
    h
}

/// Parse the fixed 16-byte prefix. Returns `None` when the bytes are not a
/// PROXY-v2 header we understand (wrong signature, wrong version) — the caller
/// then treats the connection as a direct, headerless one.
pub fn parse_prefix(prefix: &[u8; PREFIX_LEN]) -> Option<PrefixInfo> {
    if prefix[..12] != SIGNATURE {
        return None;
    }
    let ver = prefix[12] >> 4;
    if ver != 0x2 {
        return None;
    }
    let command = prefix[12] & 0x0F;
    let addr_len = u16::from_be_bytes([prefix[14], prefix[15]]) as usize;
    Some(PrefixInfo {
        fam_proto: prefix[13],
        addr_len,
        is_proxy_command: command == 0x1,
    })
}

/// Parse the source `SocketAddr` from a PROXY-v2 address block, given its
/// family/transport byte. Returns `None` for unsupported families or a block
/// too short to hold the declared family's addresses.
pub fn parse_src(fam_proto: u8, addr_block: &[u8]) -> Option<SocketAddr> {
    match fam_proto {
        FAM_TCP4 if addr_block.len() >= TCP4_BLOCK_LEN => {
            let ip = Ipv4Addr::new(addr_block[0], addr_block[1], addr_block[2], addr_block[3]);
            let port = u16::from_be_bytes([addr_block[8], addr_block[9]]);
            Some(SocketAddr::V4(SocketAddrV4::new(ip, port)))
        }
        FAM_TCP6 if addr_block.len() >= TCP6_BLOCK_LEN => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&addr_block[0..16]);
            let ip = Ipv6Addr::from(octets);
            let port = u16::from_be_bytes([addr_block[32], addr_block[33]]);
            Some(SocketAddr::V6(SocketAddrV6::new(ip, port, 0, 0)))
        }
        _ => None,
    }
}

/// Extract the value of the [`TLV_TYPE_ROUTER_AUTH`] TLV from a PROXY-v2
/// payload (the `addr_len`-byte run after the fixed prefix), given its
/// family/transport byte. The address block is skipped, then the trailing TLVs
/// are walked; the first router-auth TLV's value is returned.
///
/// Returns `None` when the family is unsupported, the block is truncated, no
/// router-auth TLV is present, or a TLV length runs past the payload (malformed
/// — treated as absent rather than trusted). nest uses this to verify the
/// header was written by the SNI router; a co-resident process that cannot read
/// the secret cannot produce a matching TLV.
pub fn parse_router_auth_tlv(fam_proto: u8, payload: &[u8]) -> Option<&[u8]> {
    let block_len = match fam_proto {
        FAM_TCP4 => TCP4_BLOCK_LEN,
        FAM_TCP6 => TCP6_BLOCK_LEN,
        _ => return None,
    };
    let mut tlvs = payload.get(block_len..)?;
    // Walk `type(1) || length(2 BE) || value` records.
    while tlvs.len() >= TLV_HEADER_LEN {
        let ty = tlvs[0];
        let len = u16::from_be_bytes([tlvs[1], tlvs[2]]) as usize;
        let value = tlvs.get(TLV_HEADER_LEN..TLV_HEADER_LEN + len)?;
        if ty == TLV_TYPE_ROUTER_AUTH {
            return Some(value);
        }
        tlvs = &tlvs[TLV_HEADER_LEN + len..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(src: SocketAddr, dst: SocketAddr) -> SocketAddr {
        let buf = encode_v2(src, dst);
        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&buf[..PREFIX_LEN]);
        let info = parse_prefix(&prefix).expect("valid prefix");
        assert!(info.is_proxy_command);
        assert_eq!(buf.len(), PREFIX_LEN + info.addr_len);
        parse_src(info.fam_proto, &buf[PREFIX_LEN..]).expect("parseable src")
    }

    #[test]
    fn ipv4_roundtrips() {
        let src: SocketAddr = "203.0.113.7:54321".parse().unwrap();
        let dst: SocketAddr = "198.51.100.1:443".parse().unwrap();
        assert_eq!(roundtrip(src, dst), src);
    }

    #[test]
    fn ipv6_roundtrips() {
        let src: SocketAddr = "[2001:db8::dead:beef]:443".parse().unwrap();
        let dst: SocketAddr = "[2001:db8::1]:443".parse().unwrap();
        assert_eq!(roundtrip(src, dst), src);
    }

    #[test]
    fn a_tls_clienthello_is_not_a_proxy_header() {
        // TLS records start with the handshake content type 0x16, never 0x0D.
        let mut prefix = [0u8; PREFIX_LEN];
        prefix[0] = 0x16;
        assert_eq!(parse_prefix(&prefix), None);
    }

    #[test]
    fn wrong_signature_rejected() {
        let mut prefix = SIGNATURE.to_vec();
        prefix[5] ^= 0xFF; // corrupt one signature byte
        prefix.extend_from_slice(&[VER_CMD_PROXY, FAM_TCP4, 0, TCP4_BLOCK_LEN as u8]);
        let mut p = [0u8; PREFIX_LEN];
        p.copy_from_slice(&prefix);
        assert_eq!(parse_prefix(&p), None);
    }

    #[test]
    fn wrong_version_rejected() {
        // Signature OK but version nibble = 1 (PROXY protocol v1 is text-based).
        let mut p = [0u8; PREFIX_LEN];
        p[..12].copy_from_slice(&SIGNATURE);
        p[12] = 0x11; // version 1
        assert_eq!(parse_prefix(&p), None);
    }

    #[test]
    fn local_command_is_parsed_but_flagged_not_proxy() {
        let mut p = [0u8; PREFIX_LEN];
        p[..12].copy_from_slice(&SIGNATURE);
        p[12] = 0x20; // version 2, LOCAL command
        p[13] = 0x00; // UNSPEC
        let info = parse_prefix(&p).expect("v2 prefix");
        assert!(!info.is_proxy_command);
        assert_eq!(parse_src(info.fam_proto, &[]), None);
    }

    #[test]
    fn truncated_address_block_returns_none() {
        // Claims TCP4 but only 3 bytes of address present.
        assert_eq!(parse_src(FAM_TCP4, &[1, 2, 3]), None);
    }

    /// The full encode→parse round-trip with the router-auth TLV: the source
    /// address still parses (the TLV rides after the address block) and the
    /// secret is recovered.
    fn authed_roundtrip(src: SocketAddr, dst: SocketAddr, secret: &[u8]) {
        let buf = encode_v2_authed(src, dst, Some(secret));
        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&buf[..PREFIX_LEN]);
        let info = parse_prefix(&prefix).expect("valid prefix");
        assert!(info.is_proxy_command);
        // `len` now spans the address block + the TLV.
        assert_eq!(buf.len(), PREFIX_LEN + info.addr_len);
        let payload = &buf[PREFIX_LEN..];
        assert_eq!(
            parse_src(info.fam_proto, payload).expect("src still parses"),
            src,
            "the address block must still resolve with a trailing TLV"
        );
        assert_eq!(
            parse_router_auth_tlv(info.fam_proto, payload),
            Some(secret),
            "the auth secret must round-trip"
        );
    }

    #[test]
    fn authed_header_roundtrips_v4_and_v6() {
        let secret = b"\x00\x01\x02 a 32-byte-ish random secret!";
        authed_roundtrip(
            "203.0.113.7:54321".parse().unwrap(),
            "198.51.100.1:443".parse().unwrap(),
            secret,
        );
        authed_roundtrip(
            "[2001:db8::dead:beef]:443".parse().unwrap(),
            "[2001:db8::1]:443".parse().unwrap(),
            secret,
        );
    }

    #[test]
    fn encode_v2_emits_no_auth_tlv() {
        // A plain (unauthenticated) header has no router-auth TLV — the byte
        // shape is unchanged from before TLV support.
        let buf = encode_v2(
            "203.0.113.7:54321".parse().unwrap(),
            "198.51.100.1:443".parse().unwrap(),
        );
        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&buf[..PREFIX_LEN]);
        let info = parse_prefix(&prefix).unwrap();
        assert_eq!(info.addr_len, TCP4_BLOCK_LEN, "no TLV bytes in len");
        assert_eq!(
            parse_router_auth_tlv(info.fam_proto, &buf[PREFIX_LEN..]),
            None
        );
    }

    #[test]
    fn auth_tlv_absent_when_other_tlvs_present() {
        // A payload with a non-auth TLV (type 0x03 = PP2_TYPE_CRC32C) but no
        // router-auth TLV must report absent, not mis-read the other TLV.
        let mut payload = vec![0u8; TCP4_BLOCK_LEN];
        payload.extend_from_slice(&[0x03, 0x00, 0x04, 0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(parse_router_auth_tlv(FAM_TCP4, &payload), None);
    }

    #[test]
    fn malformed_tlv_length_is_treated_as_absent() {
        // A TLV claiming more value bytes than remain is malformed → None
        // (never trusted), not a panic.
        let mut payload = vec![0u8; TCP4_BLOCK_LEN];
        payload.extend_from_slice(&[TLV_TYPE_ROUTER_AUTH, 0xFF, 0xFF, 0x01]);
        assert_eq!(parse_router_auth_tlv(FAM_TCP4, &payload), None);
    }

    #[test]
    fn auth_tlv_unsupported_family_returns_none() {
        assert_eq!(parse_router_auth_tlv(0x00, &[1, 2, 3]), None);
    }
}
