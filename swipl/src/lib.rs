pub mod fli;

pub mod consts;

pub mod atom;
pub mod functor;
pub mod module;
pub mod predicate;
pub mod result;
pub mod term;
pub mod blob;

pub mod context;
pub mod engine;

pub use swipl_macros::{predicates, prolog, term};
