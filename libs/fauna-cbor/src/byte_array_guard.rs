//! Debug-build guard: no plain-derive `[u8; N]` or `Vec<u8>` reaches the
//! canonical encoder.
//!
//! Every serialized raw-byte field is a CBOR byte string
//! (`docs/goal/architecture/serialization.md` § Canonical IPLD dag-cbor,
//! "Fixed-size byte arrays" and "Variable-length byte fields"). A bare
//! `[u8; N]` with a plain serde derive is a *tuple* to serde —
//! `serialize_tuple(N)` then N `serialize_u8` calls — and a plain `Vec<u8>`
//! a *sequence* of `serialize_u8` calls; both would land as an array of
//! integers. Neither additive-evolution gate can see that defect (a `with =`
//! attribute or a hand-written impl leaves the field's type token
//! untouched), so [`crate::encode_canonical`] runs this no-op pre-walk under
//! `debug_assertions` and refuses the value, naming the field path, the
//! moment a non-empty tuple or sequence of only `u8`s appears anywhere in
//! it. A genuine list of small integers in a serialized type gets a wider
//! element type or a newtype; an empty sequence says nothing and passes.

use core::fmt;

use serde::ser::{self, Serialize};

/// Which plain-derive raw-byte shape the guard found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByteShape {
    /// A tuple of `u8`s: a plain-derive `[u8; N]`.
    FixedArray,
    /// A sequence of `u8`s: a plain-derive `Vec<u8>`.
    Vec,
}

/// Walk `v` and return the dotted field path, and the shape, of the first
/// plain-derive `[u8; N]` or `Vec<u8>` it would serialize, if any.
pub(crate) fn find_plain_byte_field<T: Serialize + ?Sized>(v: &T) -> Option<(String, ByteShape)> {
    let mut state = State::default();
    match v.serialize(Walker { state: &mut state }) {
        Ok(Kind::Other | Kind::U8) => None,
        // An empty path is a value's own serialize failure, not a finding:
        // the real encoder reports it.
        Err(Found(path, _)) if path.is_empty() => None,
        Err(Found(path, shape)) => Some((path, shape)),
    }
}

#[derive(Default)]
struct State {
    path: Vec<String>,
    last_str: Option<String>,
}

impl State {
    fn path(&self) -> String {
        if self.path.is_empty() {
            "<root>".to_string()
        } else {
            self.path.join(".")
        }
    }
}

/// What one serialized value was, as far as the guard cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    U8,
    Other,
}

/// The guard's only error: a tuple or sequence of `u8`s at this path.
#[derive(Debug)]
struct Found(String, ByteShape);

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Found {}

impl ser::Error for Found {
    fn custom<M: fmt::Display>(_msg: M) -> Self {
        // A value's own serialize failure is the real encoder's to report;
        // `find_byte_array_tuple` reads the empty path as "no finding".
        Found(String::new(), ByteShape::FixedArray)
    }
}

struct Walker<'a> {
    state: &'a mut State,
}

struct Compound<'a> {
    state: &'a mut State,
    /// `Some((all_u8_so_far, shape))` for a plain tuple — the `[u8; N]`
    /// signature — or a sequence — the `Vec<u8>` one.
    all_u8: Option<(bool, ByteShape)>,
    index: usize,
    /// A variant's name was pushed onto the path and comes off at `end`.
    pops_variant: bool,
}

impl<'a> Compound<'a> {
    fn new(state: &'a mut State, watch: Option<ByteShape>) -> Self {
        Compound {
            state,
            all_u8: watch.map(|shape| (true, shape)),
            index: 0,
            pops_variant: false,
        }
    }

    fn variant(state: &'a mut State, variant: &'static str) -> Self {
        state.path.push(variant.to_string());
        let mut c = Compound::new(state, None);
        c.pops_variant = true;
        c
    }

    fn element<T: Serialize + ?Sized>(&mut self, seg: String, v: &T) -> Result<(), Found> {
        self.state.path.push(seg);
        let kind = v.serialize(Walker { state: self.state });
        self.state.path.pop();
        if let Some((all, _)) = self.all_u8.as_mut() {
            *all &= kind? == Kind::U8;
        } else {
            kind?;
        }
        self.index += 1;
        Ok(())
    }

    fn indexed<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Found> {
        let seg = format!("[{}]", self.index);
        self.element(seg, v)
    }

    fn end(self) -> Result<Kind, Found> {
        // Elements seen, not the size hint: a sequence's length may be
        // unknown up front, and an empty one says nothing about its type.
        if let Some((true, shape)) = self.all_u8
            && self.index > 0
        {
            return Err(Found(self.state.path(), shape));
        }
        if self.pops_variant {
            self.state.path.pop();
        }
        Ok(Kind::Other)
    }
}

impl<'a> ser::Serializer for Walker<'a> {
    type Ok = Kind;
    type Error = Found;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    /// The canonical encoder is not human-readable; a type whose serde impl
    /// branches on this must show the guard the shape the encoder will see.
    fn is_human_readable(&self) -> bool {
        false
    }

    fn serialize_u8(self, _: u8) -> Result<Kind, Found> {
        Ok(Kind::U8)
    }
    fn serialize_bool(self, _: bool) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_i8(self, _: i8) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_i16(self, _: i16) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_i32(self, _: i32) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_i64(self, _: i64) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_i128(self, _: i128) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_u16(self, _: u16) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_u32(self, _: u32) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_u64(self, _: u64) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_u128(self, _: u128) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_f32(self, _: f32) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_f64(self, _: f64) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_char(self, _: char) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_str(self, v: &str) -> Result<Kind, Found> {
        self.state.last_str = Some(v.to_string());
        Ok(Kind::Other)
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_none(self) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<Kind, Found> {
        v.serialize(self).map(|_| Kind::Other)
    }
    fn serialize_unit(self) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<Kind, Found> {
        Ok(Kind::Other)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<Kind, Found> {
        v.serialize(self).map(|_| Kind::Other)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<Kind, Found> {
        let mut c = Compound::new(self.state, None);
        c.element(variant.to_string(), v)?;
        c.end()
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Compound<'a>, Found> {
        Ok(Compound::new(self.state, Some(ByteShape::Vec)))
    }
    fn serialize_tuple(self, _: usize) -> Result<Compound<'a>, Found> {
        Ok(Compound::new(self.state, Some(ByteShape::FixedArray)))
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Compound<'a>, Found> {
        Ok(Compound::new(self.state, None))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, Found> {
        Ok(Compound::variant(self.state, variant))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Compound<'a>, Found> {
        Ok(Compound::new(self.state, None))
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Compound<'a>, Found> {
        Ok(Compound::new(self.state, None))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a>, Found> {
        Ok(Compound::variant(self.state, variant))
    }
}

impl ser::SerializeSeq for Compound<'_> {
    type Ok = Kind;
    type Error = Found;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Found> {
        self.indexed(v)
    }
    fn end(self) -> Result<Kind, Found> {
        Compound::end(self)
    }
}

impl ser::SerializeTuple for Compound<'_> {
    type Ok = Kind;
    type Error = Found;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Found> {
        self.indexed(v)
    }
    fn end(self) -> Result<Kind, Found> {
        Compound::end(self)
    }
}

impl ser::SerializeTupleStruct for Compound<'_> {
    type Ok = Kind;
    type Error = Found;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Found> {
        self.indexed(v)
    }
    fn end(self) -> Result<Kind, Found> {
        Compound::end(self)
    }
}

impl ser::SerializeTupleVariant for Compound<'_> {
    type Ok = Kind;
    type Error = Found;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Found> {
        self.indexed(v)
    }
    fn end(self) -> Result<Kind, Found> {
        Compound::end(self)
    }
}

impl ser::SerializeMap for Compound<'_> {
    type Ok = Kind;
    type Error = Found;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Found> {
        self.state.last_str = None;
        self.state.path.push("<key>".to_string());
        let r = key.serialize(Walker { state: self.state });
        self.state.path.pop();
        r.map(|_| ())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Found> {
        let seg = self
            .state
            .last_str
            .take()
            .unwrap_or_else(|| format!("[{}]", self.index));
        self.element(seg, v)
    }
    fn end(self) -> Result<Kind, Found> {
        Compound::end(self)
    }
}

impl ser::SerializeStruct for Compound<'_> {
    type Ok = Kind;
    type Error = Found;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        v: &T,
    ) -> Result<(), Found> {
        self.element(key.to_string(), v)
    }
    fn end(self) -> Result<Kind, Found> {
        Compound::end(self)
    }
}

impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = Kind;
    type Error = Found;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        v: &T,
    ) -> Result<(), Found> {
        self.element(key.to_string(), v)
    }
    fn end(self) -> Result<Kind, Found> {
        Compound::end(self)
    }
}
