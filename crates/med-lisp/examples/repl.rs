//! Try the language: `cargo run -p med-lisp --example repl`.
//!
//! A form may run over several lines; it is evaluated once its last `)`
//! is in. `(print ...)` is this program's own function, to show how a host
//! adds one.

use med_lisp::{ErrorKind, Interp, Value, read};
use std::io::{self, BufRead, Write};

fn main() {
    let mut lisp = Interp::new();
    // A typo should not hang the terminal.
    lisp.set_step_limit(Some(50_000_000));
    lisp.define_native("print", med_lisp::Arity::at_least(0), |call, args| {
        let text: String = args.iter().map(Value::display).collect();
        call.host::<Vec<String>>()?.push(text);
        Ok(Value::Nil)
    });

    let mut input = String::new();
    let mut lines = io::stdin().lock().lines();
    loop {
        print!("{}", if input.is_empty() { "> " } else { "  " });
        io::stdout().flush().ok();
        let Some(Ok(line)) = lines.next() else {
            println!();
            return;
        };
        input.push_str(&line);
        input.push('\n');
        // Unfinished: wait for the rest.
        if matches!(read(&input), Err(error) if error.kind == ErrorKind::Incomplete) {
            continue;
        }

        let mut printed: Vec<String> = Vec::new();
        let result = lisp.eval_str(&input, &mut printed);
        for line in printed {
            println!("{line}");
        }
        match result {
            Ok(value) => println!("{value}"),
            Err(error) => println!("error: {error}"),
        }
        input.clear();
    }
}
