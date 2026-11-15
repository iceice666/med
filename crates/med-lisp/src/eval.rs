//! The evaluator: walks the forms the reader produced.
//!
//! Names are looked up where a function was written, not where it is
//! called (lexical scope), and functions share one namespace with
//! everything else. A call in tail position reuses the caller's frame, so
//! a loop written as recursion runs in constant space.

use crate::reader;
use crate::value::{Arity, Error, ErrorKind, Form, Native, Pair, Result, Symbol, Value};
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// The names of one function call or `let`.
pub(crate) struct Scope {
    vars: RefCell<Vec<(Symbol, Value)>>,
    parent: Env,
}

/// Where names are looked up: a chain of scopes, then the globals.
pub(crate) type Env = Option<Rc<Scope>>;

impl Scope {
    fn new(parent: &Env) -> Rc<Scope> {
        Rc::new(Scope {
            vars: RefCell::new(Vec::new()),
            parent: parent.clone(),
        })
    }

    fn define(&self, name: Symbol, value: Value) {
        let mut vars = self.vars.borrow_mut();
        match vars.iter_mut().find(|(known, _)| *known == name) {
            Some(slot) => slot.1 = value,
            None => vars.push((name, value)),
        }
    }

    fn get(&self, name: Symbol) -> Option<Value> {
        let vars = self.vars.borrow();
        let (_, value) = vars.iter().find(|(known, _)| *known == name)?;
        Some(value.clone())
    }
}

/// A function written in the language, with the scope it was written in.
#[derive(Clone)]
pub struct Lambda {
    name: Option<Symbol>,
    params: Vec<Symbol>,
    /// The name the arguments past `params` go by, as a list.
    rest: Option<Symbol>,
    /// The forms of the body.
    body: Value,
    env: Env,
}

impl Lambda {
    pub fn name(&self) -> Option<Symbol> {
        self.name
    }

    /// A scope with the parameters bound to `args`.
    fn bind(&self, args: Vec<Value>) -> Result<Rc<Scope>> {
        let arity = match self.rest {
            Some(_) => Arity::at_least(self.params.len()),
            None => Arity::exactly(self.params.len()),
        };
        let name = self.name.map(Symbol::name);
        arity.check(name.as_deref().unwrap_or("fn"), args.len())?;

        let mut args = args.into_iter();
        let mut vars: Vec<(Symbol, Value)> = self.params.iter().copied().zip(&mut args).collect();
        if let Some(rest) = self.rest {
            vars.push((rest, Value::list(args)));
        }
        Ok(Rc::new(Scope {
            vars: RefCell::new(vars),
            parent: self.env.clone(),
        }))
    }
}

/// What a function of the host is handed besides its arguments.
pub struct Call<'a> {
    pub interp: &'a mut Interp,
    /// Whatever the host passed to [`Interp::eval_str`] or [`Interp::call`].
    pub host: &'a mut dyn Any,
}

impl Call<'_> {
    /// The host, which the function knows the type of.
    pub fn host<T: Any>(&mut self) -> Result<&mut T> {
        self.host.downcast_mut::<T>().ok_or_else(|| {
            Error::new(
                ErrorKind::Type,
                format!("there is no {} here", std::any::type_name::<T>()),
            )
        })
    }

    /// Call a function the program passed in.
    pub fn apply(&mut self, function: &Value, args: &[Value]) -> Result<Value> {
        self.interp.apply(function, args.to_vec(), self.host)
    }
}

/// What evaluating one form came to.
enum Step {
    Done(Value),
    /// The value is that of another form: evaluate it in place of this one.
    Tail(Value, Env),
}

/// An interpreter: the global names, and limits on how long a program runs.
pub struct Interp {
    globals: HashMap<Symbol, Value>,
    step_limit: Option<u64>,
    steps: u64,
    max_depth: usize,
    depth: usize,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    /// An interpreter with the built-in functions defined.
    pub fn new() -> Self {
        let mut interp = Self {
            globals: HashMap::new(),
            step_limit: None,
            steps: 0,
            max_depth: 400,
            depth: 0,
        };
        crate::builtins::install(&mut interp);
        interp
    }

    /// Stop a program after this many steps, a step being one form
    /// evaluated or one turn of a loop. Counted from each [`Interp::eval`],
    /// [`Interp::eval_str`] or [`Interp::call`] by the host.
    pub fn set_step_limit(&mut self, limit: Option<u64>) {
        self.step_limit = limit;
    }

    /// How deep calls that are not in tail position may nest. Each level
    /// takes room on the stack of the thread, which is what this protects.
    pub fn set_max_depth(&mut self, depth: usize) {
        self.max_depth = depth;
    }

    /// Bind a global name.
    pub fn define(&mut self, name: &str, value: Value) {
        self.globals.insert(Symbol::new(name), value);
    }

    /// The value of a global name.
    pub fn global(&self, name: &str) -> Option<&Value> {
        self.globals.get(&Symbol::new(name))
    }

    /// Define a function of the host.
    pub fn define_native(
        &mut self,
        name: &str,
        arity: impl Into<Arity>,
        run: impl Fn(&mut Call<'_>, &[Value]) -> Result<Value> + 'static,
    ) {
        let native = Native {
            name: name.into(),
            arity: arity.into(),
            run: Box::new(run),
        };
        self.define(name, Value::Native(Rc::new(native)));
    }

    /// Evaluate every form in `source`, giving the value of the last.
    /// `host` is what the host's functions will find in [`Call::host`];
    /// pass `&mut ()` when they need nothing.
    pub fn eval_str(&mut self, source: &str, host: &mut dyn Any) -> Result<Value> {
        let forms = reader::read_spanned(source)?;
        self.begin();
        let mut value = Value::Nil;
        for (form, span) in &forms {
            value = self
                .eval_in(form, &None, host)
                .map_err(|error| error.through(Some(*span)))?;
        }
        Ok(value)
    }

    /// Evaluate one form at the top level.
    pub fn eval(&mut self, form: &Value, host: &mut dyn Any) -> Result<Value> {
        self.begin();
        self.eval_in(form, &None, host)
    }

    /// Call a function of the program, such as one it registered as a
    /// handler.
    pub fn call(&mut self, function: &Value, args: &[Value], host: &mut dyn Any) -> Result<Value> {
        self.begin();
        self.apply(function, args.to_vec(), host)
    }

    /// The host is starting a program: it gets a full count of steps.
    /// Not so for a call the host makes from inside one of its functions.
    fn begin(&mut self) {
        if self.depth == 0 {
            self.steps = 0;
        }
    }

    fn tick(&mut self) -> Result<()> {
        self.steps += 1;
        match self.step_limit {
            Some(limit) if self.steps > limit => Err(Error::new(
                ErrorKind::Limit,
                format!("stopped after {limit} steps"),
            )),
            _ => Ok(()),
        }
    }

    fn lookup(&self, name: Symbol, env: &Env) -> Result<Value> {
        let mut scope = env.as_ref();
        while let Some(current) = scope {
            if let Some(value) = current.get(name) {
                return Ok(value);
            }
            scope = current.parent.as_ref();
        }
        self.globals.get(&name).cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::Unbound,
                format!("{} is not defined", name.name()),
            )
        })
    }

    fn define_in(&mut self, env: &Env, name: Symbol, value: Value) {
        match env {
            Some(scope) => scope.define(name, value),
            None => {
                self.globals.insert(name, value);
            }
        }
    }

    /// Change the nearest binding of `name`.
    fn assign(&mut self, env: &Env, name: Symbol, value: Value) -> Result<()> {
        let mut scope = env.as_ref();
        while let Some(current) = scope {
            if current.get(name).is_some() {
                current.define(name, value);
                return Ok(());
            }
            scope = current.parent.as_ref();
        }
        match self.globals.get_mut(&name) {
            Some(slot) => {
                *slot = value;
                Ok(())
            }
            None => Err(Error::new(
                ErrorKind::Unbound,
                format!("{} is not defined", name.name()),
            )),
        }
    }

    fn eval_in(&mut self, form: &Value, env: &Env, host: &mut dyn Any) -> Result<Value> {
        // Only a list takes work, and room on the stack.
        match form {
            Value::Symbol(name) => return self.lookup(*name, env),
            Value::Pair(_) => {}
            other => return Ok(other.clone()),
        }
        if self.depth >= self.max_depth {
            return Err(Error::new(ErrorKind::Limit, "calls nested too deeply"));
        }
        self.depth += 1;
        let value = self.eval_loop(form.clone(), env.clone(), host);
        self.depth -= 1;
        value
    }

    /// Evaluate `form`, then whatever it says its value is that of, and so
    /// on: tail calls go round this loop instead of down the stack.
    fn eval_loop(&mut self, mut form: Value, mut env: Env, host: &mut dyn Any) -> Result<Value> {
        loop {
            self.tick()?;
            let pair = match &form {
                Value::Symbol(name) => return self.lookup(*name, &env),
                Value::Pair(pair) => pair.clone(),
                other => return Ok(other.clone()),
            };
            match self.step(&pair, &env, host) {
                Ok(Step::Done(value)) => return Ok(value),
                Ok(Step::Tail(next, next_env)) => {
                    form = next;
                    env = next_env;
                }
                Err(error) => return Err(error.through(pair.span)),
            }
        }
    }

    fn step(&mut self, pair: &Pair, env: &Env, host: &mut dyn Any) -> Result<Step> {
        if let Value::Symbol(head) = &pair.car
            && let Some(form) = head.form()
        {
            return self.special(form, &pair.cdr, env, host);
        }

        let function = self.eval_in(&pair.car, env, host)?;
        let mut args = Vec::new();
        for arg in pair.cdr.iter() {
            args.push(self.eval_in(arg, env, host)?);
        }
        match &function {
            Value::Native(native) => {
                native.arity.check(&native.name, args.len())?;
                let mut call = Call { interp: self, host };
                Ok(Step::Done((native.run)(&mut call, &args)?))
            }
            Value::Fn(lambda) => {
                let scope = Some(lambda.bind(args)?);
                self.body(&lambda.body, &scope, host)
            }
            other => Err(Error::new(
                ErrorKind::Type,
                format!("{other} is not a function"),
            )),
        }
    }

    /// Call `function` with values already evaluated.
    pub(crate) fn apply(
        &mut self,
        function: &Value,
        args: Vec<Value>,
        host: &mut dyn Any,
    ) -> Result<Value> {
        match function {
            Value::Native(native) => {
                native.arity.check(&native.name, args.len())?;
                let mut call = Call { interp: self, host };
                (native.run)(&mut call, &args)
            }
            Value::Fn(lambda) => {
                let scope = Some(lambda.bind(args)?);
                let mut value = Value::Nil;
                for form in lambda.body.iter() {
                    value = self.eval_in(form, &scope, host)?;
                }
                Ok(value)
            }
            other => Err(Error::new(
                ErrorKind::Type,
                format!("{other} is not a function"),
            )),
        }
    }

    /// Evaluate `forms` in order. The last one is the value, in tail
    /// position.
    fn body(&mut self, forms: &Value, env: &Env, host: &mut dyn Any) -> Result<Step> {
        let mut forms = forms.iter().peekable();
        while let Some(form) = forms.next() {
            if forms.peek().is_none() {
                return Ok(Step::Tail(form.clone(), env.clone()));
            }
            self.eval_in(form, env, host)?;
        }
        Ok(Step::Done(Value::Nil))
    }

    /// A function from `(params body...)`.
    fn lambda(&self, name: Option<Symbol>, spec: &Value, env: &Env) -> Result<Lambda> {
        let mut parts = spec.iter();
        let list = match parts.next() {
            Some(list @ (Value::Nil | Value::Pair(_))) => list,
            Some(other) => {
                return Err(Error::syntax(format!(
                    "fn: expected a list of parameters, got {other}"
                )));
            }
            None => return Err(Error::syntax("fn: expected a list of parameters")),
        };
        let parameter = |value: &Value| match value {
            Value::Symbol(name) => Ok(*name),
            other => Err(Error::syntax(format!(
                "fn: a parameter is a name, not {other}"
            ))),
        };
        let mut params = Vec::new();
        let mut rest = None;
        let mut names = list.iter();
        while let Some(value) = names.next() {
            let name = parameter(value)?;
            if name != Symbol::REST {
                params.push(name);
                continue;
            }
            match (names.next(), names.next()) {
                (Some(value), None) => rest = Some(parameter(value)?),
                _ => return Err(Error::syntax("fn: expected one name after &")),
            }
        }
        Ok(Lambda {
            name,
            params,
            rest,
            body: parts.rest().clone(),
            env: env.clone(),
        })
    }

    fn special(&mut self, form: Form, args: &Value, env: &Env, host: &mut dyn Any) -> Result<Step> {
        let mut rest = args.iter();
        match form {
            // (quote x): x itself, not its value.
            Form::Quote => match (rest.next(), rest.next()) {
                (Some(value), None) => Ok(Step::Done(value.clone())),
                _ => Err(Error::syntax("quote: expected one form")),
            },

            // (if test then [else])
            Form::If => {
                let test = required(rest.next(), "if", "a test")?;
                let then = required(rest.next(), "if", "a form for when it holds")?;
                let otherwise = rest.next();
                if self.eval_in(test, env, host)?.is_truthy() {
                    Ok(Step::Tail(then.clone(), env.clone()))
                } else {
                    Ok(match otherwise {
                        Some(form) => Step::Tail(form.clone(), env.clone()),
                        None => Step::Done(Value::Nil),
                    })
                }
            }

            // (def name [value]): global at the top level, local in a body.
            Form::Def => {
                let name = name_of(rest.next(), "def")?;
                let value = match rest.next() {
                    Some(form) => self.eval_in(form, env, host)?,
                    None => Value::Nil,
                };
                // A function is known by the first name it is given.
                let value = match value {
                    Value::Fn(lambda) if lambda.name.is_none() => {
                        let mut lambda = Rc::unwrap_or_clone(lambda);
                        lambda.name = Some(name);
                        Value::Fn(Rc::new(lambda))
                    }
                    other => other,
                };
                self.define_in(env, name, value);
                Ok(Step::Done(Value::Symbol(name)))
            }

            // (defn name (params) body...)
            Form::Defn => {
                let name = name_of(rest.next(), "defn")?;
                let lambda = self.lambda(Some(name), rest.rest(), env)?;
                self.define_in(env, name, Value::Fn(Rc::new(lambda)));
                Ok(Step::Done(Value::Symbol(name)))
            }

            // (fn (params) body...), (fn (a & more) body...)
            Form::Fn => {
                let lambda = self.lambda(None, args, env)?;
                Ok(Step::Done(Value::Fn(Rc::new(lambda))))
            }

            // (let ((name value)...) body...): each value sees the names
            // before it.
            Form::Let => {
                let bindings = required(rest.next(), "let", "a list of bindings")?;
                let scope = Scope::new(env);
                let inner = Some(scope.clone());
                for binding in bindings.iter() {
                    let mut parts = binding.iter();
                    let (name, value) = match binding {
                        Value::Symbol(name) => (*name, Value::Nil),
                        Value::Pair(_) => {
                            let name = name_of(parts.next(), "let")?;
                            let value = match parts.next() {
                                Some(form) => self.eval_in(form, &inner, host)?,
                                None => Value::Nil,
                            };
                            (name, value)
                        }
                        other => {
                            return Err(Error::syntax(format!(
                                "let: expected (name value), got {other}"
                            )));
                        }
                    };
                    scope.define(name, value);
                }
                self.body(rest.rest(), &inner, host)
            }

            // (do form...)
            Form::Do => self.body(args, env, host),

            // (set! name value): change a binding that exists.
            Form::Set => {
                let name = name_of(rest.next(), "set!")?;
                let form = required(rest.next(), "set!", "a value")?;
                let value = self.eval_in(form, env, host)?;
                self.assign(env, name, value.clone())?;
                Ok(Step::Done(value))
            }

            // (and form...): the first that does not hold, else the last.
            Form::And => {
                let mut forms = rest.peekable();
                while let Some(form) = forms.next() {
                    if forms.peek().is_none() {
                        return Ok(Step::Tail(form.clone(), env.clone()));
                    }
                    let value = self.eval_in(form, env, host)?;
                    if !value.is_truthy() {
                        return Ok(Step::Done(value));
                    }
                }
                Ok(Step::Done(Value::Bool(true)))
            }

            // (or form...): the first that holds, else the last.
            Form::Or => {
                let mut forms = rest.peekable();
                while let Some(form) = forms.next() {
                    if forms.peek().is_none() {
                        return Ok(Step::Tail(form.clone(), env.clone()));
                    }
                    let value = self.eval_in(form, env, host)?;
                    if value.is_truthy() {
                        return Ok(Step::Done(value));
                    }
                }
                Ok(Step::Done(Value::Nil))
            }

            // (cond (test body...)...): the body of the first test that
            // holds, or the test's own value when it has no body.
            Form::Cond => {
                for clause in rest {
                    let mut parts = clause.iter();
                    let test = required(parts.next(), "cond", "(test body...)")?;
                    let value = self.eval_in(test, env, host)?;
                    if value.is_truthy() {
                        return match parts.rest() {
                            Value::Nil => Ok(Step::Done(value)),
                            body => self.body(body, env, host),
                        };
                    }
                }
                Ok(Step::Done(Value::Nil))
            }

            // (when test body...), (unless test body...)
            Form::When | Form::Unless => {
                let name = if form == Form::When { "when" } else { "unless" };
                let test = required(rest.next(), name, "a test")?;
                let holds = self.eval_in(test, env, host)?.is_truthy();
                if holds == (form == Form::When) {
                    self.body(rest.rest(), env, host)
                } else {
                    Ok(Step::Done(Value::Nil))
                }
            }

            // (while test body...)
            Form::While => {
                let test = required(rest.next(), "while", "a test")?;
                let body = rest.rest();
                while self.eval_in(test, env, host)?.is_truthy() {
                    // A turn with nothing to evaluate still counts.
                    self.tick()?;
                    for form in body.iter() {
                        self.eval_in(form, env, host)?;
                    }
                }
                Ok(Step::Done(Value::Nil))
            }
        }
    }
}

fn required<'a>(part: Option<&'a Value>, form: &str, what: &str) -> Result<&'a Value> {
    part.ok_or_else(|| Error::syntax(format!("{form}: expected {what}")))
}

fn name_of(part: Option<&Value>, form: &str) -> Result<Symbol> {
    match part {
        Some(Value::Symbol(name)) => Ok(*name),
        Some(other) => Err(Error::syntax(format!(
            "{form}: expected a name, got {other}"
        ))),
        None => Err(Error::syntax(format!("{form}: expected a name"))),
    }
}
