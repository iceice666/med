English | [繁體中文](examples.zh-TW.md)

# The example programs

## The example panels

An editor with no init file is the editor alone, without even its keys.
The file tree, the picker, the git screen and the list of keys that can
come next are programs in [`examples/`](../examples), and an init file
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
| `vim.lisp` | | vim's keys, in every mode: see [Keys](keys.md#keys) |
| `keys.lisp` | | the language server's, the tabs', flash's, the `space` leader keys and the `⌘` shortcuts |
| `tree.lisp` | `space e` / `⌘⇧E` | open and focus the file tree, or from it put it away (`j k h l`, `enter`, `R` refresh, `q` put away) |
| | `⌘B` | show/hide file tree (hidden at start) |
| `picker.lisp` | `space f` / `⌘P` | find file |
| | `space /` / `⌘⇧F` | search in project |
| `git.lisp` | `space g` / `⌘⇧G` | git screen |
| `which-key.lisp` | | the keys that can come next, after a pause |
| | `?` | in the tree or a listing: all of its keys at once (as `init.lisp` sets it up) |
| `todo.lisp` | `]T` / `[T` | a sign beside lines that say TODO, FIXME or NOTE, and from one to the next |
| `dired.lisp` | `space -` | the file's directory, as Emacs's dired |
| | `enter` / `-` | in a listing: open the line's file or directory / go up |
| | `i` | in a listing: edit the names, as wdired |
| `statusline.lisp` | | the status bar, as a layout to change: the one the window draws without it |
| `titlebar.lisp` | | the title bar: what is being edited, the project and its branch |
| | click the branch | switch branch (also `:titlebar.branches`) |

With `which-key.lisp`, a command that is half typed (`g`, `space`, `d g`)
lists what can follow at the bottom of the window once nothing has been
typed for 300 ms (`which-key-delay`), in an editor and in a panel alike,
and escape gives up on the command. A key that only starts longer
bindings is listed by how many they are, or by a name: `(which-key-name
"<Space>g" "git")`. It sets `key-timeout` to `nil`: `space` no longer
moves right by itself after a second, but waits for the next key.
`(which-key-help-key 'tree "?")` makes `?` in a table of keys list
all of them straight away, and the key typed then does what it does
there; `init.lisp` does so for the tree and for dired's listings, after
their keys are bound, as a key bound later is not on the list.

With `picker.lisp`, `gr` lists the references to choose from; without it,
`gr` goes to the first.

With `dired.lisp` (as Emacs's dired), a directory is a tab of names, one
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
and `=` diffs; `+` makes a directory. What asks for a name, or for a y,
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
