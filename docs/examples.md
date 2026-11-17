English | [繁體中文](examples.zh-TW.md)

# The example programs

## The example panels

An editor with no init file is the editor alone, without even its keys.
The directory editor, the file tree, the picker, the git screen and the list of keys that can
come next are programs in [`examples/`](../examples), and an init file
loads the ones it wants:

```lisp
; ~/.config/med/init.mlisp
(load "/path/to/med/examples/init.mlisp")   ; all of them, keys too, or one by one:
(load "/path/to/med/examples/vim.mlisp")
(load "/path/to/med/examples/keys.mlisp")
(load "/path/to/med/examples/oil.mlisp")
```

A path that does not start at the top is looked for beside the file that
loads it, so the files can also be copied next to `init.mlisp` and changed
there. Each brings its commands and its keys:

| File | Key | Action |
| --- | --- | --- |
| `vim.mlisp` | | vim's keys, in every mode: see [Keys](keys.md#keys) |
| `keys.mlisp` | | the language server's, the tabs', flash's, the `space` leader keys and the `⌘` shortcuts |
| `oil.mlisp` | `space -` | the file's directory as a buffer to edit, as oil.nvim |
| | `space e` / `⌘⇧E` | the same, or from a listing back to the file |
| | `enter` / `-` / `_` | in a listing: open the line's file or directory / go up / the project's root |
| | `:w` | in a listing: do what the edits ask, after a popup says what (`y` / `n`) |
| `picker.mlisp` | `space f` / `⌘P` | find file |
| | `space /` / `⌘⇧F` | search in project |
| | `space S` / `⌘T` | find a symbol of the project, as the language servers running know them |
| | `space p` / `⌘⌥O` | open a project opened before, in a window of its own |
| `structural.mlisp` | `space R` / `⌘⇧R` | find code by its shape, and replace it (below) |
| `git.mlisp` | `space g` / `⌘⇧G` | git screen |
| `which-key.mlisp` | | the keys that can come next, after a pause |
| | `g ?` | in an oil listing: all of its keys at once (as `init.mlisp` sets it up) |
| `todo.mlisp` | `]T` / `[T` | a sign beside lines that say TODO, FIXME or NOTE, and from one to the next |
| `tree.mlisp` | `space e` / `⌘⇧E` | not loaded by `init.mlisp`: open and focus the file tree, or from it put it away (`j k h l`, `enter`, `R` refresh, `q` put away) |
| | `⌘B` | show/hide file tree (hidden at start) |
| `dired.mlisp` | `space -` | not loaded by `init.mlisp`: the file's directory, as Emacs's dired |
| | `space d` | the directory at a path typed in: whole, from `~`, or from the listing's or the file's directory |
| | `enter` / `-` | in a listing: open the line's file or directory / go up |
| | `i` | in a listing: edit the names, as wdired |
| `statusline.mlisp` | | the status bar, as a layout to change: the one the window draws without it |
| `titlebar.mlisp` | | the title bar: what is being edited, the project's directory (a click switches project) and its branch |
| | click the branch | switch branch (also `:titlebar.branches`) |

With `which-key.mlisp`, a command that is half typed (`g`, `space`, `d g`)
lists what can follow at the bottom of the window once nothing has been
typed for 300 ms (`which-key-delay`), in an editor and in a panel alike,
and escape gives up on the command. A key that only starts longer
bindings is listed by how many they are, or by a name: `(which-key-name
"<Space>g" "git")`. It sets `key-timeout` to `nil`: `space` no longer
moves right by itself after a second, but waits for the next key.
`(which-key-help-key 'tree "?")` makes `?` in a table of keys list
all of them straight away, and the key typed then does what it does
there; `init.mlisp` does so with `g ?` for oil's listings, after their
keys are bound, as a key bound later is not on the list.

With `picker.mlisp`, `gr` lists the references to choose from, `space s`
the symbols of the file, `space c i` and `space c o` the callers and
calls of a function, and `gd`, `gy` and `gi` the places they find when
there are several; without it, each goes to the first.

With `structural.mlisp`, `space R` puts up a popup for structural search
and replace in the language of the file being edited, as IntelliJ IDEA
has it, with what is selected as the pattern if anything is. A pattern
is code with `$name$` where anything may be (see [Code by its
shape](lisp.md#code-by-its-shape)); what is wrong with it is said as it
is typed. `tab` goes between the pattern and the replacement, and each
match shows what it would be made. `ctrl-n` and `ctrl-p` move, `ctrl-t`
checks or unchecks a match, `ctrl-u` all of them, `ctrl-r` makes the
checked ones their replacement, and `enter` goes to the match. A match
with a comment that would be lost is shown, and left as it is.

With `oil.mlisp` (as oil.nvim), a directory is a tab called
`oil:///the/directory/` with a name to a line, a directory's with `/`
after it, the names that start with a dot left out until `g .`. It is
text to edit with vim's keys like any other, and `:w` does to the disk
what the edits ask for, once a popup has listed it all and `y` has said
yes: a name changed moves its file (with a `/` in it, into a directory
below or above, made if need be), a line taken out puts its file in the
trash (or with `(set! oil-delete-forever true)` does away with it), and
a line put in makes an empty file, or a directory if it ends in `/`.
Each line is known to be the file it was put up as while it is edited,
as oil.nvim knows it by a number hidden on the line: a line cut with
`dd` and put with `p` in another listing moves its file there, `yy` and
`p` copy it, and a `:w` writes every listing with changes at once.
Nothing is done unless all of it can be: no two lines may say the same
name, nor one a name that is there already, unless that file is taken
out in the same write. `C-l` reads the disk again (asking first if
there are changes), `g x` opens the file with the system's program for
it, and `C-c` closes the listing, back to the file it was opened from.
Going to a file or a directory closes the listing it is gone from,
unless it has changes not written. Oil.nvim's own key for it is `-`,
which `vim.mlisp` gives to a motion: `(bind 'normal "-" "oil.open")` has
it the oil way.

`tree.mlisp` and `dired.mlisp` are not loaded by `init.mlisp`; a `(load
...)` of one after it has it.

With `dired.mlisp` (as Emacs's dired), a directory is a tab of names, one
to a line, a directory's with `/` after it, what git ignores among them,
and after each name what `ls -l` says of it. The keys are dired's, in the
table `dired`, and vim's motions stay; what is not edited stays as it is
put up. `m` marks a file and `d` flags it for deletion, `u` and `U` take
marks off, `t` turns them around, and `* *`, `* /`, `* @`, `* .`, `% m`,
`% d`, `~` and `#` mark or flag by kind, by extension or by a regular
expression. On the marked files, or else the one on the line, `C` copies,
`R` renames or moves, `S` and `H` link, `% C` and `% R` copy and rename by
a regular expression, `D` deletes (`x` the flagged), `M` and `O` change
the mode and the owner, `T` touches, `Z` compresses and uncompresses, `!`
runs a shell command (`*` alone in it for all the files, `?` for each),
and `=` diffs; `+` makes a directory, and `c` an empty file. What asks for a name, or for a y,
asks in a popup. `s` lists newest first, `(` hides the details, `g r`
reads the disk again, `q` closes the listing. With `(set!
dired-dwim-target true)`, as in Emacs, `C`, `R`, `S` and `H` offer the
directory of the listing in the next pane, so that two listings side by
side copy and move between them; every listing up reads the disk again
after. A listing is in one pane: in a new one (`ctrl-w v`), `space -`
lists the project's root, taking it from the pane that had it, and
`enter` goes down from there; `space -` back in a file's pane lists its
directory.

`i` (or `C-x C-q`) edits the names, as wdired. Change a name and `:w`
(or `C-x C-s`) renames the file; a name with `/` in it moves it, into
directories that are made if need be. An empty line puts its file in the
trash, which takes `:w!`. Nothing is done unless all of it can be: the
lines must stay as many as the files, and a new name may not be one that
is there already, another line's included, so two files trade names in
two writes. `C-c C-k` gives the changes up. There are no subdirectories
inserted into a listing, and no searching or replacing in the files.

Git screen (as in lazygit): `1`–`5` or `tab` switch panel, `j`/`k` move,
`J`/`K` scroll the diff, `space` stage / checkout / apply, `a` stage all,
`c` commit, `A` amend, `d` discard / delete / drop (asks first), `n` new
branch, `s` stash, `g` pop stash, `p` pull, `P` push, `f` fetch, `enter` open
file, `q` close.
