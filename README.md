# med

A modal (vim-style) code editor built on [GPUI](https://www.gpui.rs/), the UI
framework behind Zed.

- Own editing core: rope buffer with grouped undo (`med-core`), a vim engine
  that runs with no UI attached (`med-vim`)
- Tree-sitter syntax highlighting for ~35 languages, and text objects and
  motions that go by the syntax tree: functions, types, arguments,
  comments, and the selection grown node by node
- LSP: diagnostics, completion, hover, signature help, go to definition,
  references, rename, code actions, formatting
- No key is built in. The vim layout and the editor's own keys are
  init-file programs in [`examples/`](examples) like everything else
  there, to load as they are or change
- Panels written in the editor's own Lisp. The file tree, the fuzzy file
  finder with project-wide search, a lazygit-style git screen and the
  which-key list of the keys that can come next are four of them, in
  [`examples/`](examples): not built in, and yours to load, change or
  replace
- Programs in that Lisp read the file being edited, edit it, mark it up
  and are told when it changes

## Build and run

GPUI needs a newer rustc than some distributions ship; the flake provides one
(plus rust-analyzer). macOS needs full Xcode for the Metal shader compiler.

```sh
nix develop            # or: direnv allow
cargo run -p med -- path/to/project
cargo test --workspace
```

`med` takes a directory (the project root), files to open, or nothing (the
current directory). `scripts/bundle.sh [debug|release]` wraps the built binary
in a `med.app`.

Language servers are looked up on `PATH` (rust-analyzer, clangd, gopls,
pyright, typescript-language-server, nil/nixd, …), so start med from a shell
that has them.

Environment: `MED_FONT`, `MED_FONT_SIZE` (default 14), `MED_NO_LSP`,
`MED_CONFIG_DIR` (default `$XDG_CONFIG_HOME/med`, else `~/.config/med`).

## Keys

med binds no key by itself: with no init file, no key does anything
but `:`, which opens the command line for as long as nothing at all is
bound in normal mode, and the status bar says so. There,
`:install_default_config` writes every file of [`examples/`](examples)
to the configuration directory and runs its `init.lisp`; it leaves files
that are there already alone and says which, and
`:install_default_config!` overwrites them.

The keys below are the ones of [`examples/vim.lisp`](examples/vim.lisp)
(vim's) and [`examples/keys.lisp`](examples/keys.lisp) (the rest), which
an init file loads, in that order:

```lisp
; ~/.config/med/init.lisp
(load "/path/to/med/examples/vim.lisp")
(load "/path/to/med/examples/keys.lisp")
```

Editing is vim: modes (`v`, `V` and the block of `ctrl-v`, with `I`, `A`
and `c` on every line of it), counts, operators (`d c y > < gu gU g~ gc`),
motions, text objects (`iw i" i( i{ ip` …), registers, marks (`ma`, `'a`,
`` `a ``, `''`), macros (`qa` … `q`, `@a`, `@@`), `.`, `/ ? n N * #`, `:s`,
`:w :q :wq :e :bn :bp :bd :wa :qa`. The unnamed register is the system
clipboard.

| Key | Action |
| --- | --- |
| `gd` / `gr` / `K` | definition / references / hover |
| `]d` / `[d` | next / previous diagnostic |
| `space r` | rename: the new name is typed after `:rename` |
| `space a` | code actions, for the cursor or the selection: `j` `k`, `enter` or a number |
| `space =`, `:format` | format the file |
| `ctrl-s` (insert) | the signature of what is being called; it comes up by itself after `(` |
| `ctrl-n` `ctrl-p` `tab` | completion menu: next, previous, accept |
| `ctrl-o` / `ctrl-i` | jump back / forward |
| `gt` / `gT`, `⌘⇧]` / `⌘⇧[` | next / previous tab |
| `space w` / `⌘S`, `space q` / `⌘W` | save, close tab |

Surround, as in nvim-surround: `ys` and a motion or text object, then the
delimiter (`ysiw)`, `yss"` for the line), `ds"` to take one away, `cs"'`
to change it, and `S` on a selection. A closing bracket, or `b` `r` `B`
`a`, is the pair as it is; an opening one has a blank inside each.

To go to text on screen, as in flash.nvim: `s`, then some of the text.
Every place that has it is marked and gets a letter, the nearest ones the
letters easiest to type, and the rest of the text is dimmed. The letter
goes to its place, `enter` to the first place after the cursor; backspace
takes a character back and escape gives up, as does text that is nowhere
on screen. A letter that could be more of the text is never a label, and
small letters are capitals as well until a capital is typed. In visual
mode the selection goes there. After an operator the key is `z`, `s` being
surround's, and what is found goes with its first character: `dzfoa`. The
command is `motion.flash`; what `s` is in vim, `cl` still is.

The rest of flash.nvim goes by the same marks:

| Key | Action |
| --- | --- |
| `f` `F` `t` `T` | look on every line on screen, not only the cursor's. Where else they could go is marked, and while it is the key again goes on to the next, as `;` does, and `F` after `f` back, as `,` does; any other key is itself again |
| `S` | a label before and after each syntax node around the cursor, the smallest `a`. The label selects its node, `enter` the smallest; `]x` and `[x` go on from there. After an operator the node is what it acts on: `dSs` |
| `r` after an operator | `s` for the operator to go and work elsewhere: the text and its label, then a motion or text object there, and the cursor comes back to where it was. `yrfoaiw` yanks a word further down |

Those are `motion.flash_find_forward` and the three like it, `select.flash`
and `motion.flash_remote`. Vim's own `f` and `t`, which stay on their line,
are `motion.find_forward` and the like, and what `S` was, `cc` still is.

Brackets and quotes close as they are typed, the closing one is stepped
over, and backspace between the two takes both; `(set-option 'auto-pairs
false)` turns that off.

Where a file has a syntax tree:

| Key | Action |
| --- | --- |
| `if` `af`, `it` `at` | a function, a type: its body, or all of it |
| `ia` `aa` | an argument or parameter, with its comma |
| `ic` `ac` | a comment, lines that follow each other as one |
| `]f` `[f`, `]t` `[t`, `]a` `[a`, `]c` `[c` | the next / previous of each |
| `]x` / `[x` | select the node around the cursor, then the one around that; and back |

## The example panels

An editor with no init file is the editor alone, without even its keys.
The file tree, the picker, the git screen and the list of keys that can
come next are programs in [`examples/`](examples), and an init file
loads the ones it wants:

```lisp
; ~/.config/med/init.lisp
(load "/path/to/med/examples/init.lisp")   ; all of them, keys too, or one by one:
(load "/path/to/med/examples/vim.lisp")
(load "/path/to/med/examples/keys.lisp")
(load "/path/to/med/examples/tree.lisp")
```

A path that does not start at the top is looked for beside the file that
loads it, so the files can also be copied next to `init.lisp` and changed
there. Each brings its commands and its keys:

| File | Key | Action |
| --- | --- | --- |
| `vim.lisp` | | vim's keys, in every mode: the [Keys](#keys) above |
| `keys.lisp` | | the language server's, the tabs', flash's, the `space` leader keys and the `⌘` shortcuts |
| `tree.lisp` | `space e` / `⌘⇧E` | focus file tree (`j k h l`, `enter`, `R` refresh, `q` back) |
| | `⌘B` | show/hide file tree |
| `picker.lisp` | `space f` / `⌘P` | find file |
| | `space /` / `⌘⇧F` | search in project |
| `git.lisp` | `space g` / `⌘⇧G` | git screen |
| `which-key.lisp` | | the keys that can come next, after a pause |
| `todo.lisp` | `]T` / `[T` | a sign beside lines that say TODO, FIXME or NOTE, and from one to the next |
| `dired.lisp` | `space -` | the file's directory as text to edit |
| | `enter` / `-` | in a listing: open the line's file or directory / go up |
| `statusline.lisp` | | the status bar, as a layout to change: the one the window draws without it |
| `titlebar.lisp` | | the title bar: what is being edited, the project and its branch |

With `which-key.lisp`, a command that is half typed (`g`, `space`, `d g`)
lists what can follow at the bottom of the window once nothing has been
typed for 300 ms (`which-key-delay`), in an editor and in a panel alike,
and escape gives up on the command. A key that only starts longer
bindings is listed by how many they are, or by a name: `(which-key-name
"<Space>g" "git")`. It sets `key-timeout` to `nil`: `space` no longer
moves right by itself after a second, but waits for the next key.

With `picker.lisp`, `gr` lists the references to choose from; without it,
`gr` goes to the first.

With `dired.lisp` (as Emacs's wdired), a directory is a tab of names, one
to a line, a directory's with `/` after it, and what git ignores among
them. Change a name and `:w` renames the file; a name with `/` in it
moves it, into directories that are made if need be. An empty line puts
its file in the trash, which takes `:w!`. Nothing is done unless all of
it can be: the lines must stay as many as the files, and a new name may
not be one that is there already, another line's included, so two files
trade names in two writes. `enter` and `-` are the listing's own keys,
in the table `dired`, and are vim's everywhere else.

Git screen (as in lazygit): `1`–`5` or `tab` switch panel, `j`/`k` move,
`J`/`K` scroll the diff, `space` stage / checkout / apply, `a` stage all,
`c` commit, `A` amend, `d` discard / delete / drop (asks first), `n` new
branch, `s` stash, `g` pop stash, `p` pull, `P` push, `f` fetch, `enter` open
file, `q` close.

## Commands and key bindings

No key is wired to behaviour directly. Everything the editor can do is a
named command, and keys are bound to names in tables:

- The vim engine's commands (`motion.word_forward`, `operator.delete`,
  `insert.newline`, … — `med_vim::command_names()`). Counts, registers
  and operator + motion are grammar, and work with whatever keys are
  bound. Its stock layout is `examples/vim.lisp`; `Keymap::vim()` in
  `med-vim/src/keymap.rs` is the same table, kept for the tests.
- The application's commands (`workspace.*`, `editor.*`), registered by
  each view in `Commands`, and the ones programs define (`tree.*`, `git.*`
  and `picker.*` are the examples').
- `examples/keys.lisp`: the editor keys that reach outside the buffer
  (`gd`, the `space` leader keys), the keys of flash and the `⌘`
  shortcuts.
- `med/src/defaults.rs`: the names ex commands go by (`:w` is
  `workspace.save`), the one table that is built in, and the copy of
  `examples/` that `:install_default_config` writes.

Keys are written in vim notation (`<C-w>`, `<Space>f`, `<CR>`). Any command
can also be run by its full name, as in `:workspace.find_file`.

The tables are changed from the init file, `<config>/init.lisp`, which is
run at startup, or from the command line with `:eval`:

```lisp
(bind 'normal "<Space>n" "workspace.next_tab")
(bind 'insert "jk" "insert.escape")
(bind '(normal visual) "gh" "editor.hover")   ; several modes at once
(bind-keys 'normal "Y" "y$")                  ; keys typed in place of others
(bind 'git.files "x" "git.discard")           ; a panel's keys
(bind 'app "cmd-shift-t" "tree.toggle")       ; a shortcut of the window's
(unbind 'normal "gt")
(set-option 'font-size 16)                    ; also font-family, lsp, key-timeout,
                                              ; auto-pairs, format-on-save
```

The modes are `normal`, `visual`, `operator` and `insert`. A panel's table
goes by the panel's name, or by its name and a part of it (`git.files`).
`app` is the table of the shortcuts that work wherever the keyboard is,
written as the window system has them (`cmd-shift-p`). `bind` refuses a
command that does not exist.

A tab can have keys of its own, in tables a program names with
`defkeys`. The tab looks a key up in its tables first, and what none of
them knows in its mode's:

```lisp
(defkeys 'listing)
(bind 'listing "<CR>" "user.open")          ; in normal mode
(bind 'listing.insert "<C-j>" "user.next")  ; also .visual, .operator
(set-buffer-keys '(listing))                ; the tab up is in it, nil for none
(buffer-keys)                               ; => ("listing")
```

A key a table binds is run at once, even where the mode's table has
longer bindings that start with it. `:workspace.reload_init` runs the file
again; what is wrong with it is said in the status bar, with the line.

A command can be written there too. It is a function of no arguments, and
from then on a name like any other: for keys, for the command line, and
for `run`, which runs a command by any name the command line knows it by.

```lisp
(defcommand "user.save-and-close" "Write the file, then close its tab"
  (fn ()
    (run "workspace.save")
    (run "workspace.close_tab")))
(bind 'normal "<Space>x" "user.save-and-close")

(defcommand "user.note" "Open the note named on the command line"
  (fn () (run "workspace.edit" (str "notes/" command-arg ".md"))))
```

`:user.note today` opens `notes/today.md`. Besides `command-arg`, the rest
of the command line, there are `command-bang` (was there a `!`) and
`command-count` (the count typed before the key, or `nil`).
`(current-file)` and `(project-root)` say where the editor is. A command
cannot take the name of one of the editor's own.

A key that is a binding by itself and also starts longer ones (`space`, which
moves right) waits a second for the rest before acting. `(set-option
'key-timeout 300)` gives it another time, in milliseconds, and `nil` has it
wait for the next key however long that takes.

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
layout again. With several up, the one defined last is drawn.

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
| `:mode`, `:mode-label` | the mode, as `(mode)` has it, and as the status bar names it: `'normal`, `"NORMAL"` |
| `:path`, `:scratch` | the file being edited, from the project's root; the name of a tab that is no file |
| `:modified` | whether it has changes that are not written |
| `:line`, `:col` | where the cursor is, counted from 1 |
| `:language` | `"rust"`, or `"text"` |
| `:cmdline` | the command line while it is typed, `":wq"` or `"/foo"` |
| `:message` | what was last said, until the next key |
| `:diagnostic` | the first line of the diagnostic at the cursor |
| `:pending` | the keys of a command typed so far, `"2d"` |
| `:recording` | the register a macro is being recorded into |
| `:errors`, `:warnings` | how many diagnostics of each, in every file |
| `:lsp` | what the file's language server is doing |
| `:branch` | the branch the project is on |

With no file open there is no `:mode` and nothing that comes from a
file. A status bar that leaves out `:cmdline` has `:` typed blind.
[`examples/statusline.lisp`](examples/statusline.lisp) is the window's own
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

Styles are `:name value` pairs: `width` and `height` (pixels or `'full`),
`grow` (`true`, or a share of the room next to others that grow), `shrink`,
`padding-x` `padding-y` `padding-left` `padding-right`, `gap`, `align` and
`justify` (`'start` `'center` `'end`, and `'between`), `bg`, `color`,
`border` `border-top` `border-right` `border-bottom` `border-left`,
`border-color`, `rounded`, `size` (of text), `bold`, `mono`, `nowrap`,
`clip`; `hover`, a list of styles for under the pointer, as in
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
| `(open-file path [line [column]])` | the file in a tab, at a place counted from 1 |
| `(list-dir path [all])` | what is in a directory: `((:name :path :dir)...)`, without what git ignores unless `all` |
| `(path-name p)`, `(path-parent p)`, `(path-join a b...)` | parts of paths |
| `(file-exists? path)`, `(dir? path)` | whether anything is there; a directory |
| `(walk-files function)` | every file of the project, as paths from its root |
| `(fuzzy query list limit)` | the texts a few letters mean, best first |
| `(grep query limit function)` | matching lines: `((:path :file :line :col :text)...)` |
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
| `(delete-file path)` | puts a file or a directory in the trash |
| `(make-dir path)` | a directory, and the ones it is in |

The hooks are `opened` (a window is up, and there is a project),
`files-changed` (a file was saved, the window came to the front, or
`workspace.sync` was run), `locations` (a list of places to choose
from, such as the references to a name), `keys-pending` (below), and the
ones about the file being edited (further below).

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

[`examples/todo.lisp`](examples/todo.lisp) is a short program made of
these: it reads the text once it has changed, puts a sign on the lines
that say TODO, and has two commands that move the cursor.

A program can also have a tab of text that is no file, by a name of its
own:

```lisp
(open-scratch "notes")                       ; go to it, or make it
(open-scratch "notes" "one\ntwo")            ; with this text, as written
(open-scratch "notes" nil (fn (text) (message (length (lines text)) " lines")))
(scratch-name)                               ; "notes" while it is up, else nil
```

Given a function, `:w` calls it with the text, as a command
(`command-bang` is there for `:w!`), and the text stays unsaved until the program gives the tab
its text again. Without one, there is nothing in the tab to lose and
nowhere to write it. `(current-file)` is `nil` there, and `:w name`
makes the tab a file, without the tab's own keys. [`examples/dired.lisp`](examples/dired.lisp) lists a directory in
one and renames files with what is written.

## Scripting language

`med-lisp` is a small Lisp, the language of the init file and of `:eval`.
A program can change keys and options, define commands and panels (all
above), `(load "file")` another program, and say something with
`(message ...)`. One interpreter serves the whole session, so what
`init.lisp` defines can be used from `:eval`. A program that never ends is stopped after ten million
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

## Layout

| Crate | What |
| --- | --- |
| `med-core` | text buffer, undo, coordinate conversions |
| `med-vim` | modal editing engine |
| `med-lsp` | JSON-RPC transport for language servers |
| `med-git` | wrapper around the `git` CLI |
| `med-lisp` | scripting language |
| `med` | the GPUI application |

`examples/` has the keys and the panels, which are written in Lisp rather
than built in.
