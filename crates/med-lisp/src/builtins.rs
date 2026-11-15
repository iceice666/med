//! The functions every program starts out with.
//!
//! Numbers: `+ - * / mod`, `min max`, `= < > <= >=`, `not`.
//! Lists: `list cons first rest last nth length append reverse`, with `car`
//! and `cdr` as the old names of `first` and `rest`; `map filter reduce
//! apply`; `member? find-index`; `get`, for a list of names and their
//! values.
//! Kinds: `nil? list? number? string? symbol? fn?`.
//! Text: `str`, `lowercase trim replace`, `starts-with? ends-with?
//! contains?`, `lines split join`. Giving up: `error`.

use crate::eval::{Call, Interp};
use crate::value::{Arity, Error, ErrorKind, Result, Value};
use std::cmp::Ordering;

#[derive(Clone, Copy)]
enum Number {
    Int(i64),
    Float(f64),
}

impl Number {
    fn of(value: &Value) -> Result<Number> {
        match value {
            Value::Int(value) => Ok(Number::Int(*value)),
            Value::Float(value) => Ok(Number::Float(*value)),
            other => Err(Error::wrong_type("a number", other)),
        }
    }

    fn float(self) -> f64 {
        match self {
            Number::Int(value) => value as f64,
            Number::Float(value) => value,
        }
    }

    fn value(self) -> Value {
        match self {
            Number::Int(value) => Value::Int(value),
            Number::Float(value) => Value::Float(value),
        }
    }

    /// Two integers stay an integer; a float on either side makes a float.
    fn combine(
        self,
        other: Number,
        int: fn(i64, i64) -> Option<i64>,
        float: fn(f64, f64) -> f64,
    ) -> Result<Number> {
        match (self, other) {
            (Number::Int(a), Number::Int(b)) => int(a, b).map(Number::Int).ok_or_else(|| {
                Error::new(
                    ErrorKind::Arithmetic,
                    "the result does not fit in an integer",
                )
            }),
            (a, b) => Ok(Number::Float(float(a.float(), b.float()))),
        }
    }

    fn compare(self, other: Number) -> Option<Ordering> {
        match (self, other) {
            (Number::Int(a), Number::Int(b)) => Some(a.cmp(&b)),
            (a, b) => a.float().partial_cmp(&b.float()),
        }
    }
}

/// `args` combined left to right, starting from the first.
fn fold(
    args: &[Value],
    int: fn(i64, i64) -> Option<i64>,
    float: fn(f64, f64) -> f64,
) -> Result<Value> {
    let mut numbers = args.iter().map(Number::of);
    let Some(first) = numbers.next() else {
        return Ok(Value::Nil);
    };
    let mut total = first?;
    for number in numbers {
        total = total.combine(number?, int, float)?;
    }
    Ok(total.value())
}

fn divide_by_zero() -> Error {
    Error::new(ErrorKind::Arithmetic, "division by zero")
}

/// The argument that stands in `holds` to every other.
fn extreme(args: &[Value], holds: fn(Ordering) -> bool) -> Result<Value> {
    let mut best = Number::of(&args[0])?;
    for value in &args[1..] {
        let number = Number::of(value)?;
        if number.compare(best).is_some_and(holds) {
            best = number;
        }
    }
    Ok(best.value())
}

/// Whether every number stands in `holds` to the next.
fn ordered(args: &[Value], holds: fn(Ordering) -> bool) -> Result<Value> {
    let numbers: Vec<Number> = args.iter().map(Number::of).collect::<Result<_>>()?;
    let ordered = numbers
        .windows(2)
        .all(|pair| pair[0].compare(pair[1]).is_some_and(holds));
    Ok(Value::Bool(ordered))
}

fn first(_: &mut Call<'_>, args: &[Value]) -> Result<Value> {
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Pair(pair) => Ok(pair.car.clone()),
        other => Err(Error::wrong_type("a list", other)),
    }
}

fn rest(_: &mut Call<'_>, args: &[Value]) -> Result<Value> {
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Pair(pair) => Ok(pair.cdr.clone()),
        other => Err(Error::wrong_type("a list", other)),
    }
}

pub(crate) fn install(interp: &mut Interp) {
    let any = Arity::at_least(0);
    let some = Arity::at_least(1);

    // ---- numbers
    interp.define_native("+", any, |_, args| {
        if args.is_empty() {
            return Ok(Value::Int(0));
        }
        fold(args, i64::checked_add, |a, b| a + b)
    });
    interp.define_native("*", any, |_, args| {
        if args.is_empty() {
            return Ok(Value::Int(1));
        }
        fold(args, i64::checked_mul, |a, b| a * b)
    });
    interp.define_native("-", some, |_, args| match args {
        // One number: its negative.
        [only] => Ok(Number::Int(0)
            .combine(Number::of(only)?, i64::checked_sub, |a, b| a - b)?
            .value()),
        _ => fold(args, i64::checked_sub, |a, b| a - b),
    });
    // Between integers, division drops the remainder.
    interp.define_native("/", Arity::at_least(2), |_, args| {
        if args[1..].iter().any(|value| matches!(value, Value::Int(0))) {
            return Err(divide_by_zero());
        }
        fold(args, i64::checked_div, |a, b| a / b)
    });
    // The remainder, with the sign of the divisor.
    interp.define_native("mod", 2, |_, args| {
        if matches!(args[1], Value::Int(0)) {
            return Err(divide_by_zero());
        }
        fold(
            args,
            |a, b| {
                let remainder = a.checked_rem(b)?;
                let other_sign = remainder != 0 && (remainder < 0) != (b < 0);
                Some(if other_sign { remainder + b } else { remainder })
            },
            |a, b| ((a % b) + b) % b,
        )
    });
    // The least and the greatest of the arguments.
    interp.define_native("min", some, |_, args| extreme(args, Ordering::is_lt));
    interp.define_native("max", some, |_, args| extreme(args, Ordering::is_gt));
    interp.define_native("=", some, |_, args| {
        Ok(Value::Bool(args.windows(2).all(|pair| pair[0] == pair[1])))
    });
    interp.define_native("<", some, |_, args| ordered(args, Ordering::is_lt));
    interp.define_native(">", some, |_, args| ordered(args, Ordering::is_gt));
    interp.define_native("<=", some, |_, args| ordered(args, Ordering::is_le));
    interp.define_native(">=", some, |_, args| ordered(args, Ordering::is_ge));
    interp.define_native("not", 1, |_, args| Ok(Value::Bool(!args[0].is_truthy())));

    // ---- lists
    interp.define_native("list", any, |_, args| Ok(Value::list(args.iter().cloned())));
    interp.define_native("cons", 2, |_, args| {
        Ok(Value::cons(args[0].clone(), args[1].clone()))
    });
    interp.define_native("first", 1, first);
    interp.define_native("car", 1, first);
    interp.define_native("rest", 1, rest);
    interp.define_native("cdr", 1, rest);
    interp.define_native("last", 1, |_, args| {
        if !matches!(args[0], Value::Nil | Value::Pair(_)) {
            return Err(Error::wrong_type("a list", &args[0]));
        }
        Ok(args[0].iter().last().cloned().unwrap_or(Value::Nil))
    });
    // (nth index list), counting from 0; nil past the end.
    interp.define_native("nth", 2, |_, args| {
        let index = args[0].as_int()?;
        if !matches!(args[1], Value::Nil | Value::Pair(_)) {
            return Err(Error::wrong_type("a list", &args[1]));
        }
        let item = usize::try_from(index)
            .ok()
            .and_then(|index| args[1].iter().nth(index));
        Ok(item.cloned().unwrap_or(Value::Nil))
    });
    // Of a list, its values; of a string, its characters.
    interp.define_native("length", 1, |_, args| {
        let length = match &args[0] {
            Value::Str(text) => text.chars().count(),
            list => list.to_vec()?.len(),
        };
        Ok(Value::Int(length as i64))
    });
    interp.define_native("append", any, |_, args| {
        let mut items = Vec::new();
        for list in args {
            items.extend(list.to_vec()?);
        }
        Ok(Value::list(items))
    });
    interp.define_native("reverse", 1, |_, args| {
        let mut items = args[0].to_vec()?;
        items.reverse();
        Ok(Value::list(items))
    });
    interp.define_native("map", 2, |call, args| {
        let mut items = Vec::new();
        for item in args[1].to_vec()? {
            items.push(call.apply(&args[0], &[item])?);
        }
        Ok(Value::list(items))
    });
    interp.define_native("filter", 2, |call, args| {
        let mut items = Vec::new();
        for item in args[1].to_vec()? {
            if call
                .apply(&args[0], std::slice::from_ref(&item))?
                .is_truthy()
            {
                items.push(item);
            }
        }
        Ok(Value::list(items))
    });
    // (reduce function start list): function is called with the result so
    // far and the next value.
    interp.define_native("reduce", 3, |call, args| {
        let mut result = args[1].clone();
        for item in args[2].to_vec()? {
            result = call.apply(&args[0], &[result, item])?;
        }
        Ok(result)
    });
    // (member? value list): whether the list has a value equal to it.
    interp.define_native("member?", 2, |_, args| {
        if !matches!(args[1], Value::Nil | Value::Pair(_)) {
            return Err(Error::wrong_type("a list", &args[1]));
        }
        Ok(Value::Bool(args[1].iter().any(|item| *item == args[0])))
    });
    // (find-index function list): where the first value the function holds
    // for is, counting from 0, or nil.
    interp.define_native("find-index", 2, |call, args| {
        for (index, item) in args[1].to_vec()?.into_iter().enumerate() {
            if call.apply(&args[0], &[item])?.is_truthy() {
                return Ok(Value::Int(index as i64));
            }
        }
        Ok(Value::Nil)
    });
    // (apply function list): call it with the list's values as arguments.
    interp.define_native("apply", 2, |call, args| {
        call.apply(&args[0], &args[1].to_vec()?)
    });

    // (get list name [default]): the value after `name` in a list that
    // goes name, value, name, value: (get '(:a 1 :b 2) :b) is 2.
    interp.define_native("get", Arity::between(2, 3), |_, args| {
        if !matches!(args[0], Value::Nil | Value::Pair(_)) {
            return Err(Error::wrong_type("a list", &args[0]));
        }
        let mut items = args[0].iter();
        while let Some(name) = items.next() {
            let value = items.next();
            if *name == args[1] {
                return Ok(value.cloned().unwrap_or(Value::Nil));
            }
        }
        Ok(args.get(2).cloned().unwrap_or(Value::Nil))
    });

    // ---- kinds
    interp.define_native("nil?", 1, |_, args| Ok(Value::Bool(args[0].is_nil())));
    interp.define_native("list?", 1, |_, args| {
        Ok(Value::Bool(matches!(args[0], Value::Nil | Value::Pair(_))))
    });
    interp.define_native("number?", 1, |_, args| {
        Ok(Value::Bool(matches!(
            args[0],
            Value::Int(_) | Value::Float(_)
        )))
    });
    interp.define_native("string?", 1, |_, args| {
        Ok(Value::Bool(matches!(args[0], Value::Str(_))))
    });
    interp.define_native("symbol?", 1, |_, args| {
        Ok(Value::Bool(matches!(args[0], Value::Symbol(_))))
    });
    interp.define_native("fn?", 1, |_, args| {
        Ok(Value::Bool(matches!(
            args[0],
            Value::Fn(_) | Value::Native(_)
        )))
    });

    // ---- text
    // Every argument as text, one after the other.
    interp.define_native("str", any, |_, args| {
        Ok(Value::from(
            args.iter().map(Value::display).collect::<String>(),
        ))
    });
    interp.define_native("lowercase", 1, |_, args| {
        Ok(Value::from(args[0].as_str()?.to_lowercase()))
    });
    // Without the space at either end.
    interp.define_native("trim", 1, |_, args| {
        Ok(Value::from(args[0].as_str()?.trim()))
    });
    // (replace text old new): every `old` in the text made `new`.
    interp.define_native("replace", 3, |_, args| {
        let (text, old, new) = (args[0].as_str()?, args[1].as_str()?, args[2].as_str()?);
        Ok(Value::from(text.replace(old, new)))
    });
    // (starts-with? text part), (ends-with? text part), (contains? text part)
    interp.define_native("starts-with?", 2, |_, args| {
        Ok(Value::Bool(
            args[0].as_str()?.starts_with(args[1].as_str()?),
        ))
    });
    interp.define_native("ends-with?", 2, |_, args| {
        Ok(Value::Bool(args[0].as_str()?.ends_with(args[1].as_str()?)))
    });
    interp.define_native("contains?", 2, |_, args| {
        Ok(Value::Bool(args[0].as_str()?.contains(args[1].as_str()?)))
    });
    // The lines of a text, without their ends.
    interp.define_native("lines", 1, |_, args| {
        Ok(Value::list(args[0].as_str()?.lines().map(Value::from)))
    });
    // (split text separator): the parts between the separators.
    interp.define_native("split", 2, |_, args| {
        let (text, separator) = (args[0].as_str()?, args[1].as_str()?);
        if separator.is_empty() {
            return Err(Error::new(ErrorKind::Raised, "split: nothing to split at"));
        }
        Ok(Value::list(text.split(separator).map(Value::from)))
    });
    // (join list separator): the values as text, the separator between.
    interp.define_native("join", 2, |_, args| {
        let parts: Vec<String> = args[0].to_vec()?.iter().map(Value::display).collect();
        Ok(Value::from(parts.join(args[1].as_str()?)))
    });
    // Stop the program, with the arguments as the message.
    interp.define_native("error", some, |_, args| {
        let message: String = args.iter().map(Value::display).collect();
        Err(Error::new(ErrorKind::Raised, message))
    });
}
