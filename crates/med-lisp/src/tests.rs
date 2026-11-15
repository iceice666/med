use crate::{Error, ErrorKind, Interp, Span, Value, read};

fn eval(source: &str) -> Value {
    match Interp::new().eval_str(source, &mut ()) {
        Ok(value) => value,
        Err(error) => panic!("{source}: {error}"),
    }
}

fn fail(source: &str) -> Error {
    match Interp::new().eval_str(source, &mut ()) {
        Ok(value) => panic!("{source}: expected an error, got {value}"),
        Err(error) => error,
    }
}

/// `source` evaluates to what `expected` is written as.
#[track_caller]
fn check(source: &str, expected: &str) {
    assert_eq!(eval(source).to_string(), expected, "{source}");
}

fn at(line: u32, column: u32) -> Option<Span> {
    Some(Span { line, column })
}

#[test]
fn reads_what_it_prints() {
    let source = r#"(a "two\n\"words\"" 3 -4.5 .5 +7 :key nil true false (nested ()) 'q - -x)"#;
    let forms = read(source).unwrap();
    assert_eq!(forms.len(), 1);
    assert_eq!(
        forms[0].to_string(),
        r#"(a "two\n\"words\"" 3 -4.5 0.5 7 :key nil true false (nested nil) (quote q) - -x)"#
    );
    // And back again.
    assert_eq!(read(&forms[0].to_string()).unwrap()[0], forms[0]);

    let forms = read("; nothing\n1 ; one\n(2) ;").unwrap();
    assert_eq!(forms.len(), 2);
    assert!(read("  \n; only a comment").unwrap().is_empty());
}

#[test]
fn says_where_text_is_not_a_program() {
    let error = |source: &str| read(source).unwrap_err();

    let unclosed = error("(a\n  (b c");
    assert_eq!(unclosed.kind, ErrorKind::Incomplete);
    assert_eq!(unclosed.span, at(2, 3));
    assert_eq!(unclosed.to_string(), "2:3: this ( is never closed");

    assert_eq!(error("\"abc").kind, ErrorKind::Incomplete);
    assert_eq!(error("'").kind, ErrorKind::Incomplete);

    let stray = error("(a))");
    assert_eq!((stray.kind, stray.span), (ErrorKind::Syntax, at(1, 4)));
    assert_eq!(error("12abc").message, "12abc is not a number");
    assert_eq!(error(r#""a\qb""#).span, at(1, 3));

    let deep = "(".repeat(100_000);
    assert_eq!(error(&deep).message, "lists nested too deeply");
}

#[test]
fn does_arithmetic() {
    check("(+ 1 2 3)", "6");
    check("(+)", "0");
    check("(- 10 3 2)", "5");
    check("(- 4)", "-4");
    check("(* 2 3.5)", "7.0");
    check("(/ 7 2)", "3");
    check("(/ 7 2.0)", "3.5");
    check("(mod 7 3)", "1");
    check("(mod -7 3)", "2");
    check("(mod 7 -3)", "-2");
    check("(< 1 2 3)", "true");
    check("(< 1 3 2)", "false");
    check("(>= 3 3 1.5)", "true");
    check("(= 1 1.0)", "true");
    check("(= '(1 (2 \"x\")) (list 1 (list 2 \"x\")))", "true");
    check("(= 'a 'b)", "false");
    check("(not nil)", "true");
    check("(not 0)", "false");

    assert_eq!(fail("(/ 1 0)").kind, ErrorKind::Arithmetic);
    assert_eq!(fail("(mod 1 0)").kind, ErrorKind::Arithmetic);
    assert_eq!(
        fail("(* 9223372036854775807 2)").kind,
        ErrorKind::Arithmetic
    );
    assert_eq!(fail("(+ 1 \"2\")").message, "expected a number, got \"2\"");
}

#[test]
fn has_the_special_forms() {
    check("(quote (a b))", "(a b)");
    check("'(1 . 2)", "(1 . 2)");
    check("(if nil 1 2)", "2");
    check("(if false 1)", "nil");
    check("(if 0 'zero-holds 'no)", "zero-holds");
    check("(def x 5) (set! x (+ x 1)) x", "6");
    check("(def x)", "x");
    check("(let ((a 1) (b (+ a 1)) c) (list a b c))", "(1 2 nil)");
    check("(do 1 2 3)", "3");
    check("(do)", "nil");
    check("(and 1 2 3)", "3");
    check("(and 1 nil (error \"not reached\"))", "nil");
    check("(and)", "true");
    check("(or nil false 3 (error \"not reached\"))", "3");
    check("(or nil false)", "false");
    check("(cond (false 1) ((= 1 1) 2 3) (true 4))", "3");
    check("(cond (false 1) (7))", "7");
    check("(cond (false 1))", "nil");
    check("(when (< 1 2) 'a 'b)", "b");
    check("(when nil 'a)", "nil");
    check("(unless nil 'a 'b)", "b");
    check(
        "(def i 0) (def total 0)
         (while (< i 5) (set! total (+ total i)) (set! i (+ i 1)))
         total",
        "10",
    );
    check(":key", ":key");

    assert_eq!(fail("(set! nope 1)").kind, ErrorKind::Unbound);
    assert_eq!(fail("(if)").kind, ErrorKind::Syntax);
    assert_eq!(fail("(def 3 4)").message, "def: expected a name, got 3");
    assert_eq!(
        fail("(let (3) 1)").message,
        "let: expected (name value), got 3"
    );
    assert_eq!(fail("(1 2)").message, "1 is not a function");
}

#[test]
fn functions_close_over_where_they_were_written() {
    check("((fn (a b) (+ a b)) 1 2)", "3");
    check(
        "(defn twice (f x) (f (f x))) (twice (fn (n) (* n n)) 3)",
        "81",
    );
    check(
        "(defn tail (a & more) more) (list (tail 1) (tail 1 2 3))",
        "(nil (2 3))",
    );
    check("((fn (& all) all))", "nil");

    // Each counter has its own count, and keeps it between calls.
    check(
        "(defn counter ()
           (let ((count 0))
             (fn () (set! count (+ count 1)) count)))
         (def a (counter))
         (def b (counter))
         (a) (a)
         (list (a) (b))",
        "(3 1)",
    );
    // Lexical, not dynamic: `f` sees the x it was written under.
    check(
        "(def x 'outer)
         (defn f () x)
         (defn g (x) (f))
         (g 'inner)",
        "outer",
    );
    // A def in a body is local to it.
    check("(defn f () (def local 1) local) (f) (def local 2) (f)", "1");
    check("(defn f () (def local 1)) (f) (fn? f)", "true");
    assert_eq!(
        fail("(defn f () (def local 1)) (f) local").kind,
        ErrorKind::Unbound
    );

    check("(def named (fn () 1)) named", "#<fn named>");
    check("(fn () 1)", "#<fn>");
    check("car", "#<native car>");

    assert_eq!(
        fail("(defn f (a b) a) (f 1)").message,
        "f: expected 2 arguments, got 1"
    );
    assert_eq!(
        fail("((fn (a & more) a))").message,
        "fn: expected at least 1 argument, got 0"
    );
    assert_eq!(fail("(car)").message, "car: expected 1 argument, got 0");
    assert_eq!(fail("(fn (a &) a)").kind, ErrorKind::Syntax);
    assert_eq!(
        fail("(fn (a 1) a)").message,
        "fn: a parameter is a name, not 1"
    );
}

#[test]
fn works_on_lists_and_text() {
    check("(list 1 \"a\" 'b)", "(1 \"a\" b)");
    check("(cons 1 '(2 3))", "(1 2 3)");
    check("(cons 1 2)", "(1 . 2)");
    check(
        "(list (first '(1 2)) (rest '(1 2)) (car nil) (cdr nil))",
        "(1 (2) nil nil)",
    );
    check(
        "(list (nth 0 '(a b)) (nth 1 '(a b)) (nth 2 '(a b)) (nth -1 '(a b)))",
        "(a b nil nil)",
    );
    check(
        "(list (length nil) (length '(1 2 3)) (length \"héllo\"))",
        "(0 3 5)",
    );
    check("(append '(1) nil '(2 3))", "(1 2 3)");
    check("(reverse '(1 2 3))", "(3 2 1)");
    check("(map (fn (n) (* n n)) '(1 2 3))", "(1 4 9)");
    check("(map car '((1 2) (3 4)))", "(1 3)");
    check("(filter (fn (n) (> n 1)) '(1 2 3))", "(2 3)");
    check("(reduce + 0 '(1 2 3 4))", "10");
    check("(reduce (fn (all n) (cons n all)) nil '(1 2 3))", "(3 2 1)");
    check("(apply + '(1 2 3))", "6");
    check(
        "(list (nil? nil) (nil? '()) (list? '(1)) (list? 1) (number? 1.5)
               (string? \"\") (symbol? 'a) (fn? car) (fn? (fn () 1)))",
        "(true true true false true true true true true)",
    );
    check(
        "(str \"a\" 1 'b :c \" \" 2.0 nil '(\"x\"))",
        "\"a1b:c 2.0nil(\\\"x\\\")\"",
    );
    check("(str)", "\"\"");

    assert_eq!(fail("(length 3)").message, "expected a list, got 3");
    assert_eq!(fail("(length (cons 1 2))").kind, ErrorKind::Type);
    assert_eq!(fail("(car 1)").kind, ErrorKind::Type);
}

#[test]
fn errors_say_where_they_happened() {
    let error = fail("(def x 1)\n(+ x\n   (car 5))");
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(error.span, at(3, 4));
    assert_eq!(error.to_string(), "3:4: expected a list, got 5 (from 2:1)");
    assert_eq!(error.trace, [Span { line: 2, column: 1 }]);

    // In the function, not at the call.
    let error = fail("(defn f (n)\n  (+ n missing))\n\n(f 1)");
    assert_eq!(error.kind, ErrorKind::Unbound);
    assert_eq!(error.to_string(), "2:3: missing is not defined (from 4:1)");

    // A lone name has only the top-level form to go by.
    assert_eq!(fail("1\n  nope").to_string(), "2:3: nope is not defined");

    let error = fail("(when true\n  (error \"bad: \" 'thing))");
    assert_eq!(error.kind, ErrorKind::Raised);
    assert_eq!(error.to_string(), "2:3: bad: thing (from 1:1)");

    // The forms a value was still owed to, from the inside out. The
    // calls on lines 4 and 6 are not: each was the last thing its form
    // did, and had handed over to the function by then.
    let error = fail(
        "(defn inner (n) (car n))
         (defn outer (n)
           (+ 1
              (inner n)))
         (list
           (outer 5))",
    );
    let lines: Vec<u32> = error.trace.iter().map(|span| span.line).collect();
    assert_eq!((error.span.unwrap().line, lines), (1, vec![3, 5]));
    assert_eq!(
        error.to_string(),
        "1:17: expected a list, got 5 (from 5:10)"
    );
    // On one line there is nothing to add.
    assert_eq!(
        fail("(+ 1 (car 5))").to_string(),
        "1:6: expected a list, got 5"
    );

    // What was defined before the error stays defined.
    let mut lisp = Interp::new();
    assert!(
        lisp.eval_str("(def kept 1) (car 5) (def lost 2)", &mut ())
            .is_err()
    );
    assert_eq!(lisp.global("kept"), Some(&Value::Int(1)));
    assert_eq!(lisp.global("lost"), None);
}

#[test]
fn tail_calls_do_not_nest() {
    // A loop of a million turns, written as a function calling itself.
    check(
        "(defn count (n total)
           (if (= n 0) total (count (- n 1) (+ total 1))))
         (count 1000000 0)",
        "1000000",
    );
    // Through cond, when, and, or, let and do, and between two functions.
    check(
        "(defn even? (n) (cond ((= n 0) true) (true (odd? (- n 1)))))
         (defn odd? (n) (and (not (= n 0)) (let ((m (- n 1))) (do (even? m)))))
         (list (even? 100000) (odd? 100000))",
        "(true false)",
    );
}

#[test]
fn deep_recursion_is_an_error_not_a_crash() {
    let source = "(defn depth (n) (if (= n 0) 0 (+ 1 (depth (- n 1)))))";
    // Half the stack a test gets, and an eighth of the main thread's.
    let thread = std::thread::Builder::new().stack_size(1024 * 1024);
    let run = move || {
        let mut lisp = Interp::new();
        lisp.eval_str(source, &mut ()).unwrap();
        assert_eq!(lisp.eval_str("(depth 300)", &mut ()), Ok(Value::Int(300)));

        let error = lisp.eval_str("(depth 100000)", &mut ()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Limit);
        assert_eq!(error.message, "calls nested too deeply");
        // The interpreter is as good as new afterwards.
        assert_eq!(lisp.eval_str("(depth 300)", &mut ()), Ok(Value::Int(300)));

        // Through a function of the host as well.
        let error = lisp
            .eval_str("(defn m (n) (map m (list n))) (m 1)", &mut ())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Limit);
    };
    thread.spawn(run).unwrap().join().unwrap();
}

#[test]
fn a_step_limit_stops_a_runaway_program() {
    let mut lisp = Interp::new();
    lisp.set_step_limit(Some(10_000));
    for source in [
        "(while true)",
        "(while true 1)",
        "(defn spin () (spin)) (spin)",
        "(defn spin (n) (spin (+ n 1))) (spin 0)",
    ] {
        let error = lisp.eval_str(source, &mut ()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Limit, "{source}");
        assert_eq!(error.message, "stopped after 10000 steps");
    }
    // Each run by the host starts counting afresh.
    for _ in 0..10 {
        let value = lisp.eval_str("(def i 0) (while (< i 1000) (set! i (+ i 1))) i", &mut ());
        assert_eq!(value, Ok(Value::Int(1000)));
    }
}

#[test]
fn the_host_defines_functions_and_calls_back() {
    struct Editor {
        bindings: Vec<(String, String)>,
    }

    let mut lisp = Interp::new();
    lisp.define_native("bind", 2, |call, args| {
        let binding = (args[0].as_str()?.to_string(), args[1].as_str()?.to_string());
        call.host::<Editor>()?.bindings.push(binding);
        Ok(Value::Nil)
    });
    lisp.define("leader", Value::from("<Space>"));

    let mut editor = Editor {
        bindings: Vec::new(),
    };
    let handler = lisp
        .eval_str(
            r#"
            (defn leader-key (key command) (bind (str leader key) command))
            (leader-key "f" "workspace.find_file")
            (leader-key "g" "workspace.toggle_git")
            ; What a click handler would be: kept by the host, called later.
            (fn (name) (bind "click" (str "tree.open " name)))
            "#,
            &mut editor,
        )
        .unwrap();
    assert_eq!(handler.to_string(), "#<fn>");
    lisp.call(&handler, &[Value::from("a.txt")], &mut editor)
        .unwrap();

    let bound = |key: &str, command: &str| (key.to_string(), command.to_string());
    assert_eq!(
        editor.bindings,
        [
            bound("<Space>f", "workspace.find_file"),
            bound("<Space>g", "workspace.toggle_git"),
            bound("click", "tree.open a.txt"),
        ]
    );

    // An error of the host's function is the program's, at the call.
    let error = lisp.eval_str("(bind \"x\"\n  1)", &mut editor).unwrap_err();
    assert_eq!(error.to_string(), "1:1: expected a string, got 1");
    assert_eq!(
        lisp.eval_str("(bind 1)", &mut editor).unwrap_err().message,
        "bind: expected 2 arguments, got 1"
    );
    // The function asks for a host that is not the one it was given.
    let error = lisp.eval_str("(bind \"x\" \"y\")", &mut ()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(
        lisp.call(&Value::Int(1), &[], &mut ()).unwrap_err().message,
        "1 is not a function"
    );
}

#[test]
fn a_long_list_is_no_deeper_than_a_short_one() {
    let numbers = || Value::list((0..1_000_000).map(Value::Int));
    let list = numbers();
    // Comparing, counting and dropping all walk it in a loop.
    assert_eq!(list, numbers());
    assert_eq!(list.iter().count(), 1_000_000);
    let mut lisp = Interp::new();
    lisp.define("numbers", list);
    assert_eq!(
        lisp.eval_str("(length (reverse numbers))", &mut ()),
        Ok(Value::Int(1_000_000))
    );
}
