use super::atom::*;
use super::context::*;
use std::convert::TryInto;
use std::os::raw::c_char;
use swipl_sys::*;

pub struct Term<'a> {
    term: term_t,
    context: &'a dyn TermOrigin,
}

impl<'a> Term<'a> {
    pub unsafe fn new(term: term_t, context: &'a dyn TermOrigin) -> Self {
        Term { term, context }
    }

    pub fn term_ptr(&self) -> term_t {
        self.term
    }

    pub fn assert_term_handling_possible<T: ContextType>(&self, context: &Context<T>) {
        if !self.context.is_engine_active() {
            panic!("term is not part of an active engine");
        }

        if self.context.origin_engine_ptr() != context.engine_ptr() {
            panic!("term unification called with a context whose engine does not match this term");
        }
    }

    pub fn unify<U: Unifiable>(&self, unifiable: U) -> bool {
        // unsafe justification: we know there is a valid context, otherwise this term would not exist. We just don't care exactly what it is.
        let context = self.context.context();
        unifiable.unify(&context, self)
    }

    pub fn get<G: TermGetable>(&self) -> Option<G> {
        // unsafe justification: we know there is a valid context, otherwise this term would not exist. We just don't care exactly what it is.
        let context = self.context.context();
        G::get(&context, self)
    }

    pub fn get_str<R, F>(&self, func: F) -> R
    where
        F: Fn(Option<&str>) -> R,
    {
        let context = self.context.context();
        self.assert_term_handling_possible(&context);
        let mut ptr = std::ptr::null_mut();
        let mut len = 0;
        let result = unsafe {
            PL_get_nchars(
                self.term,
                &mut len,
                &mut ptr,
                (CVT_STRING | REP_UTF8).try_into().unwrap(),
            )
        };
        let arg = if result == 0 {
            None
        } else {
            let swipl_string_ref =
                unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };

            let swipl_string = std::str::from_utf8(swipl_string_ref).unwrap();

            Some(swipl_string)
        };

        func(arg)
    }

    pub fn get_atom<R, F>(&self, func: F) -> R
    where
        F: Fn(Option<&Atom>) -> R,
    {
        get_atom(self, func)
    }

    pub fn get_atomable<R, F>(&self, func: F) -> R
    where
        F: Fn(Option<&Atomable>) -> R,
    {
        get_atomable(self, func)
    }
}

pub trait TermOrigin {
    fn origin_engine_ptr(&self) -> PL_engine_t;
    fn is_engine_active(&self) -> bool;
    fn context(&self) -> Context<Unknown>;
}

/// Trait for term unification.
///
/// This is marked unsafe because in order to do term unification, we
/// must be sure that
/// - the term is created on the engine which is currently active
/// - the given context is a context for this engine
///
/// Not checking those preconditions results in undefined
/// behavior. Therefore, care must be taken to ensure that `unify` is
/// actually safe.
///
/// The macro `unifiable` provides a way to safely implement this trait.
pub unsafe trait Unifiable {
    fn unify<T: ContextType>(self, context: &Context<T>, term: &Term) -> bool;
}

pub unsafe trait TermGetable: Sized {
    fn get<T: ContextType>(context: &Context<T>, term: &Term) -> Option<Self>;
}

#[macro_export]
macro_rules! unifiable {
    (($self_:ident : $t:ty, $context_: ident, $term_: ident) => $b: block) => {
        // unsafe justification: this macro inserts an assert in front
        // of the unification body, to ensure that we are given a term
        // that matches the given context, and that the currently
        // activated engine is one for which this term was created.
        unsafe impl<'a> Unifiable for $t {
            fn unify<T:ContextType>($self_, $context_: &Context<T>, $term_: &Term) -> bool {
                $term_.assert_term_handling_possible($context_);

                $b
            }
        }
    }
}

#[macro_export]
macro_rules! term_getable {
    (($t:ty, $context_: ident, $term_: ident) => $b: block) => {
        // unsafe justification: this macro inserts an assert in front
        // of the term get body, to ensure that we are given a term
        // that matches the given context, and that the currently
        // activated engine is one for which this term was created.
        unsafe impl<'a> TermGetable for $t {
            fn get<T: ContextType>($context_: &Context<T>, $term_: &Term) -> Option<Self> {
                $term_.assert_term_handling_possible($context_);

                $b
            }
        }
    };
}

unifiable! {
    (self:&Term<'a>, _context, term) => {
        if self.context.origin_engine_ptr() != term.context.origin_engine_ptr() {
            panic!("terms being unified are not part of the same engine");
        }

        // unsafe justification: the fact that we have terms here means we are dealing with some kind of active context, and therefore an initialized swipl. The checks above have made sure that both terms are part of the same engine too, and that this engine is the current engine.
        let result = unsafe { PL_unify(self.term, term.term) };

        // TODO we should actually properly test for an exception here.
        result != 0
    }
}

unifiable! {
    (self:bool, _context, term) => {
        let num = match self {
            true => 1,
            false => 0,
        };
        let result = unsafe { PL_unify_bool(term.term, num) };

        result != 0
    }
}

term_getable! {
    (bool, context, term) => {
        let mut out = 0;
        let result = unsafe { PL_get_bool(term.term, &mut out) };
        if result == 0 {
            None
        }
        else {
            Some(out != 0)
        }
    }
}

unifiable! {
    (self:u64, _context, term) => {
        let result = unsafe { PL_unify_uint64(term.term, self) };

        result != 0
    }
}

term_getable! {
    (u64, context, term) => {
        let mut out = 0;
        let result = unsafe { PL_cvt_i_uint64(term.term, &mut out) };
        if result == 0 {
            None
        }
        else {
            Some(out)
        }
    }
}

unifiable! {
    (self:i64, _context, term) => {
        let result = unsafe { PL_unify_int64(term.term, self) };

        result != 0
    }
}

term_getable! {
    (i64, context, term) => {
        let mut out = 0;
        let result = unsafe { PL_cvt_i_int64(term.term, &mut out) };
        if result == 0 {
            None
        }
        else {
            Some(out)
        }
    }
}

unifiable! {
    (self:f64, _context, term) => {
        let result = unsafe { PL_unify_float(term.term, self) };

        result != 0
    }
}

term_getable! {
    (f64, context, term) => {
        let mut out = 0.0;
        let result = unsafe { PL_get_float(term.term, &mut out) };
        if result == 0 {
            None
        }
        else {
            Some(out)
        }
    }
}

unifiable! {
    (self:&str, _context, term) => {
        let result = unsafe { PL_unify_chars(
            term.term_ptr(),
            (PL_STRING | REP_UTF8).try_into().unwrap(),
            self.len().try_into().unwrap(),
            self.as_bytes().as_ptr() as *const c_char,
        )
        };

        return result != 0;
    }
}

term_getable! {
    (String, context, term) => {
        term.get_str(|s|s.map(|s|s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use crate::context::*;
    use crate::engine::*;
    #[test]
    fn unify_some_terms_with_success() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term1 = context.new_term_ref();
        let term2 = context.new_term_ref();
        assert!(term1.unify(42_u64));
        assert!(term2.unify(42_u64));
        assert!(term1.unify(&term2));
    }

    #[test]
    fn unify_some_terms_with_failure() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term1 = context.new_term_ref();
        let term2 = context.new_term_ref();
        assert!(term1.unify(42_u64));
        assert!(term2.unify(43_u64));
        assert!(!term1.unify(&term2));
    }

    #[test]
    fn unify_twice_different_failure() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.new_term_ref();
        assert!(term.unify(42_u64));
        assert!(!term.unify(43_u64));
    }

    #[test]
    fn unify_twice_different_with_rewind_success() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();
        let term = context.new_term_ref();
        let context2 = context.open_frame();

        assert!(term.unify(42_u64));
        context2.rewind_frame();
        assert!(term.unify(43_u64));
    }

    #[test]
    fn unify_and_get_bools() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term1 = context.new_term_ref();
        assert!(term1.get::<bool>().is_none());
        term1.unify(true);
        assert_eq!(Some(true), term1.get::<bool>());
        let term2 = context.new_term_ref();
        term2.unify(false);
        assert_eq!(Some(false), term2.get::<bool>());
    }

    #[test]
    fn unify_and_get_u64s() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term1 = context.new_term_ref();
        assert!(term1.get::<u64>().is_none());
        term1.unify(42_u64);
        assert_eq!(Some(42), term1.get::<u64>());
        let term2 = context.new_term_ref();
        term2.unify(0xffffffff_u64);
        assert_eq!(Some(0xffffffff), term2.get::<u64>());
        let term3 = context.new_term_ref();
        term3.unify(0xffffffffffffffff_u64);
        assert_eq!(Some(0xffffffffffffffff), term3.get::<u64>());
    }

    #[test]
    fn unify_and_get_string_refs() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term1 = context.new_term_ref();
        term1.get_str(|s| assert!(s.is_none()));
        term1.unify("hello there");
        term1.get_str(|s| assert_eq!("hello there", s.unwrap()));
    }

    #[test]
    fn unify_and_get_strings() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term1 = context.new_term_ref();
        assert!(term1.get::<String>().is_none());
        term1.unify("hello there");
        assert_eq!("hello there", term1.get::<String>().unwrap());
    }

    #[test]
    fn unify_and_get_different_types_fails() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term1 = context.new_term_ref();
        term1.unify(42_u64);
        assert_eq!(None, term1.get::<bool>());
    }
}
