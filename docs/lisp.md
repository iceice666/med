English | [繁體中文](lisp.zh-TW.md)

# Scripting

## Panels

A program adds panels: sidebars to the left or right of the editor, strips
at the bottom of the window, popups over the editor, and whole screens in
its place.

```lisp
(def notes '("call Ada" "ship it"))

(defpanel 'notes :at 'right :width 240
  (fn (panel)
    (col :width 'full :height 'full :bg 'sidebar
      (text "Notes" :bold true :padding-x 12 :height 32)
      (rows notes :grow true
        (fn (note)
          (row :width 'full :height 24 :padding-x 12
               :hover (list :bg 'list-hover)
               :on-click (fn () (set! notes (filter (fn (n) (not (= n note))) notes)))
            (text note)))))))

(defcommand "user.notes" "Show or hide my notes" (fn () (toggle-panel 'notes)))
(bind 'normal "<Space>n" "user.notes")
```

`defpanel` takes a name, the options `:at` (`'left`, `'right`, `'bottom`,
`'popup`, `'full`, or `'status` and `'title` for [the bars](#the-bars)), `:width`, `:shown` (up from the start) and
`:contexts` (below), and a layout. `show-panel`, `hide-panel` and `toggle-panel` put it up and
away, and `panel-shown?` asks. A popup is as tall as what is in it, and a
click beside it or escape puts it away. A panel at the bottom is as tall as
what is in it too, goes across the window above the status bar, and stays
there under a `'full` panel; it takes no key and no click from what is
above it, which makes it the place for what is shown while a command is
being typed. `set-layout` gives a panel that is already defined another
layout.

### The bars

The status bar and the title bar are the window's own until a program
gives one a layout: a panel at `'status` is drawn in place of the status
bar, and one at `'title` in the title bar, which can still be dragged to
move the window and double-clicked to zoom it, and on macOS starts after
the window's buttons. Such a panel is up once it is defined;
`hide-panel` gives the window its own bar back, and so does a layout
that fails, which the status bar then says, until the panel is given a
layout again. With several up, the one defined last is drawn. The
window's own title bar shows the project's directory, and a click on it
runs `workspace.switch_project`: the picker's list of projects if
`picker.mlisp` is loaded, or else a directory to choose.

```lisp
(defpanel 'mine :at 'status
  (fn (panel)
    (row :width 'full :height 24 :padding-x 8 :gap 12 :align 'center :bg 'status-bar
      (text (or (get panel :cmdline) (get panel :path) "") :grow true)
      (text (str (get panel :line) ":" (get panel :col))))))
```

Besides what every panel has, a bar's `panel` has what the window's bars
show, `nil` where there is nothing to show:

| Name | Is |
| --- | --- |
| `:title` | the window's title, `"src/main.rs — med"` |
| `:directory` | the project's directory, home as `~`: `"~/code/med"` |
| `:mode`, `:mode-label` | the mode, as `(mode)` has it, and as the status bar names it: `'normal`, `"NORMAL"` |
| `:path`, `:scratch` | the file being edited, from the project's root; the name of a tab that is no file |
| `:modified` | whether it has changes that are not written |
| `:line`, `:col` | where the cursor is, counted from 1 |
| `:language` | `"rust"`, or `"text"` |
| `:cmdline` | the command line while it is typed, `":wq"` or `"/foo"` |
| `:cmdline-before`, `:cmdline-after` | the command line on each side of its cursor, `":w"` and `"q"`; the two make `:cmdline` |
| `:cmdline-preedit` | what the input method is composing at the cursor, not yet typed |
| `:message` | what was last said, until the next key |
| `:diagnostic` | the first line of the diagnostic at the cursor |
| `:pending` | the keys of a command typed so far, `"2d"` |
| `:recording` | the register a macro is being recorded into |
| `:errors`, `:warnings` | how many diagnostics of each, in every file |
| `:lsp` | what the file's language server is doing |
| `:branch` | the branch the project is on |

With no file open there is an empty `[No Name]` tab, which the next
file opened takes the place of: it has a `:mode`, but nothing that comes
from a file. A status bar that leaves out `:cmdline` has `:` typed blind.
To draw the command line's cursor, put `(caret)` between
`:cmdline-before` and the rest: it has no width, but the input method
puts its window where it is, which with no `(caret)` is the corner of
the bar.

```lisp
(row :grow true :nowrap true
  (text (get panel :cmdline-before))
  (caret)
  (text (or (get panel :cmdline-preedit) "") :underline true)
  (text (str "▏" (get panel :cmdline-after))))
```

[`examples/statusline.mlisp`](../examples/statusline.mlisp) is the window's own
status bar, and the colors of the bars are `'status-bar`, `'title-bar`,
`'title-bar-border` and `'tab-bar`.

### Layouts

How a panel looks is its layout: a function that is called each time the
panel is drawn and describes it.

| Function | Makes |
| --- | --- |
| `(row styles... children...)`, `(col ...)` | a row or a column of its children |
| `(text value styles...)` | text |
| `(rows list styles... function)` | a list that scrolls; the function describes a row |
| `(input name styles...)` | a line to type in |
| `(markdown value styles...)` | text read as Markdown: headings, lists, code, tables, links |
| `(caret styles...)` | where a bar draws the command line's cursor, for the input method |

Styles are `:name value` pairs: `width` and `height` (pixels or `'full`),
`grow` (`true`, or a share of the room next to others that grow), `shrink`,
`padding-x` `padding-y` `padding-left` `padding-right`, `gap`, `align` and
`justify` (`'start` `'center` `'end`, and `'between`), `bg`, `color`,
`border` `border-top` `border-right` `border-bottom` `border-left`,
`border-color`, `rounded`, `size` (of text), `bold`, `underline`, `mono`,
`nowrap`, `clip`; `hover`, a list of styles for under the pointer, as in
`(list :bg 'list-hover)`; and `on-click`, a command's name or a function. A
color is one of the theme's (`'sidebar`, `'muted-foreground`,
`'list-active`, `'warning`…) or `"#rrggbb"`.

A style given `nil` is left alone and a `nil` child is no child, so what
depends on a value is written with `if`, `when` and `cond`. A child can
also be a string, or a list of children, as `map` makes.

The layout's `panel` has `:name`, `:file` (the file being edited, or
`nil`), `:root` and `:focused`. Anything else a panel shows is the
program's own: what it keeps with `def` and changes with `set!`. Panels are
drawn again after any program has run, so a click that changes a value
shows at once.

`rows` draws only the rows in view, which must all be the same height. Its
function is called with one of the list's values, and with where in the
list that is if it takes a second argument. `:reveal` names the row to keep
in view and `:top` the row to have at the top: the list scrolls when either
changes.

An `input` keeps its own text. `:placeholder` is shown while it is empty,
`:on-change` is called with the text whenever something is typed, and
`:on-submit` on enter. `(input-text panel name)` reads it and `(set-input
panel name text)` writes it.

`markdown` is as tall as what it shows, unless it is given `:scroll true`:
then it is as tall as its styles make it (`:grow true`, or a `height`) and
scrolls the rest.

A layout only describes: it cannot bind keys or run commands while it is
being drawn, though the functions it gives `on-click` and an input can when
they are called. One that fails is drawn as its error message.

### The keyboard

`(focus-panel name)` puts a panel up and gives it the keyboard, which goes
on to its input if it has one; `(panel-focused? name)` asks, and
`(focus-editor)` gives the keyboard back. A click on a panel leaves the
keyboard where it was. A panel that is put away while it has the keyboard
hands it back to where it came from.

A panel with the keyboard looks its keys up in the table of its own name:

```lisp
(defcommand "user.close-notes" "Put my notes away" (fn () (hide-panel 'notes)))
(bind 'notes "q" "user.close-notes")
```

`:contexts` names other tables, most specific first: a list, or a function
that gives one, for a panel whose keys depend on what it is showing. The
git screen's is `(fn () (list (str "git." git-section) "git"))`. A key that
no table knows goes on to the panel's input.

### What a program can find out

What takes time is given a function to call with the answer, and returns
at once.

| Function | Gives |
| --- | --- |
| `(current-file)`, `(project-root)` | where the editor is |
| `(recent-projects)` | the directories windows were opened on, the last first, those still there |
| `(open-project dir)` | brings up the window on a directory: the one open on it, or a new one |
| `(buffer-language)` | the language of the tab up, as the status bar names it: `"rust"`, `"text"` for none |
| `(open-file path [line [column]])` | the file in a tab, at a place counted from 1 |
| `(list-dir path [all])` | what is in a directory: `((:name :path :dir)...)`, without what git ignores unless `all` |
| `(path-name p)`, `(path-parent p)`, `(path-join a b...)` | parts of paths |
| `(file-exists? path)`, `(dir? path)` | whether anything is there; a directory |
| `(file-info path)` | what is there, a link and not what it points to: `(:name :path :kind :dir :size :modified :mode :links :uid :gid :target)`, or `nil` |
| `(regex-match pattern text)` | the first match and each group's, or `nil` |
| `(regex-replace pattern text replacement)` | every match replaced, `$1` and `${name}` being a group's |
| `(spawn program arguments dir function)` | runs a program, in `dir` or the project for `nil`, in the project's environment; the function gets `(:status :out :err)` and `nil`, or `nil` and why it could not be run |
| `(walk-files function)` | every file of the project, as paths from its root |
| `(fuzzy query list limit)` | the texts a few letters mean, best first |
| `(grep query limit function)` | matching lines: `((:path :file :line :col :text)...)` |
| `(workspace-symbols query limit function)` | the symbols of the project the language servers running find, as `grep`'s places with the kind and name as `:text` |
| `(git what arguments... function)` | the repository: see `crates/med/src/host.rs` |
| `(bindings table [keys])` | the keys that can come after `keys` in a mode's table or a panel's |
| `(after milliseconds function)` | a call that long from now, and a timer for `(cancel timer)` to call it off |
| `(add-hook name function)` | a call when something happens |

And it changes what is on disk. Each of these gives `nil` once it is
done, or else what went wrong, and a path that does not start at the top
is the project's:

| Function | Does |
| --- | --- |
| `(rename-file from to)` | moves a file or a directory, never into the place of another; the tabs that had it go with it |
| `(copy-file from to [function])` | copies a file, or a directory with all in it, links as links, never into the place of another; given a function, in the background, and it gets what `copy-file` would give |
| `(delete-file path [forever])` | puts a file or a directory in the trash, or does away with it |
| `(make-dir path)` | a directory, and the ones it is in |
| `(make-file path)` | an empty file, never in the place of anything that is there |
| `(make-link target path [hard])` | a symbolic link at `path` to `target`, as written, or a hard one |
| `(set-file-mode path mode)` | permissions, as chmod: `493` or `"755"` |

Each window is one project. `:project dir` (`workspace.open_project`)
opens another in a window of its own, or brings up the window already on
it. A click on the title bar's directory (`workspace.switch_project`)
offers the other projects to switch to. `:q` on the last tab, `:qa`
(`workspace.quit_window`) and closing a window close only that one, and
`cmd-q` (`workspace.quit_all`) quits every window, keeping each one's session. Which panels are
up is each window's own; one the init file puts up is up in every window.
The projects
opened are listed in `projects` under `MED_STATE_DIR`. All windows share
one interpreter, so a program's names are the same in each: a program
that keeps something of a project's should keep it by `(project-root)`,
as `tree.mlisp` and `picker.mlisp` do. A command, a hook or a click runs
for the window it came from; a layout is told its window's project as
`:root`.

The hooks are `opened` (a window is up, and there is a project),
`files-changed` (a file was saved, the window came to the front, or
`workspace.sync` was run), `locations` (a list of places to choose
from, such as the references to a name, its implementations or the
symbols of the file), `keys-pending` (below),
`terminal-exited` (see Terminals), and the ones about the file being
edited (further below).

### The keys being typed

A program can follow a command as it is typed, to show what can come
next. The `keys-pending` hook is called whenever what the keys typed so
far wait for changes: with `nil` once they wait for nothing, and until
then with

| Name | Is |
| --- | --- |
| `:tables` | the key tables the next key is looked up in, the first to know it deciding: a tab's own and then its mode's, `("normal")` or `("listing.insert" "insert")`, or a panel's, `("git.files" "git")` |
| `:keys` | the start of a binding typed so far, `"<Space>"`; `""` after a count or an operator, when any binding of the table can follow |
| `:typed` | every key of the command so far, `"2dg"` |
| `:awaiting` | `'register`, `'char`, `'text-object`, `'mark` or `'flash` when the next key is not looked up but taken as it is (after `"`, `f` or `r`, `di`, `m`, `s`) |

`(bindings table keys)` lists what can follow, in the order of the keys'
notation, each `(:key :keys :command :description :to :prefix)`: the key
(`"f"`) and all the keys with it (`"<Space>f"`), the command they are
bound to and what it says of itself (`nil` for the vim engine's own
commands, which have only their names), or else the keys they stand for
(`:to`), and whether longer bindings go on from there. With no keys it
lists the first key of every binding. The `app` table cannot be listed.

```lisp
(def timer nil)
(add-hook 'keys-pending
  (fn (pending)
    (cancel timer)                      ; nil is a timer that never was
    (when pending
      (set! timer (after 300 (fn () (message (get pending :typed) " …")))))))
```

## The file being edited

A program reads the file in the tab that is up, edits it, and marks it.
A place is a `(line column)`, both counted from 1; the end of a range is
the place just past it.

| Function | Gives, or does |
| --- | --- |
| `(cursor)`, `(set-cursor line column)` | where the cursor is; puts it elsewhere |
| `(mode)` | `'normal` `'insert` `'visual` `'visual-line` `'visual-block` `'command` |
| `(selection)` | `(:start :end :text :kind)`, or `nil` |
| `(line-count)`, `(buffer-line n)` | how many lines; the text of one |
| `(modified?)` | whether it has changes that are not written |
| `(buffer-text [start end])` | all the text, or what is between two places |
| `(visible-lines)` | the first and the last line on screen |
| `(insert text)` | text at the cursor |
| `(edit start end text)` | text in place of what is between two places: one step to undo |
| `(feed-keys keys)` | types keys, as the keyboard would: `"ciwfoo<Esc>"` |
| `(diagnostics)` | the file's: `((:line :col :end-line :end-col :severity :message)...)` |
| `(all-diagnostics)` | every file's, as places: `((:path :file :line :col :text :severity)...)` |
| `(decorate group decorations)` | the group's marks on the file, in place of the ones it had |

A decoration is a list of names and values. With `:line` it is a line's:
`:sign` puts a character beside the number, `:text` words after the end,
`:bg` a color behind it all. With `:start` and `:end` it is that text's,
which gets `:bg` and `:color`. `:color` is also the color of a sign or of
the words. Decorations stay on their text as lines come and go around
it.

```lisp
(decorate 'review
  (list (list :line 12 :sign "?" :color 'warning)
        (list :line 12 :text "is this still needed" :color 'muted-foreground)
        (list :start '(14 5) :end '(14 11) :bg 'warning)))
(decorate 'review nil)                  ; and none
```

And it is told what happens, with `add-hook`:

| Hook | When, and what the function is called with |
| --- | --- |
| `buffer-entered` | another file is the one being edited: its path, or `nil` |
| `buffer-changed` | its text changed |
| `cursor-moved` | the cursor is somewhere else |
| `mode-changed` | the editor went into another mode: the mode |
| `diagnostics-changed` | a language server has news of a file: its path |

[`examples/todo.mlisp`](../examples/todo.mlisp) is a short program made of
these: it reads the text once it has changed, puts a sign on the lines
that say TODO, and has two commands that move the cursor.

A program can also have a tab of text that is no file, by a name of its
own:

```lisp
(open-scratch "notes")                       ; go to it, or make it
(open-scratch "notes" "one\ntwo")            ; with this text, as written
(open-scratch "notes" nil (fn (text) (message (length (lines text)) " lines")))
(scratch-name)                               ; "notes" while it is up, else nil
(panes)                                      ; ((:focused true :scratch "notes" :path nil :terminal nil)...)
(scratch-tabs)                               ; every such tab, shown or not: ((:name "notes" :modified false :text "one\ntwo\n")...)
(with-pane 1 (fn () (scratch-name)))         ; in the second pane, the keys staying
```

Given a function, `:w` calls it with the text, as a command
(`command-bang` is there for `:w!`), and the text stays unsaved until the program gives the tab
its text again. Without one, there is nothing in the tab to lose and
nowhere to write it. `(current-file)` is `nil` there, and `:w name`
makes the tab a file, without the tab's own keys. [`examples/dired.mlisp`](../examples/dired.mlisp) lists a directory in
one, does what Emacs's dired does to the files, and renames them with
what is written.

A tab is in one pane, so that a file has one buffer: opening a tab that
another pane has (`open-scratch`, `open-file`, `:e`) brings it over, and
that pane shows the tab before it. `(panes)` says what each pane shows,
from the top left, and `(with-pane n f)` calls `f` with the `n`th of them
(from 0) as the one being edited, the keys staying where they are: dired
reads every listing up again with it.

## Terminals

A program can start a program of its own in a terminal, in a pane split
off below that gets the keys, and talk to it. A terminal is known by a
number:

| Function | Does, or gives |
| --- | --- |
| `(open-terminal [command [dir]])` | a new terminal running `command` (with the shell, `$SHELL -c`), or the shell, in `dir` or the project's root, in the project's environment: its number. With neither, the terminal still running, if there is one |
| `(terminals)` | every terminal, from the top left pane: `((:id :title :command :running :exit)...)`, `:command` `nil` for the shell, `:exit` its exit code once it is done, `nil` until then or if it was killed |
| `(terminal-send id text)` | `text` to the program, as if typed (`"\r"` is enter); `false` if the program is done |
| `(terminal-keys id keys)` | keys to the program, as `bind` writes them: `"<C-c>"`, `"q<CR>"`; `false` if the program is done |
| `(terminal-text id)` | what went by and what is on its screen, as text, a line for each row |
| `(close-terminal id)` | closes its tab, hanging up on the program; `false` if there is no such terminal |

The `terminal-exited` hook is called with a terminal's number and its
exit code (`nil` if the program was killed) once its program is done.
`(panes)` gives the number of the terminal a pane shows as `:terminal`.

A `:make` of a sort: the build in a terminal of its own, the one before
closed, the keys going back to the file, and a word when it is done.

```lisp
(def make-terminal nil)
(defcommand "user.make" "Build the project in a terminal of its own"
  (fn ()
    (when make-terminal (close-terminal make-terminal))
    (set! make-terminal (open-terminal "cargo build"))
    (run "workspace.prev_pane")))
(add-hook 'terminal-exited
  (fn (id code)
    (when (= id make-terminal)
      (message (if (= code 0) "Built" (str "The build failed: " code))))))
(bind 'normal "<Space>b" "user.make")
```

## Scripting language

`med-lisp` is a small Lisp, the language of the init file and of `:eval`.
A program can change keys and options, define commands and panels (all
above), `(load "file")` another program, and say something with
`(message ...)`. One interpreter serves the whole session, so what
`init.mlisp` defines can be used from `:eval`. A program that never ends is stopped after ten million
steps rather than left to hang the window.

The language itself can be tried without the editor:

```sh
cargo run -p med-lisp --example repl
```

```lisp
(defn leader-key (key command)       ; lexical scope, one namespace
  (bind (str "<Space>" key) command))
(map (fn (n) (* n n)) '(1 2 3))      ; => (1 4 9)
```

Forms: `quote if def defn fn let do set! and or cond when unless while`.
`nil` (also the empty list) and `false` are the only values that do not
hold. A call in tail position takes no stack. Errors carry the line and
column of the form they happened in and of the forms around it. Functions are listed in `crates/med-lisp/src/builtins.rs`;
there are no macros yet.
