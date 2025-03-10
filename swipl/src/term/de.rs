//! Deserialization of rust values from prolog terms.
use super::ser::ATOM_STRUCT_NAME;
use super::*;
use crate::dict::*;
use crate::functor::*;
use crate::text::*;
use crate::{atom, functor};
use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use serde::Deserialize;
use std::cell::Cell;
use std::fmt::{self, Display};

/// Deserialize a term into a rust value using serde.
pub fn from_term<'a, C: QueryableContextType, T>(
    context: &'a Context<C>,
    term: &Term<'a>,
) -> Result<T>
where
    T: Deserialize<'a>,
{
    let deserializer = Deserializer {
        context,
        term: term.clone(),
    };

    Deserialize::deserialize(deserializer)
}

/// A serde deserializer for turning prolog terms into rust values.
pub struct Deserializer<'de, C: QueryableContextType> {
    context: &'de Context<'de, C>,
    term: Term<'de>,
}

impl<'de, C: QueryableContextType> Deserializer<'de, C> {
    /// Create a new deserializer.
    pub fn new(context: &'de Context<'de, C>, term: Term<'de>) -> Self {
        Self { context, term }
    }
}

/// Error type for serialization/deserialization.
#[derive(Debug)]
pub enum Error {
    Message(String),
    PrologError(PrologException),
    UnsupportedValue,
    UnexpectedType(&'static str),
    ValueNotOfExpectedType(&'static str),
    ValueOutOfRange,
    UnificationFailed,
}

impl From<PrologException> for Error {
    fn from(error: PrologException) -> Self {
        Self::PrologError(error)
    }
}

impl Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Message(msg) => formatter.write_str(msg),
            Self::PrologError(_) => formatter.write_str("prolog error"),
            Self::UnsupportedValue => formatter.write_str("unsupported value"),
            Self::UnexpectedType(t) => write!(formatter, "unexpected type {}", t),
            Self::ValueNotOfExpectedType(t) => {
                write!(formatter, "value not of expected type {}", t)
            }
            Self::ValueOutOfRange => formatter.write_str("value out of range"),
            Self::UnificationFailed => formatter.write_str("unification failed"),
        }
    }
}

impl std::error::Error for Error {}

impl de::Error for Error {
    fn custom<T>(msg: T) -> Self
    where
        T: Display,
    {
        Error::Message(msg.to_string())
    }
}

/// Result type for deserialization.
pub type Result<T> = std::result::Result<T, Error>;

struct DictMapAccess<'de, C: QueryableContextType> {
    context: &'de Context<'de, C>,
    iter: DictIterator<'de, 'de, C>,
    next_value: Option<Term<'de>>,
}

impl<'de, C: QueryableContextType> MapAccess<'de> for DictMapAccess<'de, C> {
    type Error = Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
    where
        K: DeserializeSeed<'de>,
    {
        match self.iter.next() {
            Some((key, value)) => {
                self.next_value = Some(value);

                let inner_de = KeyDeserializer { key };
                seed.deserialize(inner_de).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<K>(&mut self, seed: K) -> Result<K::Value>
    where
        K: DeserializeSeed<'de>,
    {
        let mut next_value = None;
        std::mem::swap(&mut next_value, &mut self.next_value);
        match next_value {
            Some(value) => {
                let inner_de = Deserializer {
                    context: self.context,
                    term: value,
                };
                seed.deserialize(inner_de)
            }
            None => panic!("MapAccess used out of order"),
        }
    }
}

struct CompoundTermSeqAccess<'a, C: QueryableContextType> {
    context: &'a Context<'a, C>,
    terms: Vec<Term<'a>>,
}

impl<'de, C: QueryableContextType> SeqAccess<'de> for CompoundTermSeqAccess<'de, C> {
    type Error = Error;

    fn next_element_seed<T>(&mut self, seed: T) -> std::result::Result<Option<T::Value>, Error>
    where
        T: DeserializeSeed<'de>,
    {
        if let Some(term) = self.terms.pop() {
            let inner_de = Deserializer {
                context: self.context,
                term,
            };
            seed.deserialize(inner_de).map(Some)
        } else {
            Ok(None)
        }
    }
}

struct CompoundTermEnumAccess<'a, C: QueryableContextType> {
    context: &'a Context<'a, C>,
    variant_name: String,
    term: Term<'a>,
}

impl<'de, C: QueryableContextType> EnumAccess<'de> for CompoundTermEnumAccess<'de, C> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<T>(self, seed: T) -> std::result::Result<(T::Value, Self::Variant), Error>
    where
        T: DeserializeSeed<'de>,
    {
        // this seems hugely wasteful but having the seed here requires us to go through a visitor
        let value = seed.deserialize(EnumVariantDeserializer {
            variant_name: self.variant_name.clone(),
        })?;
        Ok((value, self))
    }
}

impl<'de, C: QueryableContextType> VariantAccess<'de> for CompoundTermEnumAccess<'de, C> {
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        if self.term.is_atom() {
            Ok(())
        } else if let Some(f) = attempt_opt(self.term.get::<Functor>())? {
            if f.arity() == 0 {
                Ok(())
            } else {
                Err(Error::ValueOutOfRange)
            }
        } else {
            Err(Error::ValueOutOfRange)
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value>
    where
        T: DeserializeSeed<'de>,
    {
        if let Some([term]) = attempt_opt(self.context.compound_terms(&self.term))? {
            seed.deserialize(Deserializer {
                context: self.context,
                term,
            })
        } else {
            Err(Error::ValueOutOfRange)
        }
    }

    fn tuple_variant<V>(self, len: usize, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let inner_de = Deserializer {
            context: self.context,
            term: self.term,
        };

        de::Deserializer::deserialize_tuple(inner_de, len, visitor)
    }

    fn struct_variant<V>(self, _fields: &'static [&'static str], visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let inner_de = Deserializer {
            context: self.context,
            term: self.term,
        };

        de::Deserializer::deserialize_map(inner_de, visitor)
    }
}

struct CommaCompoundTermSeqAccess<'a, C: QueryableContextType> {
    context: &'a Context<'a, C>,
    term: Term<'a>,
}

impl<'de, C: QueryableContextType> SeqAccess<'de> for CommaCompoundTermSeqAccess<'de, C> {
    type Error = Error;

    fn next_element_seed<T>(&mut self, seed: T) -> std::result::Result<Option<T::Value>, Error>
    where
        T: DeserializeSeed<'de>,
    {
        if attempt_opt(self.term.get::<Functor>())? == Some(functor!(",/2")) {
            let [head, tail] = attempt_opt(self.context.compound_terms(&self.term))?.unwrap();
            self.term = tail;
            let inner_de = Deserializer {
                context: self.context,
                term: head,
            };
            seed.deserialize(inner_de).map(Some)
        } else {
            let inner_de = Deserializer {
                context: self.context,
                term: self.term.clone(),
            };
            seed.deserialize(inner_de).map(Some)
        }
    }
}

struct ListSeqAccess<'a, C: QueryableContextType> {
    context: &'a Context<'a, C>,
    iter: TermListIterator<'a, 'a, C>,
}

impl<'de, C: QueryableContextType> SeqAccess<'de> for ListSeqAccess<'de, C> {
    type Error = Error;

    fn next_element_seed<T>(&mut self, seed: T) -> std::result::Result<Option<T::Value>, Error>
    where
        T: DeserializeSeed<'de>,
    {
        if let Some(term) = self.iter.next() {
            let inner_de = Deserializer {
                context: self.context,
                term,
            };
            seed.deserialize(inner_de).map(Some)
        } else {
            Ok(None)
        }
    }
}

impl<'de, C: QueryableContextType> de::Deserializer<'de> for Deserializer<'de, C> {
    type Error = Error;
    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.term.term_type() {
            TermType::Atom => self.deserialize_newtype_struct(ATOM_STRUCT_NAME, visitor),
            TermType::Nil => self.deserialize_unit(visitor),
            TermType::String => self.deserialize_string(visitor),
            // TODO check signedness and call the correct one here
            TermType::Integer => self.deserialize_i64(visitor),
            TermType::Float => self.deserialize_f64(visitor),
            // we do the following inline rather than calling to
            // another deserializer cause we do not care about the
            // tuple length and don't want to check for it.
            TermType::CompoundTerm => {
                let f = attempt_opt(self.term.get::<Functor>())?.unwrap();
                if f.name() == atom!(",") && f.arity() == 2 {
                    visitor.visit_seq(CommaCompoundTermSeqAccess {
                        context: self.context,
                        term: self.term,
                    })
                } else {
                    let mut terms =
                        attempt_opt(self.context.compound_terms_vec(&self.term))?.unwrap();
                    terms.reverse();
                    visitor.visit_seq(CompoundTermSeqAccess {
                        context: self.context,
                        terms,
                    })
                }
            }
            TermType::ListPair => self.deserialize_seq(visitor),
            TermType::Dict => self.deserialize_map(visitor),
            TermType::Variable => todo!("variables are not yet supported"),
            _ => Err(Error::UnsupportedValue),
        }
    }
    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<Atom>())? {
            Some(atom) => {
                if atom == atom!("true") {
                    visitor.visit_bool(true)
                } else if atom == atom!("false") {
                    visitor.visit_bool(false)
                } else {
                    Err(Error::ValueNotOfExpectedType("bool"))
                }
            }
            None => Err(Error::ValueNotOfExpectedType("bool")),
        }
    }
    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<i64>())? {
            Some(i) => {
                if i >= i8::MIN as i64 && i <= i8::MAX as i64 {
                    visitor.visit_i8(i as i8)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            None => Err(Error::ValueNotOfExpectedType("i8")),
        }
    }
    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<i64>())? {
            Some(i) => {
                if i >= i16::MIN as i64 && i <= i16::MAX as i64 {
                    visitor.visit_i16(i as i16)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            None => Err(Error::ValueNotOfExpectedType("i16")),
        }
    }
    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<i64>())? {
            Some(i) => {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    visitor.visit_i32(i as i32)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            None => Err(Error::ValueNotOfExpectedType("i32")),
        }
    }
    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<i64>())? {
            Some(i) => visitor.visit_i64(i),
            None => Err(Error::ValueNotOfExpectedType("i64")),
        }
    }
    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<u64>())? {
            Some(i) => {
                if i <= u8::MAX as u64 {
                    visitor.visit_u8(i as u8)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            None => Err(Error::ValueNotOfExpectedType("u8")),
        }
    }
    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<u64>())? {
            Some(i) => {
                if i <= u16::MAX as u64 {
                    visitor.visit_u16(i as u16)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            None => Err(Error::ValueNotOfExpectedType("u16")),
        }
    }
    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<u64>())? {
            Some(i) => {
                if i <= u32::MAX as u64 {
                    visitor.visit_u32(i as u32)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            None => Err(Error::ValueNotOfExpectedType("u32")),
        }
    }
    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<u64>())? {
            Some(i) => visitor.visit_u64(i),
            None => Err(Error::ValueNotOfExpectedType("u64")),
        }
    }
    fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<f64>())? {
            // a little bit suspicious as this loses precision
            Some(f) => visitor.visit_f32(f as f32),
            None => Err(Error::ValueNotOfExpectedType("f32")),
        }
    }
    fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<f64>())? {
            // a little bit suspicious as this loses precision
            Some(f) => visitor.visit_f64(f),
            None => Err(Error::ValueNotOfExpectedType("f64")),
        }
    }
    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        // there's two representations in prolog, namely as a single character atom or as a code number
        match self.term.term_type() {
            TermType::Atom => {
                let c = attempt_opt(self.term.get_atom_name(|a| {
                    let mut it = a?.chars();
                    if let Some(c) = it.next() {
                        if it.next().is_none() {
                            return Some(c);
                        }
                    }

                    None
                }))?
                .expect("get_atom_name should not fail");
                match c {
                    Some(c) => visitor.visit_char(c),
                    None => Err(Error::ValueNotOfExpectedType("char")),
                }
            }
            TermType::Integer => match attempt_opt(self.term.get::<u64>())? {
                Some(i) => {
                    if i > u32::MAX as u64 {
                        Err(Error::ValueOutOfRange)
                    } else {
                        match char::from_u32(i as u32) {
                            Some(c) => visitor.visit_char(c),
                            None => Err(Error::ValueOutOfRange),
                        }
                    }
                }
                None => Err(Error::ValueNotOfExpectedType("char")),
            },
            _ => Err(Error::ValueNotOfExpectedType("char")),
        }
    }
    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<PrologText>())? {
            Some(s) => visitor.visit_string(s.into_inner()),
            None => Err(Error::ValueNotOfExpectedType("string")),
        }
    }
    fn deserialize_bytes<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnsupportedValue)
    }
    fn deserialize_byte_buf<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnsupportedValue)
    }
    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        // us being here indicates a value was present.
        visitor.visit_some(self)
    }
    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        if self.term.term_type() == TermType::Nil {
            visitor.visit_unit()
        } else {
            Err(Error::ValueNotOfExpectedType("unit"))
        }
    }
    fn deserialize_unit_struct<V>(self, _name: &'static str, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }
    fn deserialize_newtype_struct<V>(self, name: &'static str, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        if name == ATOM_STRUCT_NAME {
            if DeserializingAtomState::is_deserializing_atom() {
                let atom = attempt_opt(self.term.get::<Atom>())?;
                match atom {
                    Some(atom) => {
                        if cfg!(target_pointer_width = "32") {
                            visitor.visit_u32(atom.atom_ptr() as u32)
                        } else {
                            visitor.visit_u64(atom.atom_ptr() as u64)
                        }
                    }
                    None => Err(Error::ValueNotOfExpectedType("atom")),
                }
            } else {
                self.deserialize_string(visitor)
            }
        } else {
            visitor.visit_newtype_struct(self)
        }
    }
    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let cleanup_term = self.context.new_term_ref();
        let iter = self.context.term_list_iter(&self.term);
        let result = visitor.visit_seq(ListSeqAccess {
            context: self.context,
            iter,
        });
        unsafe {
            cleanup_term.reset();
        }

        result
    }
    fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let cleanup_term = self.context.new_term_ref();
        let result;
        if attempt_opt(self.term.get::<Functor>())? == Some(functor!(",/2")) {
            result = visitor.visit_seq(CommaCompoundTermSeqAccess {
                context: self.context,
                term: self.term,
            });
        } else if let Some(mut terms) =
            attempt_opt(self.context.compound_terms_vec_sized(&self.term, len))?
        {
            terms.reverse();
            result = visitor.visit_seq(CompoundTermSeqAccess {
                context: self.context,
                terms,
            });
        } else if self.term.term_type() == TermType::ListPair
            || self.term.term_type() == TermType::Nil
        {
            let mut terms = self.context.term_list_vec(&self.term);
            if terms.len() != len {
                result = Err(Error::ValueOutOfRange);
            } else {
                terms.reverse();

                result = visitor.visit_seq(CompoundTermSeqAccess {
                    context: self.context,
                    terms,
                });
            }
        } else {
            result = Err(Error::ValueNotOfExpectedType("tuple"));
        };

        unsafe {
            cleanup_term.reset();
        }

        result
    }
    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        // Possibly we can actually check for the functor name here.
        // But we want to serialize loosely. Us being here means the
        // user is expecting something that looks enough like a tuple
        // struct that we can make it happen. So even if the struct
        // we're deserializing into has a different name, that doesn't
        // matter.
        self.deserialize_tuple(len, visitor)
    }
    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        if self.term.term_type() == TermType::Dict {
            visitor.visit_map(DictMapAccess {
                context: self.context,
                iter: self.context.dict_entries(&self.term),
                next_value: None,
            })
        } else {
            Err(Error::ValueNotOfExpectedType("dict"))
        }
    }
    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_map(visitor)
    }
    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let variant_name;
        if let Some(Some(atom)) = attempt_opt(self.term.get_dict_tag())? {
            variant_name = atom;
        } else if let Some(functor) = attempt_opt(self.term.get::<Functor>())? {
            variant_name = functor.name();
        } else if let Some(atom) = attempt_opt(self.term.get::<Atom>())? {
            variant_name = atom;
        } else {
            return Err(Error::ValueOutOfRange);
        }

        // TODO more efficient string handling without atom reserving
        visitor.visit_enum(CompoundTermEnumAccess {
            context: self.context,
            variant_name: variant_name.to_string(),
            term: self.term,
        })
    }
    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match attempt_opt(self.term.get::<PrologText>())? {
            Some(text) => visitor.visit_string(text.to_string()),
            None => Err(Error::ValueNotOfExpectedType("identifier")),
        }
    }
    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_none()
    }
}

struct KeyDeserializer {
    key: Key,
}

impl<'de> de::Deserializer<'de> for KeyDeserializer {
    type Error = Error;
    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }
    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Atom(atom) => {
                if atom == atom!("true") {
                    visitor.visit_bool(true)
                } else if atom == atom!("false") {
                    visitor.visit_bool(false)
                } else {
                    Err(Error::ValueNotOfExpectedType("bool"))
                }
            }
            Key::Int(i) => visitor.visit_u64(i),
        }
    }
    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Int(i) => {
                if i <= i8::MAX as u64 {
                    visitor.visit_i8(i as i8)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            _ => Err(Error::ValueNotOfExpectedType("i8")),
        }
    }
    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Int(i) => {
                if i <= i16::MAX as u64 {
                    visitor.visit_i16(i as i16)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            _ => Err(Error::ValueNotOfExpectedType("i16")),
        }
    }
    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Int(i) => {
                if i <= i32::MAX as u64 {
                    visitor.visit_i32(i as i32)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            _ => Err(Error::ValueNotOfExpectedType("i32")),
        }
    }
    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Int(i) => {
                if i <= i64::MAX as u64 {
                    visitor.visit_i64(i as i64)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            _ => Err(Error::ValueNotOfExpectedType("i64")),
        }
    }
    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Int(i) => {
                if i <= u8::MAX as u64 {
                    visitor.visit_u8(i as u8)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            _ => Err(Error::ValueNotOfExpectedType("u8")),
        }
    }
    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Int(i) => {
                if i <= u16::MAX as u64 {
                    visitor.visit_u16(i as u16)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            _ => Err(Error::ValueNotOfExpectedType("u16")),
        }
    }
    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Int(i) => {
                if i <= u32::MAX as u64 {
                    visitor.visit_u32(i as u32)
                } else {
                    Err(Error::ValueOutOfRange)
                }
            }
            _ => Err(Error::ValueNotOfExpectedType("u32")),
        }
    }
    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Int(i) => visitor.visit_u64(i),
            _ => Err(Error::ValueNotOfExpectedType("u64")),
        }
    }
    fn deserialize_f32<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("f32"))
    }
    fn deserialize_f64<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("f64"))
    }
    fn deserialize_char<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("char"))
    }
    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Atom(a) => visitor.visit_string(a.to_string()),
            // dubious, maybe error
            Key::Int(i) => visitor.visit_string(i.to_string()),
        }
    }
    fn deserialize_bytes<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("bytes"))
    }
    fn deserialize_byte_buf<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("byte_buf"))
    }
    fn deserialize_option<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("option"))
    }
    fn deserialize_unit<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("unit"))
    }
    fn deserialize_unit_struct<V>(self, _name: &'static str, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("unit struct"))
    }
    fn deserialize_newtype_struct<V>(self, name: &'static str, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        // an atom key!
        if name == ATOM_STRUCT_NAME {
            if DeserializingAtomState::is_deserializing_atom() {
                match self.key {
                    Key::Atom(atom) => {
                        if cfg!(target_pointer_width = "32") {
                            visitor.visit_u32(atom.atom_ptr() as u32)
                        } else {
                            visitor.visit_u64(atom.atom_ptr() as u64)
                        }
                    }
                    _ => Err(Error::ValueNotOfExpectedType("atom")),
                }
            } else {
                self.deserialize_string(visitor)
            }
        } else {
            Err(Error::UnexpectedType("newtype struct"))
        }
    }
    fn deserialize_seq<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("seq"))
    }
    fn deserialize_tuple<V>(self, _len: usize, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("tuple"))
    }
    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("tuple struct"))
    }
    fn deserialize_map<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("map"))
    }
    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("struct"))
    }
    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("enum"))
    }
    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.key {
            Key::Atom(a) => visitor.visit_string(a.to_string()),
            // dubious, maybe error
            Key::Int(i) => visitor.visit_string(i.to_string()),
        }
    }
    fn deserialize_ignored_any<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("ignored any"))
    }
}

struct EnumVariantDeserializer {
    variant_name: String,
}

impl<'de> de::Deserializer<'de> for EnumVariantDeserializer {
    type Error = Error;
    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }
    fn deserialize_bool<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("bool"))
    }
    fn deserialize_i8<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("i8"))
    }
    fn deserialize_i16<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("i16"))
    }
    fn deserialize_i32<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("i32"))
    }
    fn deserialize_i64<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("i64"))
    }
    fn deserialize_u8<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("u8"))
    }
    fn deserialize_u16<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("u16"))
    }
    fn deserialize_u32<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("u32"))
    }
    fn deserialize_u64<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("u64"))
    }
    fn deserialize_f32<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("f32"))
    }
    fn deserialize_f64<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("f64"))
    }
    fn deserialize_char<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("char"))
    }
    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        // we already parsed the variant, so lets visit it now
        visitor.visit_string(self.variant_name)
    }
    fn deserialize_bytes<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("bytes"))
    }
    fn deserialize_byte_buf<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("byte_buf"))
    }
    fn deserialize_option<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("option"))
    }
    fn deserialize_unit<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("unit"))
    }
    fn deserialize_unit_struct<V>(self, _name: &'static str, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("unit struct"))
    }
    fn deserialize_newtype_struct<V>(self, _name: &'static str, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("newtype struct"))
    }
    fn deserialize_seq<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("seq"))
    }
    fn deserialize_tuple<V>(self, _len: usize, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("tuple"))
    }
    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("tuple struct"))
    }
    fn deserialize_map<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("map"))
    }
    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("struct"))
    }
    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("enum"))
    }
    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        // we already parsed the variant, so lets visit it now
        visitor.visit_string(self.variant_name)
    }
    fn deserialize_ignored_any<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::UnexpectedType("ignored any"))
    }
}

thread_local! {
    static DESERIALIZING_ATOM: Cell<bool> = Cell::new(false);
}

struct DeserializingAtomState;

impl DeserializingAtomState {
    fn start() -> Self {
        DESERIALIZING_ATOM.with(|da| {
            if da.get() {
                panic!("atom serialization was already set. did we recurse?");
            }
            da.set(true)
        });

        Self
    }

    fn is_deserializing_atom() -> bool {
        DESERIALIZING_ATOM.with(|da| da.get())
    }
}

impl Drop for DeserializingAtomState {
    fn drop(&mut self) {
        DESERIALIZING_ATOM.with(|da| da.set(false));
    }
}

impl<'de> Deserialize<'de> for Atom {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        let _state = DeserializingAtomState::start();
        deserializer.deserialize_newtype_struct(ATOM_STRUCT_NAME, AtomVisitor)
    }
}

struct AtomVisitor;

impl<'de> Visitor<'de> for AtomVisitor {
    type Value = Atom;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "an atom")
    }

    #[cfg(target_pointer_width = "32")]
    #[allow(clippy::useless_conversion)]
    fn visit_u32<E>(self, v: u32) -> std::result::Result<Atom, E>
    where
        E: de::Error,
    {
        if !DeserializingAtomState::is_deserializing_atom() {
            panic!("visiting atom as pointer but not in deserializing state");
        }
        let atom = unsafe { Atom::wrap((v as usize).into()) };
        atom.increment_refcount();
        Ok(atom)
    }

    #[cfg(target_pointer_width = "64")]
    #[allow(clippy::useless_conversion)]
    fn visit_u64<E>(self, v: u64) -> std::result::Result<Atom, E>
    where
        E: de::Error,
    {
        if !DeserializingAtomState::is_deserializing_atom() {
            panic!("visiting atom as pointer but not in deserializing state");
        }
        let atom = unsafe { Atom::wrap((v as usize).into()) };
        atom.increment_refcount();
        Ok(atom)
    }

    fn visit_str<E>(self, s: &str) -> std::result::Result<Atom, E>
    where
        E: de::Error,
    {
        Ok(Atom::new(s))
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> std::result::Result<Atom, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // us being here indicates we're being deserialized from an
        // unknown deserializer (not swipl). That means we have to
        // interpret as string and not as a pointer.
        deserializer.deserialize_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Baa {
        c: String,
    }

    #[derive(Deserialize, Debug, PartialEq)]
    struct Moo {
        a: String,
        b: String,
        baa: Option<Baa>,
    }

    #[test]
    fn deserialize_a_struct() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context
            .term_from_string("_{1:\"foo\",a:\"wah\",b:\"bar\", baa: _{c:\"wow\"}}")
            .unwrap();

        let result: Moo = from_term(&context, &term).unwrap();

        assert_eq!(
            Moo {
                a: "wah".to_string(),
                b: "bar".to_string(),
                baa: Some(Baa {
                    c: "wow".to_string()
                })
            },
            result
        );
    }

    #[test]
    fn deserialize_an_atom() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("foo").unwrap();

        let result: Atom = from_term(&context, &term).unwrap();

        assert_eq!(atom!("foo"), result);
    }

    use std::collections::HashMap;

    #[test]
    fn deserialize_a_hashmap() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("_{foo:bar,baz:quux}").unwrap();

        let result: HashMap<Atom, Atom> = from_term(&context, &term).unwrap();

        assert_eq!(
            HashMap::from([(atom!("foo"), atom!("bar")), (atom!("baz"), atom!("quux"))]),
            result
        );
    }

    #[test]
    fn deserialize_a_hashmap_from_number_keys() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("_{10:foo,20:bar}").unwrap();

        let result: HashMap<u8, Atom> = from_term(&context, &term).unwrap();

        assert_eq!(
            HashMap::from([(10, atom!("foo")), (20, atom!("bar"))]),
            result
        );
    }

    #[test]
    fn deserialize_a_named_tuple() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("foo(a,b,42)").unwrap();

        let result: (Atom, String, u64) = from_term(&context, &term).unwrap();

        assert_eq!((atom!("a"), "b".to_string(), 42), result);
    }

    #[derive(Deserialize, PartialEq, Debug)]
    #[serde(rename = "a_named_tuple")]
    struct ANamedTuple(Atom, Atom);

    #[test]
    fn deserialize_a_named_tuple_to_a_struct() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("a_named_tuple(foo,bar)").unwrap();

        let result: ANamedTuple = from_term(&context, &term).unwrap();

        assert_eq!(ANamedTuple(atom!("foo"), atom!("bar")), result);
    }

    #[test]
    fn deserialize_a_named_tuple_to_a_struct_with_another_name() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context
            .term_from_string("a_wrongly_named_tuple(foo,bar)")
            .unwrap();

        let result: ANamedTuple = from_term(&context, &term).unwrap();

        assert_eq!(ANamedTuple(atom!("foo"), atom!("bar")), result);
    }

    #[test]
    fn deserialize_an_unnamed_tuple() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("(a,b,42)").unwrap();

        let result: (Atom, String, u64) = from_term(&context, &term).unwrap();

        assert_eq!((atom!("a"), "b".to_string(), 42), result);
    }

    #[test]
    fn deserialize_a_list_to_a_tuple() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("[a,b,c]").unwrap();

        let result: [Atom; 3] = from_term(&context, &term).unwrap();

        assert_eq!([atom!("a"), atom!("b"), atom!("c")], result);
    }

    #[test]
    fn deserialize_a_list_to_vec() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("[a,b,c]").unwrap();

        let result: Vec<Atom> = from_term(&context, &term).unwrap();

        assert_eq!(vec![atom!("a"), atom!("b"), atom!("c")], result);
    }

    #[test]
    fn deserialize_a_list_to_const_array() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("[a,b,c]").unwrap();

        let result: [Atom; 3] = from_term(&context, &term).unwrap();

        assert_eq!([atom!("a"), atom!("b"), atom!("c")], result);
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "snake_case")]
    enum Animal {
        Cow,
        Duck(String),
        Horse(Atom, u64),
        Goat { horns: usize },
    }

    #[test]
    fn deserialize_an_enum() {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context
            .term_from_string("(cow, duck(quack), horse(neigh, 42), goat{horns: 42})")
            .unwrap();

        let result: (Animal, Animal, Animal, Animal) = from_term(&context, &term).unwrap();

        assert_eq!(
            (
                Animal::Cow,
                Animal::Duck("quack".to_string()),
                Animal::Horse(atom!("neigh"), 42),
                Animal::Goat { horns: 42 }
            ),
            result
        );
    }
}
