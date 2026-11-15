# med

A modal (vim-style) code editor built on [GPUI](https://www.gpui.rs/), the UI
framework behind Zed.

- Own editing core: rope buffer with grouped undo (`med-core`), a vim engine
  that runs with no UI attached (`med-vim`)
- Tree-sitter syntax highlighting for ~35 languages
- LSP: diagnostics, completion, hover, go to definition, references
- File tree, fuzzy file finder, project-wide search
- A lazygit-style git screen

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

Editing is vim: modes, counts, operators (`d c y > < gu gU g~ gc`), motions,
text objects (`iw i" i( i{ ip` …), registers, `.`, `/ ? n N * #`, `:s`,
`:w :q :wq :e :bn :bp :bd :wa :qa`. The unnamed register is the system
clipboard.

| Key | Action |
| --- | --- |
| `space f` / `⌘P` | find file |
| `space /` / `⌘⇧F` | search in project |
| `space e` / `⌘⇧E` | focus file tree (`j k h l`, `enter`, `R` refresh, `q` back) |
| `⌘B` | show/hide file tree |
| `space g` / `⌘⇧G` | git screen |
| `gd` / `gr` / `K` | definition / references / hover |
| `]d` / `[d` | next / previous diagnostic |
| `ctrl-n` `ctrl-p` `tab` | completion menu: next, previous, accept |
| `ctrl-o` / `ctrl-i` | jump back / forward |
| `gt` / `gT`, `⌘⇧]` / `⌘⇧[` | next / previous tab |
| `space w` / `⌘S`, `space q` / `⌘W` | save, close tab |

Git screen (as in lazygit): `1`–`5` or `tab` switch panel, `j`/`k` move,
`J`/`K` scroll the diff, `space` stage / checkout / apply, `a` stage all,
`c` commit, `A` amend, `d` discard / delete / drop (asks first), `n` new
branch, `s` stash, `g` pop stash, `p` pull, `P` push, `f` fetch, `enter` open
file, `q` close.

## Commands and key bindings

No key is wired to behaviour directly. Everything the editor can do is a
named command, and keys are bound to names in tables:

- The vim engine's commands (`motion.word_forward`, `operator.delete`,
  `insert.newline`, … — `med_vim::command_names()`) and its stock layout,
  `Keymap::vim()` in `med-vim/src/keymap.rs`. Counts, registers and
  operator + motion are grammar, and work with whatever keys are bound.
- The application's commands (`workspace.*`, `editor.*`, `tree.*`, `git.*`),
  registered by each view in `Commands`.
- `med/src/defaults.rs`: the editor keys that reach outside the buffer
  (`gd`, the `space` leader keys), the file tree's and git screen's keys, the
  `⌘` shortcuts, and the names ex commands go by (`:w` is `workspace.save`).

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
(unbind 'normal "gt")
(set-option 'font-size 16)                    ; also font-family, lsp
```

The modes are `normal`, `visual`, `operator` and `insert`; the panels are
`tree`, `git`, `git.files`, `git.branches` and `git.stash`. `bind` refuses
a command that does not exist. `:workspace.reload_init` runs the file
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
moves right) waits a second for the rest before acting.

## Panel layouts

How a panel looks is a layout: a JSON tree the panel's view is drawn from.
The file tree is the first panel done this way. Its layout is
`crates/med/src/panels/file_tree.json`; a copy at
`<config>/panels/file_tree.json` replaces it, and `:workspace.reload_panels`
reads it again while running. A file that cannot be used is reported in the
status bar and the built-in layout stays.

A node has a `type` and, all optional, a `style`, `when` (styles added
while flags hold: `{ "if": ["selected", "!focused"], "style": … }`),
`hover`, `if` (flags that must hold for the node to be there) and
`on_click` (a command name).

| Type | Also takes |
| --- | --- |
| `row`, `col` | `children` |
| `text` | `text`, with `{name}` for a value of the panel |
| `list` | `source` (a list of the panel) and `item`, the layout of one row |

Styles: `width` and `height` (pixels or `"full"`), `grow`, `shrink`,
`padding_x` `padding_y` `padding_left` `padding_right`, `gap`, `align` and
`justify` (`start` `center` `end`, and `between`), `bg`, `color`,
`border_top` `border_right` `border_bottom` `border_left`, `border_color`,
`bold`, `nowrap`. A color is a theme color (`sidebar`, `muted_foreground`,
`list_active`, `warning`…) or `#rrggbb`. A padding or gap can grow with a
number of the panel: `{ "base": 10, "step": 14, "times": "depth" }`.

The file tree has the text `root_name` and the flag `focused`; in a row of
its list `entries`, the texts `name` and `status`, the number `depth` and
the flags `selected`, `dir`, `expanded`, `added`, `modified`. A click on a
row selects it, then runs the row's command.

## Scripting language

`med-lisp` is a small Lisp, the language of the init file and of `:eval`.
So far a program can change keys and options, define commands (above) and
say something with `(message ...)`; it cannot yet draw anything. One
interpreter serves the whole session, so what `init.lisp` defines can be
used from `:eval`. A program that never ends is stopped after ten million
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
