//! Property tests over the three stranger-facing PROXY-v2 parsers. Nest reads
//! this header on its TLS accept path (trusted only from a loopback peer, but
//! the bytes are still peer-supplied), so the residual bug class is a panic or
//! an over-read on crafted input. The properties: never panic, never read past
//! the buffer, and agree with the encoder on every well-formed header.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::{
    PREFIX_LEN, SIGNATURE, encode_v2_authed, parse_prefix, parse_router_auth_tlv, parse_src,
};
use proptest::prelude::*;

fn arb_addr() -> impl Strategy<Value = SocketAddr> {
    prop_oneof![
        (any::<[u8; 4]>(), any::<u16>())
            .prop_map(|(o, p)| SocketAddr::new(IpAddr::V4(Ipv4Addr::from(o)), p)),
        (any::<[u8; 16]>(), any::<u16>())
            .prop_map(|(o, p)| SocketAddr::new(IpAddr::V6(Ipv6Addr::from(o)), p)),
    ]
}

proptest! {
    #[test]
    fn parse_prefix_never_panics(prefix in any::<[u8; PREFIX_LEN]>()) {
        if let Some(info) = parse_prefix(&prefix) {
            // addr_len is a u16 off the wire: bounded, whatever it claims.
            prop_assert!(info.addr_len <= u16::MAX as usize);
        }
    }

    /// A prefix without the signature is never accepted as a header.
    #[test]
    fn prefix_without_signature_is_none(
        mut prefix in any::<[u8; PREFIX_LEN]>(),
        flip in 0usize..12,
    ) {
        prefix[..12].copy_from_slice(&SIGNATURE);
        prefix[flip] ^= 0x01;
        prop_assert!(parse_prefix(&prefix).is_none());
    }

    #[test]
    fn parse_src_never_panics(
        fam in any::<u8>(),
        block in proptest::collection::vec(any::<u8>(), 0..128),
    ) {
        let _ = parse_src(fam, &block);
    }

    #[test]
    fn parse_router_auth_tlv_never_panics_and_borrows_from_input(
        fam in prop_oneof![Just(0x11u8), Just(0x21u8), any::<u8>()],
        payload in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        if let Some(v) = parse_router_auth_tlv(fam, &payload) {
            let start = v.as_ptr() as usize;
            let base = payload.as_ptr() as usize;
            prop_assert!(start >= base);
            prop_assert!(start + v.len() <= base + payload.len());
        }
    }

    /// TLV records whose declared lengths lie must read as absent, not trusted.
    #[test]
    fn lying_tlv_lengths_never_panic(
        fam in prop_oneof![Just(0x11u8), Just(0x21u8)],
        block in proptest::collection::vec(any::<u8>(), 36..37),
        ty in any::<u8>(),
        len in any::<u16>(),
        value in proptest::collection::vec(any::<u8>(), 0..16),
    ) {
        let block_len = if fam == 0x11 { 12 } else { 36 };
        let mut payload = block[..block_len].to_vec();
        payload.push(ty);
        payload.extend_from_slice(&len.to_be_bytes());
        payload.extend_from_slice(&value);
        let got = parse_router_auth_tlv(fam, &payload);
        if usize::from(len) > value.len() {
            prop_assert!(got.is_none());
        }
    }

    /// Whatever the encoder writes, the three parsers read back exactly.
    #[test]
    fn encoder_output_roundtrips(
        src in arb_addr(),
        dst in arb_addr(),
        auth in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..64)),
    ) {
        let buf = encode_v2_authed(src, dst, auth.as_deref());
        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&buf[..PREFIX_LEN]);
        let info = parse_prefix(&prefix).expect("encoder writes a valid prefix");
        prop_assert!(info.is_proxy_command);
        prop_assert_eq!(buf.len(), PREFIX_LEN + info.addr_len);
        let payload = &buf[PREFIX_LEN..];
        prop_assert_eq!(parse_src(info.fam_proto, payload), Some(src));
        prop_assert_eq!(parse_router_auth_tlv(info.fam_proto, payload), auth.as_deref());
    }
}
