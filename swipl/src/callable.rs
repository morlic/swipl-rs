//! Support for calling into prolog or prolog-like code.
use crate::context::*;
use crate::engine::*;
use crate::fli::*;
use crate::functor::*;
use crate::module::*;
use crate::predicate::*;
use crate::result::*;
use crate::term::*;
use std::convert::TryInto;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use thiserror::Error;

/// Looks up a predicate on first call to `as_callable` and keeps it cached afterwards.
///
/// This is used by both the `prolog!` macro and the `pred!` macro to
/// only look up a predicate once.
pub struct LazyCallablePredicate<const N: usize> {
    module: Option<&'static str>,
    name: &'static str,
    predicate: AtomicPtr<c_void>,
}

impl<const N: usize> LazyCallablePredicate<N> {
    /// Create a new `LazyCallablepredicate` from a module, a name, and the const type argument which is used as arity.
    pub const fn new(module: Option<&'static str>, name: &'static str) -> Self {
        Self {
            module,
            name,
            predicate: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    /// Return a `CallablePredicate` from the information in this struct.
    ///
    /// If this was previously called for this struct, it'll return
    /// what it previously looked up. Otherwise, it'll do the lookup.
    pub fn as_callable(&self) -> CallablePredicate<N> {
        assert_some_engine_is_active();
        let mut loaded: predicate_t = self.predicate.load(Ordering::Relaxed) as predicate_t;
        if loaded.is_null() {
            let functor = Functor::new(self.name, N as u16);
            let module_name = self.module.unwrap_or("");
            let module = Module::new(module_name);

            loaded = Predicate::new(functor, module).predicate_ptr();

            self.predicate
                .store(loaded as *mut c_void, std::sync::atomic::Ordering::Relaxed);
        }

        unsafe { CallablePredicate::wrap(loaded) }
    }
}

impl<const N: usize> Callable<N> for LazyCallablePredicate<N> {
    type ContextType = OpenQuery;

    fn open<'a, C: ContextType>(
        self,
        context: &'a Context<C>,
        module: Option<Module>,
        args: [&Term; N],
    ) -> Context<'a, OpenQuery> {
        self.as_callable().open(context, module, args)
    }
}

impl<'a, const N: usize> Callable<N> for &'a LazyCallablePredicate<N> {
    type ContextType = OpenQuery;

    fn open<'b, C: ContextType>(
        self,
        context: &'b Context<C>,
        module: Option<Module>,
        args: [&Term; N],
    ) -> Context<'b, OpenQuery> {
        self.as_callable().open(context, module, args)
    }
}

/// Error type for turning a [Predicate](crate::predicate::Predicate) into a [CallablePredicate].
#[derive(Error, Debug)]
pub enum PredicateWrapError {
    #[error("predicate has arity {actual} but {expected} was required")]
    WrongArity { expected: u16, actual: u16 },
}

/// A prolog predicate which is ready to be called.
#[derive(Clone, Copy)]
pub struct CallablePredicate<const N: usize> {
    predicate: predicate_t,
}

impl<const N: usize> CallablePredicate<N> {
    /// Wrap a `predicate_t` from the SWI-Prolog fli, not checking if arity matches.
    ///
    /// # Safety
    /// This is only safe if the given predicate is in fact a
    /// predicate in SWI-Prolog.
    pub unsafe fn wrap(predicate: predicate_t) -> Self {
        // no check for arity or if the predicate even exists!
        Self { predicate }
    }

    /// Wrap a [Predicate](crate::predicate::Predicate).
    ///
    ///This checks that the arity matches the const generic, and
    /// panics otherwise.
    pub fn new(predicate: Predicate) -> Result<Self, PredicateWrapError> {
        assert_some_engine_is_active();
        let arity = predicate.arity();
        if arity as usize != N {
            Err(PredicateWrapError::WrongArity {
                expected: N as u16,
                actual: arity,
            })
        } else {
            Ok(unsafe { Self::wrap(predicate.predicate_ptr()) })
        }
    }
}

/// Trait for things that can be called as if they are prolog predicates.
///
/// This is obviously the case for prolog predicates themselves, which
/// are implemented through [CallablePredicate]. The intention though
/// is to also implement frontends for foreign predicates through this
/// trait, so that they may be used as prolog predicates without
/// actually having to go through prolog. However, this is currently
/// not yet implemented.
///
/// Through the const generic, this knows about the predicate arity at
/// compile time. This allows `context.open(..)` to check at compile
/// time that the arity matches.
pub trait Callable<const N: usize> {
    type ContextType: OpenCall;

    fn open<'a, C: ContextType>(
        self,
        context: &'a Context<C>,
        module: Option<Module>,
        args: [&Term; N],
    ) -> Context<'a, Self::ContextType>;
}

/// An open query.
pub struct OpenQuery {
    qid: qid_t,
    closed: bool,
}

/// An open call.
///
/// A context that wraps an open call can be asked for the next
/// solution, or close itself through cutting the query or
/// discarding. All these functions are actually implemented here
/// using the `OpenCall` trait.
///
/// This context type is the only type where new terms are not allowed
/// to be created, nor is it allowed to start new queries directly
/// from this context. This is because SWI-Prolog really doesn't like
/// it if the prolog stack changes between solutions. It is still
/// possible however to open a new frame and do all that stuff in the
/// new frame. You'll need to close the frame before continuing with
/// solution retrieval.
///
/// # Safety
/// A type implementing OpenCall automatically becomes a ContextType
/// as well, so all the safety concerns regarding ContextType apply
/// here too.
pub unsafe trait OpenCall: Sized {
    /// Retrieve the next solution.
    ///
    /// If solution retrieval led to a failure or an error, this is
    /// returned in the `Err` part of the `PrologResult`. Otherwise,
    /// `Ok(true)` is returned if there are more solutions, and
    /// `Ok(false)` is returned when this is the last solution.
    fn next_solution(this: &Context<Self>) -> PrologResult<bool>;

    /// Cut the query, keeping all data it has created.
    ///
    /// Any unifications the query did to terms from parent contexts
    /// will be retained.
    fn cut(this: Context<Self>);

    /// Discard the query, discarding all data it has created.
    ///
    /// Any unifications the query did to terms from parent contexts
    /// will be discarded.
    fn discard(this: Context<Self>);
}

impl<'a, C: OpenCall> Context<'a, C> {
    /// Retrieve the next solution.
    ///
    /// If solution retrieval led to a failure or an error, this is
    /// returned in the `Err` part of the `PrologResult`. Otherwise,
    /// `Ok(true)` is returned if there are more solutions, and
    /// `Ok(false)` is returned when this is the last solution.
    pub fn next_solution(&self) -> PrologResult<bool> {
        C::next_solution(self)
    }

    /// Cut the query, keeping all data it has created.
    ///
    /// Any unifications the query did to terms from parent contexts
    /// will be retained.
    pub fn cut(self) {
        C::cut(self)
    }

    /// Discard the query, discarding all data it has created.
    ///
    /// Any unifications the query did to terms from parent contexts
    /// will be discarded.
    pub fn discard(self) {
        C::discard(self)
    }

    /// Retrieve one result, and then cut.
    pub fn once(self) -> PrologResult<()> {
        self.next_solution()?;
        self.cut();

        Ok(())
    }

    /// Retrieve one result, ignoring failures, and then cut.
    ///
    /// Exceptions will still be returned as such.
    pub fn ignore(self) -> PrologResult<()> {
        if let Err(PrologError::Exception) = self.next_solution() {
            Err(PrologError::Exception)
        } else {
            self.cut();

            Ok(())
        }
    }
}

unsafe impl<T: OpenCall> ContextType for T {}
impl<T: OpenCall> FrameableContextType for T {}

unsafe impl OpenCall for OpenQuery {
    fn next_solution(this: &Context<Self>) -> PrologResult<bool> {
        this.assert_activated();
        let result = unsafe { PL_next_solution(this.context.qid) };
        match result {
            -1 => {
                let exception = unsafe { PL_exception(this.context.qid) };
                // rethrow this exception but as the special 0 exception which remains alive
                unsafe { PL_raise_exception(exception) };

                Err(PrologError::Exception)
            }
            0 => Err(PrologError::Failure),
            1 => Ok(true),
            2 => Ok(false),
            _ => panic!("unknown query result type {}", result),
        }
    }

    fn cut(mut this: Context<Self>) {
        this.assert_activated();
        // TODO handle exceptions

        unsafe { PL_cut_query(this.context.qid) };
        this.context.closed = true;
    }

    fn discard(mut this: Context<Self>) {
        this.assert_activated();
        // TODO handle exceptions

        unsafe { PL_close_query(this.context.qid) };
        this.context.closed = true;
    }
}

impl Drop for OpenQuery {
    fn drop(&mut self) {
        if !self.closed {
            unsafe { PL_close_query(self.qid) };
        }
    }
}

impl<const N: usize> Callable<N> for CallablePredicate<N> {
    type ContextType = OpenQuery;

    fn open<'a, C: ContextType>(
        self,
        context: &'a Context<C>,
        module: Option<Module>,
        args: [&Term; N],
    ) -> Context<'a, Self::ContextType> {
        context.assert_activated();
        context.assert_no_exception();
        let module_context = module
            .map(|c| c.module_ptr())
            .unwrap_or(std::ptr::null_mut());
        let flags = PL_Q_NORMAL | PL_Q_CATCH_EXCEPTION | PL_Q_EXT_STATUS;
        unsafe {
            let terms = PL_new_term_refs(N.try_into().unwrap());
            for (i, arg) in args.iter().enumerate() {
                let term = context.wrap_term_ref(terms + i);
                assert!(term.unify(arg).is_ok());
            }

            let qid = PL_open_query(
                module_context,
                flags.try_into().unwrap(),
                self.predicate,
                terms,
            );

            let query = OpenQuery { qid, closed: false };

            context.deactivate();
            Context::new_activated(context, query, context.engine_ptr())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;
    #[test]
    fn call_prolog_inline_slash() -> PrologResult<()> {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = term! {context: flurps(flargh)}?;
        context.call_once(pred!(writeq / 1), [&term]).unwrap();
        context.call_once(pred!(nl / 0), []).unwrap();

        Ok(())
    }

    #[test]
    fn call_prolog_inline_module_slash() -> PrologResult<()> {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = term! {context: flurps(flargh)}?;
        context.call_once(pred!(user: writeq / 1), [&term]).unwrap();
        context.call_once(pred!(user: nl / 0), []).unwrap();

        Ok(())
    }

    #[test]
    fn call_prolog_inline_str() -> PrologResult<()> {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = term! {context: flurps(flargh)}?;
        context.call_once(pred!("writeq/1"), [&term]).unwrap();
        context.call_once(pred!("nl/0"), []).unwrap();

        Ok(())
    }

    #[test]
    fn call_prolog_inline_module_str() -> PrologResult<()> {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = term! {context: flurps(flargh)}?;
        context.call_once(pred!(user:"writeq/1"), [&term]).unwrap();
        context.call_once(pred!(user:"nl/0"), []).unwrap();

        Ok(())
    }

    #[test]
    fn call_prolog_inline_str_with_module() -> PrologResult<()> {
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = term! {context: flurps(flargh)}?;
        context.call_once(pred!("user:writeq/1"), [&term]).unwrap();
        context.call_once(pred!("user:nl/0"), []).unwrap();

        Ok(())
    }
}
