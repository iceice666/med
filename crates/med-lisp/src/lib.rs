//! A small Lisp, for scripting med. Independent of the editor and of any UI.
//!
//! ```
//! use med_lisp::{Interp, Value};
//!
//! let mut lisp = Interp::new();
//!
//! // A function of the host. What it works on is handed to each run.
//! lisp.define_native("log", 1, |call, args| {
//!     call.host::<Vec<String>>()?.push(args[0].display());
//!     Ok(Value::Nil)
//! });
//!
//! let mut log: Vec<String> = Vec::new();
//! let value = lisp
//!     .eval_str(
//!         r#"
//!         (defn greet (name) (str "hello, " name))
//!         (log (greet "med"))
//!         (+ 1 2)
//!         "#,
//!         &mut log,
//!     )
//!     .unwrap();
//! assert_eq!(value, Value::Int(3));
//! assert_eq!(log, ["hello, med"]);
//! ```
//!
//! The language: lexical scope, one namespace, `nil` and `false` the only
//! values that do not hold, tail calls in constant space. The forms are
//! `quote if def defn fn let do set! and or cond when unless while`; the
//! functions are listed in `builtins.rs`. There are no macros yet.

mod builtins;
mod eval;
mod reader;
mod value;

#[cfg(test)]
mod tests;

pub use eval::{Call, Interp, Lambda};
pub use reader::read;
pub use value::{
    Arity, Error, ErrorKind, ListIter, Native, NativeFn, Pair, Result, Span, Symbol, Value,
};
