//! What a program can find out about the file being edited, and do to it.
//!
//! ```lisp
//! (cursor)                        ; (line column), both counted from 1
//! (set-cursor 12 1)
//! (mode)                          ; 'normal 'insert 'visual 'visual-line
//!                                 ; 'visual-block 'command
//! (selection)                     ; (:start (l c) :end (l c) :text :kind), or nil
//! (line-count)
//! (buffer-line 3)                 ; a line's text, or nil past the end
//! (buffer-text)                   ; all of it
//! (buffer-text '(2 1) '(4 1))     ; from a place up to another
//! (visible-lines)                 ; (first last), what is on screen
//!
//! (insert "text")                 ; at the cursor, which ends up after it
//! (edit '(2 1) '(2 4) "let")      ; in place of what is between two places
//! (feed-keys "ciwfoo<Esc>")       ; type keys, as the keyboard would
//!
//! (diagnostics)                   ; ((:line :col :end-line :end-col
//!                                 ;   :severity :message)...) of the file
//! (all-diagnostics)               ; of every file: (:path :file :line :col
//!                                 ;   :text :severity), as `locations` has them
//!
//! (decorate 'todo                 ; marks of a program's own on the file:
//!   (list (list :line 3 :sign "!" :color 'warning)
//!         (list :line 3 :text "fix this" :color 'muted-foreground)
//!         (list :start '(5 1) :end '(5 5) :bg 'warning)))
//! (decorate 'todo nil)            ; and none
//! ```
//!
//! A place is a `(line column)`, counted from 1 in characters; the end of
//! a range is the place just past it. A place beyond the text is the
//! nearest one in it. All of these are about the file in the tab that is
//! up, in the window the program was started from.
//!
//! A decoration is a list of names and values. With `:line` it is that
//! line's: `:sign` puts a character beside its number, `:text` words
//! after its end, and `:bg` a color behind all of it. With `:start` and
//! `:end` it is that text's, which gets `:bg` behind it and `:color`.
//! `:color` is also the color of a sign or of the words. Decorations
//! belong to a group, the name `decorate` is given first: decorating
//! under a name again replaces what that name had. They stay on their
//! text as lines come and go around it.

use crate::editor::{Decoration, Editor};
use crate::host::record;
use crate::panel::{color, is_function, word, wrong};
use crate::script;
use gpui::{App, Entity, SharedString};
use med_lisp::{Arity, Error, ErrorKind, Interp, Value};
use med_vim::{Mode, parse_keys};

fn failed(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Raised, message)
}

/// The editor of the tab that is up, where the program was started from.
fn editor(name: &str, cx: &mut App) -> Result<Entity<Editor>, Error> {
    script::in_workspace(cx, |workspace, _, _| workspace.active_editor().cloned())
        .flatten()
        .ok_or_else(|| failed(format!("{name}: there is no file being edited")))
}

/// A `(line column)`, both from 1, as it is counted inside: from 0.
fn place(name: &str, value: &Value) -> Result<(usize, usize), Error> {
    let from_one = |value: &Value| match value {
        Value::Int(number) if *number >= 1 => Some(*number as usize - 1),
        _ => None,
    };
    let parts = value.to_vec().ok();
    match parts.as_deref() {
        Some([line, col]) => from_one(line).zip(from_one(col)),
        _ => None,
    }
    .ok_or_else(|| wrong(name, "a place, (line column) counted from 1", value))
}

fn place_value(editor: &Editor, offset: usize) -> Value {
    let point = editor.buf.offset_to_point(offset);
    Value::list([
        Value::Int(point.row as i64 + 1),
        Value::Int(point.col as i64 + 1),
    ])
}

/// The value named `key` in a list of names and values.
pub(crate) fn field<'a>(fields: &'a [Value], key: &str) -> Option<&'a Value> {
    let key = Value::keyword(key);
    let found = fields.chunks(2).find(|pair| pair[0] == key);
    found
        .and_then(|pair| pair.get(1))
        .filter(|value| !matches!(value, Value::Nil))
}

pub(crate) fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "normal",
        Mode::Insert => "insert",
        Mode::Visual => "visual",
        Mode::VisualLine => "visual-line",
        Mode::VisualBlock => "visual-block",
        Mode::Command => "command",
    }
}

fn severity_name(severity: u8) -> &'static str {
    match severity {
        1 => "error",
        2 => "warning",
        3 => "info",
        _ => "hint",
    }
}

/// One of `decorate`'s decorations, as the editor keeps it.
fn decoration(editor: &Editor, value: &Value) -> Result<Decoration, Error> {
    let name = "decorate";
    let fields = value
        .to_vec()
        .map_err(|_| wrong(name, "a decoration, a list of names and values", value))?;
    let offset = |(line, col): (usize, usize)| editor.offset_at_place(line, col);
    let (start, end, whole_line) = match (field(&fields, "line"), field(&fields, "start")) {
        (Some(Value::Int(line)), None) if *line >= 1 => {
            let start = offset((*line as usize - 1, 0));
            (start, start, true)
        }
        (None, Some(start)) => {
            let start = offset(place(name, start)?);
            let end = match field(&fields, "end") {
                Some(end) => offset(place(name, end)?),
                None => start + 1,
            };
            (start, end.max(start), false)
        }
        _ => {
            return Err(wrong(
                name,
                "a decoration with :line, or with :start and :end",
                value,
            ));
        }
    };
    let text = |key: &str| match field(&fields, key) {
        Some(Value::Str(text)) => Ok(Some(SharedString::from(text.to_string()))),
        Some(other) => Err(wrong(key, "text", other)),
        None => Ok(None),
    };
    let paint = |key: &str| {
        field(&fields, key)
            .map(|value| color(key, value))
            .transpose()
    };
    Ok(Decoration {
        start,
        end,
        whole_line,
        sign: text("sign")?,
        text: text("text")?,
        bg: paint("bg")?,
        color: paint("color")?,
    })
}

pub(crate) fn define_functions(interp: &mut Interp) {
    // (cursor): where the cursor is.
    interp.define_native("cursor", 0, |call, _| {
        let cx = script::app(call)?;
        let editor = editor("cursor", cx)?.read(cx);
        Ok(place_value(editor, editor.vim.cursor))
    });

    // (set-cursor line column): put it somewhere else.
    interp.define_native("set-cursor", 2, |call, args| {
        let to = place(
            "set-cursor",
            &Value::list([args[0].clone(), args[1].clone()]),
        )?;
        let cx = script::app(call)?;
        editor("set-cursor", cx)?.update(cx, |editor, cx| {
            let offset = editor.offset_at_place(to.0, to.1);
            editor.move_cursor(offset, cx);
        });
        Ok(Value::Nil)
    });

    // (mode): the mode the editor is in.
    interp.define_native("mode", 0, |call, _| {
        let cx = script::app(call)?;
        let mode = editor("mode", cx)?.read(cx).vim.mode;
        Ok(Value::symbol(mode_name(mode)))
    });

    // (selection): what is selected, or nil. `:end` is the place just
    // past it. Of a block, `:text` is its lines one after the other.
    interp.define_native("selection", 0, |call, _| {
        let cx = script::app(call)?;
        let editor = editor("selection", cx)?.read(cx);
        let ranges = editor.vim.selections(&editor.buf);
        let (Some(first), Some(last)) = (ranges.first(), ranges.last()) else {
            return Ok(Value::Nil);
        };
        let pieces: Vec<String> = ranges
            .iter()
            .map(|&(start, end)| editor.buf.slice_string(start..end))
            .collect();
        let kind = match editor.vim.mode {
            Mode::VisualLine => "line",
            Mode::VisualBlock => "block",
            _ => "char",
        };
        Ok(record([
            ("start", place_value(editor, first.0)),
            ("end", place_value(editor, last.1)),
            ("text", Value::from(pieces.join("\n"))),
            ("kind", Value::symbol(kind)),
        ]))
    });

    // (line-count)
    interp.define_native("line-count", 0, |call, _| {
        let cx = script::app(call)?;
        let editor = editor("line-count", cx)?.read(cx);
        Ok(Value::Int(editor.buf.line_count() as i64))
    });

    // (buffer-line number): the text of a line, without its end; nil for
    // a line there is not.
    interp.define_native("buffer-line", 1, |call, args| {
        let line = args[0].as_int()?;
        let cx = script::app(call)?;
        let editor = editor("buffer-line", cx)?.read(cx);
        if line < 1 || line as usize > editor.buf.line_count() {
            return Ok(Value::Nil);
        }
        let line = line as usize - 1;
        let (start, end) = (editor.buf.line_start(line), editor.buf.line_end(line));
        Ok(Value::from(editor.buf.slice_string(start..end)))
    });

    // (buffer-text [start end]): the text, or what is between two places.
    interp.define_native("buffer-text", Arity::between(0, 2), |call, args| {
        let range = match args {
            [] => None,
            [start, end] => Some((place("buffer-text", start)?, place("buffer-text", end)?)),
            _ => return Err(failed("buffer-text: expected no places, or two")),
        };
        let cx = script::app(call)?;
        let editor = editor("buffer-text", cx)?.read(cx);
        Ok(Value::from(match range {
            None => editor.buf.text(),
            Some((start, end)) => {
                let start = editor.offset_at_place(start.0, start.1);
                let end = editor.offset_at_place(end.0, end.1);
                editor.buf.slice_string(start..end.max(start))
            }
        }))
    });

    // (visible-lines): the first and the last line on screen.
    interp.define_native("visible-lines", 0, |call, _| {
        let cx = script::app(call)?;
        let editor = editor("visible-lines", cx)?.read(cx);
        let (first, last) = editor.visible_lines();
        Ok(Value::list([
            Value::Int(first as i64 + 1),
            Value::Int(last as i64 + 1),
        ]))
    });

    // (insert text): at the cursor, which ends up after it.
    interp.define_native("insert", 1, |call, args| {
        let text = args[0].as_str()?.to_string();
        let cx = script::app(call)?;
        editor("insert", cx)?.update(cx, |editor, cx| {
            let cursor = editor.vim.cursor;
            editor.replace_range(cursor..cursor, &text, cx);
        });
        Ok(Value::Nil)
    });

    // (edit start end text): text in place of what is between two places.
    // One step to undo.
    interp.define_native("edit", 3, |call, args| {
        let (start, end) = (place("edit", &args[0])?, place("edit", &args[1])?);
        let text = args[2].as_str()?.to_string();
        let cx = script::app(call)?;
        editor("edit", cx)?.update(cx, |editor, cx| {
            let start = editor.offset_at_place(start.0, start.1);
            let end = editor.offset_at_place(end.0, end.1);
            editor.replace_range(start..end.max(start), &text, cx);
        });
        Ok(Value::Nil)
    });

    // (feed-keys keys): type keys, in vim notation, as the keyboard would.
    interp.define_native("feed-keys", 1, |call, args| {
        let keys = parse_keys(args[0].as_str()?);
        let cx = script::app(call)?;
        editor("feed-keys", cx)?.update(cx, |editor, cx| editor.feed_keys(keys, cx));
        Ok(Value::Nil)
    });

    // (diagnostics): what the language server says of the file.
    interp.define_native("diagnostics", 0, |call, _| {
        let cx = script::app(call)?;
        let editor = editor("diagnostics", cx)?.read(cx);
        let diagnostics = editor.diagnostics().iter().map(|diagnostic| {
            let start = editor.buf.offset_to_point(diagnostic.start);
            let end = editor.buf.offset_to_point(diagnostic.end);
            record([
                ("line", Value::Int(start.row as i64 + 1)),
                ("col", Value::Int(start.col as i64 + 1)),
                ("end-line", Value::Int(end.row as i64 + 1)),
                ("end-col", Value::Int(end.col as i64 + 1)),
                (
                    "severity",
                    Value::symbol(severity_name(diagnostic.severity)),
                ),
                ("message", Value::from(diagnostic.message.as_str())),
            ])
        });
        Ok(Value::list(diagnostics.collect::<Vec<_>>()))
    });

    // (all-diagnostics): of every file the language servers know, each a
    // place as `grep` and the `locations` hook give them, with :severity.
    interp.define_native("all-diagnostics", 0, |call, _| {
        let cx = script::app(call)?;
        let found = script::in_workspace(cx, |workspace, _, cx| workspace.all_diagnostics(cx));
        let Some(found) = found else {
            return Err(failed("all-diagnostics: there is no window"));
        };
        Ok(Value::list(found.into_iter().map(|diagnostic| {
            record([
                ("path", Value::from(diagnostic.path.display().to_string())),
                ("file", Value::from(diagnostic.file)),
                ("line", Value::Int(diagnostic.line as i64 + 1)),
                ("col", Value::Int(diagnostic.col as i64 + 1)),
                ("text", Value::from(diagnostic.message)),
                (
                    "severity",
                    Value::symbol(severity_name(diagnostic.severity)),
                ),
            ])
        })))
    });

    // (decorate group decorations): the group's marks on the file, in
    // place of the ones it had; nil for none. See the top of this file.
    interp.define_native("decorate", 2, |call, args| {
        let Some(group) = word(&args[0]) else {
            return Err(wrong("decorate", "the name of a group", &args[0]));
        };
        let given = match &args[1] {
            Value::Nil => Vec::new(),
            list => list
                .to_vec()
                .map_err(|_| wrong("decorate", "a list of decorations", list))?,
        };
        if given.iter().any(is_function) {
            return Err(failed("decorate: a decoration is a list, not a function"));
        }
        let cx = script::app(call)?;
        let editor = editor("decorate", cx)?;
        let decorations = given
            .iter()
            .map(|value| decoration(editor.read(cx), value))
            .collect::<Result<Vec<_>, _>>()?;
        let count = decorations.len();
        editor.update(cx, |editor, cx| {
            editor.set_decorations(group.into(), decorations, cx);
        });
        Ok(Value::Int(count as i64))
    });
}
