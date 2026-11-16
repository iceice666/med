English | [繁體中文](keys.zh-TW.md)

# Keys and commands

## Keys

med binds no key by itself: with no init file, no key does anything
but `:`, which opens the command line for as long as nothing at all is
bound in normal mode. A popup over the editor says so, and what to type;
it steps aside while the command line is open, escape or a click beside
it puts it away, and it goes once keys are bound. There,
`:install_default_config` writes every file of [`examples/`](../examples)
to the configuration directory and runs its `init.mlisp`; it leaves files
that are there already alone and says which, and
`:install_default_config!` overwrites them.

The keys below are the ones of [`examples/vim.mlisp`](../examples/vim.mlisp)
(vim's) and [`examples/keys.mlisp`](../examples/keys.mlisp) (the rest), which
an init file loads, in that order:

```lisp
; ~/.config/med/init.mlisp
(load "/path/to/med/examples/vim.mlisp")
(load "/path/to/med/examples/keys.mlisp")
```

Editing is vim: modes (`v`, `V` and the block of `ctrl-v`, with `I`, `A`
and `c` on every line of it), counts, operators (`d c y > < gu gU g~ gc`),
motions, text objects (`iw i" i( i{ ip` …), registers, marks (`ma`, `'a`,
`` `a ``, `''`), macros (`qa` … `q`, `@a`, `@@`), `.`, `/ ? n N * #`, `:s`,
`:w :q :wq :e :bn :bp :bd :wa :qa :mks :project :vs :sp :close :only`. The unnamed register is the system
clipboard. Searches go as vim's with `ignorecase` and `smartcase`: a
pattern with no capital letter matches any case, one with a capital only
its own, and `\c` / `\C` in it say which; `*` and `#` match the word in
any case.

| Key | Action |
| --- | --- |
| `gd` / `gr` / `K` | definition / references / hover |
| `]d` / `[d` | next / previous diagnostic |
| `space r` | rename: the new name is typed after `:rename` |
| `space a` | code actions, for the cursor or the selection: `j` `k`, `enter` or a number |
| `space =`, `:format` | format the file |
| `ctrl-s` (insert) | the signature of what is being called; it comes up by itself after `(` |
| `ctrl-n` `ctrl-p` `tab` | completion menu: next, previous, accept |
| `tab` / `shift-tab` (on `:`) | complete a command's name, a file after `:e` `:w`, or a topic after `:h`; again for the next / previous match |
| `ctrl-o` / `ctrl-i` | jump back / forward |
| `gt` / `gT`, `⌘⇧]` / `⌘⇧[` | next / previous tab |
| `ctrl-w v` / `ctrl-w s`, `:vs` / `:sp` | split the pane: a new one, with an empty buffer, to the right / below |
| `ctrl-w h j k l`, `ctrl-w w` / `ctrl-w W` | the pane to the left, below, above, right; the next / previous one |
| `ctrl-w c` / `:close`, `ctrl-w o` / `:only` | close the pane / every other pane: their tabs go to the pane left with the keys |
| `space w` / `⌘S`, `space q` / `⌘W` | save, close tab |
| `space m`, `:preview` | a Markdown file rendered, in a pane split off to the right that follows it as it is typed, the keys staying with the file; again, to the preview and back |
| `space t`, `:terminal` | a shell in the project's root and environment, in a pane split off below; again, to it, if it is still running. `:terminal cmd` runs `cmd` in a new one, `:terminal!` starts another shell. In it every key goes to the program but ⌘ ones; the wheel and `shift-pageup` / `shift-pagedown` scroll back through what went by, a drag of the mouse selects (twice, words; three times, lines) and `⌘C` copies it. `ctrl-\ ctrl-n` is normal mode: what went by, copied into an editor, to move about, search, yank, and to go to another pane from; it is not kept up to date, and changes to it go nowhere. `i`, `a` or anything else that inserts goes back to the program. Once the program is done, a key closes the tab, and normal mode still reads what it wrote |

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

`Tab`, `>>` and a new line indent by what the file's language says (2 for
med-lisp, Markdown, TOML, Nix, JavaScript and the like, 4 for Rust or
Python), or by what the file is indented with already. `(set-option
'tab-width 4)` makes it 4 columns in every file; `nil` goes back. Files
indented with tabs keep them, that many columns wide.

`(set-option 'relative-number true)` numbers each line by how far it is
from the cursor's, the count `j` or `k` takes to get there, as vim's
`relativenumber` with `number`: the cursor's line keeps its own number.

The view keeps 3 lines between the cursor and its top and bottom, as
vim's `scrolloff`, and `H`, `L`, `ctrl-e` and `ctrl-y` keep to that
too, but at the start and end of the file. `(set-option 'scroll-off 5)`
makes it 5; 0 lets the cursor go to the edge, and 999 keeps it in the
middle.

The cursor's line is marked, as vim's `cursorline`, and `(set-option
'cursor-line false)` leaves it as the others. `(set-option 'color-column
80)` marks the 80th column down the view, as vim's `colorcolumn`; a list,
`'(80 120)`, marks each, and `nil` none, as at first.

`(set-option 'wrap true)` goes on with a line longer than the view on
the next row of it, rather than off its right edge, as vim's `wrap`: at
any character, a wide one or a tab going on whole. Only the first row
has the line's number. The view scrolls by rows, `scroll-off` counts
them, and a click goes to the char under it on whatever row. `j` and `k`
still go a line at a time, and `gj` and `gk` (`motion.row_down`,
`motion.row_up`) a row, keeping to the same cell of it; where nothing
wraps they are `j` and `k`. What is said after a line's end, a
diagnostic or a program's text, is cut off at the right edge, and
`color-column` marks a column of the view, on every row. With
`set-buffer-option` and `buffer-language` it can be for some files only:
`(set-buffer-option 'wrap true)` where the language is `"markdown"`.

`(set-option 'indent-guides true)` draws a thin line down each step of
the lines' indents, a step being the file's tab width, as vim's
indent-blankline; a blank line has the indent of the lines around it,
so that a block's lines go on through it. The line of the block the
cursor is in is the stronger, down the block. A tab can have its own.

`(set-option 'sticky-scroll true)` keeps the first lines of the
functions, types and modules the top of the view is in standing at its
top, one under the other, as VS Code's sticky scroll; in markdown, the
headings of the sections. They go by the syntax tree, so a file with no
grammar has none; a closure is not one. As a scope's end comes up the
view, its line is pushed up out of it. They take no more than a third
of the view, five at most, and never stand over the cursor's line. A
click on one goes to it. A tab can have its own.

Beside the line numbers, a bar marks the lines that are not as they
are at HEAD, the last commit: green for lines added, blue for lines
changed, and a red tick where lines were taken out. It follows the text
as it is typed, a moment after. What is staged is still a change from
HEAD, and keeps its mark until it is committed. HEAD is read again when
the window comes to the front and after `workspace.sync`, which the git
screen runs after each thing it does. A file git does not have, or a
file in no repository, has no marks. `(set-option 'git-gutter false)`
turns them off; a tab can have its own.

`(set-option 'inlay-hints true)` shows what the language server says
inline of the text, dimmed, in the line where it goes: a variable's
type, the name of a parameter an argument is given for. They are asked
for when the text has been still for a moment, and again when the
server says it has more; until then they go with the text as it is
edited. They are not shown where lines wrap. A tab can have its own,
with `set-buffer-option`.

`(set-option 'theme "Catppuccin Mocha")` gives the window that theme's
colors, the text's and the syntax's, whatever the system's appearance;
`'("Catppuccin Latte" "Catppuccin Mocha")`, a light theme and a dark
one, goes by the appearance, and `nil` goes back to the light and dark
themes med has of itself. Themes are the files of `themes/` in the
configuration directory, in gpui-component's theme format, read at
startup and again as they change. `examples/themes/catppuccin.json` has
Catppuccin's Latte and Mocha, made from its palette
(<https://github.com/catppuccin/palette>, MIT licence), and
`:install_default_config` writes it there.

`(set-option 'undo-file true)` keeps each file's undo history when it is
saved, as vim's `undofile`, and opening the file again, in this run or
another, takes it up: `u` goes back past the save, through the last 1000
changes. It is kept in med's state directory (`$MED_STATE_DIR`, else
`$XDG_STATE_HOME/med`, else `~/.local/state/med`), under `undo/`. A file
changed since by something else starts with no history. The option is
every tab's, read as files are opened and saved.

Where a file has a syntax tree:

| Key | Action |
| --- | --- |
| `if` `af`, `it` `at` | a function, a type: its body, or all of it |
| `ia` `aa` | an argument or parameter, with its comma |
| `ic` `ac` | a comment, lines that follow each other as one |
| `]f` `[f`, `]t` `[t`, `]a` `[a`, `]c` `[c` | the next / previous of each |
| `]x` / `[x` | select the node around the cursor, then the one around that; and back |


## Commands and key bindings

No key is wired to behaviour directly. Everything the editor can do is a
named command, and keys are bound to names in tables:

- The vim engine's commands (`motion.word_forward`, `operator.delete`,
  `insert.newline`, … — `med_vim::command_names()`). Counts, registers
  and operator + motion are grammar, and work with whatever keys are
  bound. Its stock layout is `examples/vim.mlisp`; `Keymap::vim()` in
  `med-vim/src/keymap.rs` is the same table, kept for the tests.
- The application's commands (`workspace.*`, `editor.*`), registered by
  each view in `Commands`, and the ones programs define (`tree.*`, `git.*`
  and `picker.*` are the examples').
- `examples/keys.mlisp`: the editor keys that reach outside the buffer
  (`gd`, the `space` leader keys), the keys of flash and the `⌘`
  shortcuts.
- `med/src/defaults.rs`: the names ex commands go by (`:w` is
  `workspace.save`), the one table that is built in, and the copy of
  `examples/` that `:install_default_config` writes.

Keys are written in vim notation (`<C-w>`, `<Space>f`, `<CR>`). Any command
can also be run by its full name, as in `:workspace.find_file`.

`:help` (`:h`) says what a command does: `:help w` or `:help
workspace.save` gives what it says of itself, the names the command line
knows it by, and every key bound to it, in the modes' tables, the
buffers', the panels' and `app`. Nothing in it is written by hand: the
page is made when it is asked for, so it has the keys and the commands of
the init file as they are then. `:help` alone lists every command, and
the vim engine's with their keys in normal mode. The pages come up in a
tab of their own, called `help`, which each `:help` reuses.

The tables are changed from the init file, `<config>/init.mlisp`, which is
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
                                              ; auto-pairs, format-on-save, tab-width,
                                              ; relative-number, scroll-off, cursor-line,
                                              ; color-column, wrap, inlay-hints,
                                              ; indent-guides, sticky-scroll,
                                              ; git-gutter, undo-file, theme,
                                              ; project-env
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

A tab can have options of its own as well: `tab-width`,
`relative-number`, `format-on-save`, `auto-pairs`, `scroll-off`,
`cursor-line`, `color-column`, `wrap`, `inlay-hints`, `indent-guides`,
`sticky-scroll` and `git-gutter` (the others are the window's or the application's). Its own value goes before the one `set-option` gives
every tab, until it is reset. With
`buffer-entered`, which is told of every file that comes up, and
`buffer-language`, an option can go by the language:

```lisp
(set-buffer-option 'tab-width 8)            ; the tab up's own
(reset-buffer-option 'tab-width)            ; back to set-option's
(buffer-language)                           ; => "markdown", "text" for none

(add-hook 'buffer-entered
  (fn (path)
    (when (= (buffer-language) "sql")
      (set-buffer-option 'format-on-save false))))
```

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

## Language servers

A file is served by the language servers of its language that are found
on the project's `PATH`. med has servers of its own for the languages it
knows (`rust-analyzer`, `clangd`, `basedpyright-langserver` or else
`pyright-langserver` or else `pylsp`, `typescript-language-server`,
`gopls`, …), each known by the name of its program; `(lsp-servers)` lists
them all. The init file changes them, turns them off and adds others:

```lisp
; One of med's: what is given changes, the rest stays.
(lsp-server "rust-analyzer"
  :settings '(:rust-analyzer (:check (:command "clippy") :cargo (:features "all"))))
(lsp-server "pylsp" :enabled false)

; One of your own needs a command and the languages it serves.
(lsp-server "eslint"
  :command "vscode-eslint-language-server" :args '("--stdio")
  :languages '("javascript" "typescript" "tsx")
  :settings "{\"validate\": \"on\", \"run\": \"onType\", \"quiet\": false,
              \"useESLintClass\": false, \"experimental\": {}, \"nodePath\": \"\",
              \"problems\": {\"shortenToSingleLine\": false},
              \"rulesCustomizations\": [], \"onIgnoredFiles\": \"off\",
              \"codeActionOnSave\": {\"enable\": false, \"mode\": \"all\"},
              \"codeAction\": {\"disableRuleComment\": {\"enable\": true, \"location\": \"separateLine\"},
                             \"showDocumentation\": {\"enable\": true}},
              \"workingDirectory\": {\"mode\": \"auto\"}}")
```

Some servers, as eslint, read nothing they are not given: their
settings are all of what their documentation (or nvim-lspconfig's
configuration of them) lists.

`:settings` is what the server is answered when it asks for its
configuration (`workspace/configuration`, a section at a time, as
`"python.analysis"`), and is sent to it once it has started;
`:init-options` goes with `initialize`. Both are written as lists of
keywords and values, `'(:a 1 :b (:c true))` being `{"a": 1, "b": {"c":
true}}` and any other list an array, or as a JSON string, as a server's
documentation has them. `:enabled false` keeps a server from starting.

A language med does not know is named with `deflanguage`, which gives its
files a language for `buffer-language` and its servers, though no
highlighting:

```lisp
(deflanguage "dart" :extensions '("dart") :comment "//" :tab-width 2)
(lsp-server "dartls" :command "dart" :args '("language-server" "--protocol=lsp")
  :languages '("dart"))
```

Its options are `:extensions`, `:filenames` (`"Justfile"`), `:lsp-id` (the
`languageId` servers are told, the name by default), `:comment`,
`:tab-width` and `:expand-tab`. Named after one of med's languages, it
adds to that one's files and keeps its highlighting.

Every server of a file's language serves it, in the order they were set
up, but of med's own that are alternatives, as the three for Python, the
first one found. All of them are told of the file's text, and the
diagnostics, code actions, completions and hover of all of them are
shown, hover one server's under another's; a code action is run by the
server that offered it. Completions are asked of every server that
offers them, but after a character that triggers them only of the
servers that named that character; among equal matches, those of the
first server come first. Everything else (definition, references,
rename, signature help, inlay hints, formatting) goes to the first that
says it can do it.

A server keeps what it was started with: after the init file is changed
and run again (`:workspace.reload_init`), `:workspace.reload_env` starts
the servers again with what it now says.
