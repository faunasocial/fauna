//! Field-path tracking for a refused strict decode.
//!
//! `serde_ipld_dagcbor`'s mismatch errors carry only the two initial bytes, so
//! a kind with several byte-carrying fields left the caller guessing which one
//! it mis-spelled. [`failing_path`] re-runs the decode of a payload that was
//! already refused through a forwarding [`serde::Deserializer`] adapter that
//! records the path (`scope.actor_id`, `scopes[1].actor_id`) of the first
//! error, and `decode_strict` puts it in front of the refusal text.
//!
//! The adapter is error-path only: the accepted-input set of
//! `decode_strict` never passes through it, so it cannot change what is
//! accepted — it only forwards every call to the real deserializer and notes
//! where an error surfaced. The mirror of the encoder's
//! `byte_array_guard` path walker.

use core::cell::RefCell;
use core::fmt;

use serde::de::{
    self, DeserializeSeed, Deserializer, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor,
};

/// The dotted path of the first error `T`'s deserialization raises on `bytes`,
/// or `None` when the error sits at the root (or the re-run does not fail).
pub(crate) fn failing_path<T: de::DeserializeOwned>(bytes: &[u8]) -> Option<String> {
    let st = RefCell::new(State::default());
    let mut de = serde_ipld_dagcbor::de::Deserializer::from_slice(bytes);
    let _ = T::deserialize(Track {
        inner: &mut de,
        st: &st,
        capture_key: false,
    });
    let failed = st.into_inner().failed?;
    (!failed.is_empty()).then_some(failed)
}

enum Seg {
    Field(String),
    Index(usize),
}

#[derive(Default)]
struct State {
    path: Vec<Seg>,
    /// The map key (or enum variant name) the last key seed read.
    key: Option<String>,
    /// The rendered path at the first error — set once, by the innermost
    /// wrapper, before the unwinding pops the path.
    failed: Option<String>,
}

impl State {
    fn record(&mut self) {
        if self.failed.is_none() {
            let mut out = String::new();
            for seg in &self.path {
                match seg {
                    Seg::Field(name) => {
                        if !out.is_empty() {
                            out.push('.');
                        }
                        out.push_str(name);
                    }
                    Seg::Index(i) => {
                        use fmt::Write;
                        let _ = write!(out, "[{i}]");
                    }
                }
            }
            self.failed = Some(out);
        }
    }
}

type St<'a> = &'a RefCell<State>;

/// Note where `r` failed, then hand it back unchanged.
fn rec<T, E>(st: St<'_>, r: Result<T, E>) -> Result<T, E> {
    if r.is_err() {
        st.borrow_mut().record();
    }
    r
}

/// A forwarding `Deserializer`. `capture_key` marks a map-key or variant-name
/// position, whose string is remembered as the next path segment.
struct Track<'a, D> {
    inner: D,
    st: St<'a>,
    capture_key: bool,
}

impl<'a, D> Track<'a, D> {
    fn visitor<V>(&self, v: V) -> TrackVisitor<'a, V> {
        TrackVisitor {
            inner: v,
            st: self.st,
            capture_key: self.capture_key,
        }
    }
}

macro_rules! forward {
    ($($method:ident),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
            let v = self.visitor(visitor);
            rec(self.st, self.inner.$method(v))
        }
    )*};
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for Track<'_, D> {
    type Error = D::Error;

    forward!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_seq,
        deserialize_map,
        deserialize_identifier,
        deserialize_ignored_any,
    );

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        let v = self.visitor(visitor);
        rec(self.st, self.inner.deserialize_unit_struct(name, v))
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        let v = self.visitor(visitor);
        rec(self.st, self.inner.deserialize_newtype_struct(name, v))
    }
    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        let v = self.visitor(visitor);
        rec(self.st, self.inner.deserialize_tuple(len, v))
    }
    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        let v = self.visitor(visitor);
        rec(self.st, self.inner.deserialize_tuple_struct(name, len, v))
    }
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        let v = self.visitor(visitor);
        rec(self.st, self.inner.deserialize_struct(name, fields, v))
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, D::Error> {
        let v = self.visitor(visitor);
        rec(self.st, self.inner.deserialize_enum(name, variants, v))
    }
    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }
}

struct TrackVisitor<'a, V> {
    inner: V,
    st: St<'a>,
    capture_key: bool,
}

macro_rules! visit_scalar {
    ($($method:ident($ty:ty)),* $(,)?) => {$(
        fn $method<E: de::Error>(self, v: $ty) -> Result<V::Value, E> {
            rec(self.st, self.inner.$method(v))
        }
    )*};
}

impl<'de, V: Visitor<'de>> Visitor<'de> for TrackVisitor<'_, V> {
    type Value = V::Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.expecting(f)
    }

    visit_scalar!(
        visit_bool(bool),
        visit_i8(i8),
        visit_i16(i16),
        visit_i32(i32),
        visit_i64(i64),
        visit_i128(i128),
        visit_u8(u8),
        visit_u16(u16),
        visit_u32(u32),
        visit_u64(u64),
        visit_u128(u128),
        visit_f32(f32),
        visit_f64(f64),
        visit_char(char),
        visit_bytes(&[u8]),
        visit_byte_buf(Vec<u8>),
    );

    fn visit_borrowed_bytes<E: de::Error>(self, v: &'de [u8]) -> Result<V::Value, E> {
        rec(self.st, self.inner.visit_borrowed_bytes(v))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<V::Value, E> {
        if self.capture_key {
            self.st.borrow_mut().key = Some(v.to_owned());
        }
        rec(self.st, self.inner.visit_str(v))
    }
    fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<V::Value, E> {
        if self.capture_key {
            self.st.borrow_mut().key = Some(v.to_owned());
        }
        rec(self.st, self.inner.visit_borrowed_str(v))
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<V::Value, E> {
        if self.capture_key {
            self.st.borrow_mut().key = Some(v.clone());
        }
        rec(self.st, self.inner.visit_string(v))
    }
    fn visit_none<E: de::Error>(self) -> Result<V::Value, E> {
        rec(self.st, self.inner.visit_none())
    }
    fn visit_unit<E: de::Error>(self) -> Result<V::Value, E> {
        rec(self.st, self.inner.visit_unit())
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<V::Value, D::Error> {
        let d = Track {
            inner: d,
            st: self.st,
            capture_key: false,
        };
        rec(self.st, self.inner.visit_some(d))
    }
    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<V::Value, D::Error> {
        let d = Track {
            inner: d,
            st: self.st,
            capture_key: false,
        };
        rec(self.st, self.inner.visit_newtype_struct(d))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<V::Value, A::Error> {
        let seq = TrackSeq {
            inner: seq,
            st: self.st,
            index: 0,
        };
        rec(self.st, self.inner.visit_seq(seq))
    }
    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<V::Value, A::Error> {
        let map = TrackMap {
            inner: map,
            st: self.st,
            index: 0,
        };
        rec(self.st, self.inner.visit_map(map))
    }
    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<V::Value, A::Error> {
        let data = TrackEnum {
            inner: data,
            st: self.st,
        };
        rec(self.st, self.inner.visit_enum(data))
    }
}

/// A seed whose value is deserialized through a [`Track`]; `capture_key`
/// marks the key / variant-name position.
struct TrackSeed<'a, S> {
    inner: S,
    st: St<'a>,
    capture_key: bool,
}

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for TrackSeed<'_, S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<S::Value, D::Error> {
        let d = Track {
            inner: d,
            st: self.st,
            capture_key: self.capture_key,
        };
        rec(self.st, self.inner.deserialize(d))
    }
}

struct TrackSeq<'a, A> {
    inner: A,
    st: St<'a>,
    index: usize,
}

impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for TrackSeq<'_, A> {
    type Error = A::Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, A::Error> {
        self.st.borrow_mut().path.push(Seg::Index(self.index));
        self.index += 1;
        let seed = TrackSeed {
            inner: seed,
            st: self.st,
            capture_key: false,
        };
        let r = rec(self.st, self.inner.next_element_seed(seed));
        if r.is_ok() {
            self.st.borrow_mut().path.pop();
        }
        r
    }
    fn size_hint(&self) -> Option<usize> {
        self.inner.size_hint()
    }
}

struct TrackMap<'a, A> {
    inner: A,
    st: St<'a>,
    index: usize,
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for TrackMap<'_, A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, A::Error> {
        self.st.borrow_mut().key = None;
        let seed = TrackSeed {
            inner: seed,
            st: self.st,
            capture_key: true,
        };
        rec(self.st, self.inner.next_key_seed(seed))
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, A::Error> {
        let seg = match self.st.borrow_mut().key.take() {
            Some(name) => Seg::Field(name),
            // A non-string key (the dag-cbor decoder refuses those anyway):
            // name the entry by position.
            None => Seg::Index(self.index),
        };
        self.index += 1;
        self.st.borrow_mut().path.push(seg);
        let seed = TrackSeed {
            inner: seed,
            st: self.st,
            capture_key: false,
        };
        let r = rec(self.st, self.inner.next_value_seed(seed));
        if r.is_ok() {
            self.st.borrow_mut().path.pop();
        }
        r
    }
    fn size_hint(&self) -> Option<usize> {
        self.inner.size_hint()
    }
}

struct TrackEnum<'a, A> {
    inner: A,
    st: St<'a>,
}

impl<'de, 'a, A: EnumAccess<'de>> EnumAccess<'de> for TrackEnum<'a, A> {
    type Error = A::Error;
    type Variant = TrackVariant<'a, A::Variant>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), A::Error> {
        let st = self.st;
        st.borrow_mut().key = None;
        let seed = TrackSeed {
            inner: seed,
            st,
            capture_key: true,
        };
        let (value, variant) = rec(st, self.inner.variant_seed(seed))?;
        // Always pushed: every `VariantAccess` method pops it.
        let name = st.borrow_mut().key.take();
        let name = name.unwrap_or_else(|| "<variant>".to_string());
        st.borrow_mut().path.push(Seg::Field(name));
        Ok((value, TrackVariant { inner: variant, st }))
    }
}

struct TrackVariant<'a, A> {
    inner: A,
    st: St<'a>,
}

/// The variant's path segment comes off once its payload decoded.
fn variant_done<T, E>(st: St<'_>, r: Result<T, E>) -> Result<T, E> {
    let r = rec(st, r);
    if r.is_ok() {
        st.borrow_mut().path.pop();
    }
    r
}

impl<'de, A: VariantAccess<'de>> VariantAccess<'de> for TrackVariant<'_, A> {
    type Error = A::Error;

    fn unit_variant(self) -> Result<(), A::Error> {
        variant_done(self.st, self.inner.unit_variant())
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, A::Error> {
        let seed = TrackSeed {
            inner: seed,
            st: self.st,
            capture_key: false,
        };
        variant_done(self.st, self.inner.newtype_variant_seed(seed))
    }
    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, A::Error> {
        let v = TrackVisitor {
            inner: visitor,
            st: self.st,
            capture_key: false,
        };
        variant_done(self.st, self.inner.tuple_variant(len, v))
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, A::Error> {
        let v = TrackVisitor {
            inner: visitor,
            st: self.st,
            capture_key: false,
        };
        variant_done(self.st, self.inner.struct_variant(fields, v))
    }
}
