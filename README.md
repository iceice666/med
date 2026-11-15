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

Environment: `MED_FONT`, `MED_FONT_SIZE` (default 14), `MED_NO_LSP`.

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

There is no configuration file yet; the tables are changed in code:

```rust
let keymap = &mut cx.global_mut::<med::VimGlobal>().0.keymap;
keymap.bind(MapMode::Normal, "<Space>n", "workspace.next_tab");
keymap.bind(MapMode::Insert, "jk", "insert.escape");
keymap.bind_keys(MapMode::Normal, "Y", "y$");
cx.global_mut::<PanelKeymaps>().bind("git.files", "x", "git.discard");
```

A key that is a binding by itself and also starts longer ones (`space`, which
moves right) waits a second for the rest before acting.

## Layout

| Crate | What |
| --- | --- |
| `med-core` | text buffer, undo, coordinate conversions |
| `med-vim` | modal editing engine |
| `med-lsp` | JSON-RPC transport for language servers |
| `med-git` | wrapper around the `git` CLI |
| `med` | the GPUI application |
