English | [繁體中文](README.zh-TW.md)

# med

**M**y **ED**itor: a modal (vim-style) code editor built on
[GPUI](https://www.gpui.rs/), the UI framework behind Zed.

- Its own editing core: a rope buffer with grouped undo (`med-core`) and a
  vim engine that runs without a UI (`med-vim`)
- Tree-sitter highlighting for ~35 languages, plus text objects and motions
  that follow the syntax tree (functions, types, arguments, comments)
- LSP: diagnostics, completion, hover, signature help, go to definition,
  declaration, type definition and implementation, references, the
  places of the name under the cursor, call hierarchy, document and
  project symbols, semantic tokens, code lenses, snippets, rename, code
  actions, formatting; servers, their settings and several to a file set
  up from the init file
- No built-in keys. The vim layout, an oil.nvim-style directory editor,
  the fuzzy finder, a lazygit-style git screen and which-key are Lisp
  programs in
  [`examples/`](examples) that you load, change or replace

## Install

Each [release](https://github.com/iceice666/med/releases) has a `.dmg` and a
zipped `med.app` for macOS on Apple silicon, and a `.deb` and an `.AppImage`
for Linux on x86_64 and arm64.

The macOS app is not signed by an Apple developer, so macOS reports a
downloaded copy as "damaged". After moving it to Applications, clear the
quarantine flag once:

```sh
xattr -dr com.apple.quarantine /Applications/med.app
```

## Build and run

The flake provides a recent enough rustc and rust-analyzer. macOS needs full
Xcode for the Metal shader compiler.

```sh
nix develop            # or: direnv allow
cargo run -p med -- path/to/project
cargo test --workspace
```

`med` takes a project directory, files to open, or nothing (the current
directory). `scripts/bundle.sh [debug|release]` wraps the binary in a
`med.app`.

Environment variables: `MED_FONT`, `MED_FONT_SIZE` (default 14),
`MED_NO_LSP`, `MED_NO_PROJECT_ENV`, `MED_NO_SESSION`, `MED_CONFIG_DIR`
(default `$XDG_CONFIG_HOME/med`, else `~/.config/med`), `MED_STATE_DIR`
(default `$XDG_STATE_HOME/med`, else `~/.local/state/med`).

### Sessions

Started with no file named (`med` or `med dir`), med opens what the project
had open when it last quit there: the panes as they were split, the files in
each and where the cursor was. Files no longer on disk are left out, and
terminals and the texts of programs are never kept. Started with files,
med opens only those, and quitting leaves the project's session as it was;
`:mksession` (`:mks`) keeps what is open now either way. Sessions are kept
in `sessions/` under `MED_STATE_DIR`, one file per project; `MED_NO_SESSION`
turns them off.

Each window is one project. `:project dir` opens another in a window of
its own, and with `picker.mlisp`, `space p` lists the projects opened
before, as does a click on the directory in the title bar. `:q` on the
last tab, or `:qa` on any, closes that window only; `cmd-q` quits them all.

### Project environment

Language servers (rust-analyzer, clangd, gopls, pyright, …, found on the
project's `PATH`) and programs started with `spawn` run in the project's
environment. Started from a terminal, med keeps the environment it has;
started from Finder, the Dock or a desktop launcher, it takes your login
shell's (`$SHELL -l -i`). Either way, if direnv is installed,
`direnv export json` goes on top, which also covers devbox and nix-direnv
flakes. med never runs `direnv allow` itself; after allowing or editing
`.envrc`, `:workspace.reload_env` reloads the environment and restarts the
language servers. `MED_NO_PROJECT_ENV` or `(set-option 'project-env false)`
turns this off.

## Getting started

With no init file, no key is bound except `:`. Run
`:install_default_config` to copy [`examples/`](examples) into the config
directory and load it (`!` overwrites existing files), or load the examples
from a checkout:

```lisp
; ~/.config/med/init.mlisp
(load "/path/to/med/examples/init.mlisp")   ; everything, or pick files:
(load "/path/to/med/examples/vim.mlisp")
(load "/path/to/med/examples/keys.mlisp")
```

| File | What it adds |
| --- | --- |
| `vim.mlisp` | vim keys in every mode, plus nvim-surround-style `ys` / `ds` / `cs` |
| `keys.mlisp` | LSP keys (`gd`, `gr`, `K`), tabs, panes, flash-style jumps, `space` leader keys, `⌘` shortcuts |
| `oil.mlisp` | directories as buffers to edit, as oil.nvim (`space e`, `space -`) |
| `picker.mlisp` | fuzzy file finder, project search, project symbols and recent projects (`space f`, `space /`, `space S`, `space p`) |
| `git.mlisp` | lazygit-style git screen (`space g`) |
| `which-key.mlisp` | the keys that can come next, after a pause |
| `todo.mlisp` | signs on TODO/FIXME/NOTE lines, `]T` / `[T` |
| `tree.mlisp` | a file tree in a sidebar (`space e`, `⌘B`); not loaded by `init.mlisp` |
| `dired.mlisp` | Emacs-style dired and wdired (`space -`); not loaded by `init.mlisp` |
| `statusline.mlisp`, `titlebar.mlisp` | the status and title bars as editable layouts |
| `rss.mlisp` | a feed reader (`:newsticker`); not loaded by `init.mlisp` |

`:help` (`:h`) describes any command and lists the keys bound to it.

## Configuration

Everything the editor does is a named command, and keys are bound to names
in the init file (`<config>/init.mlisp`) or with `:eval`:

```lisp
(bind 'normal "<Space>n" "workspace.next_tab")
(bind 'insert "jk" "insert.escape")
(set-option 'font-size 16)

(defcommand "user.save-and-close" "Write the file, then close its tab"
  (fn ()
    (run "workspace.save")
    (run "workspace.close_tab")))
(bind 'normal "<Space>x" "user.save-and-close")
```

Programs can also add panels, read and edit the buffer, decorate lines and
react to hooks. The reference is in `docs/`:

- [Keys and commands](docs/keys.md): the default keys, key tables, `bind`,
  `defcommand`, options, language servers (`lsp-server`, `deflanguage`)
- [The example programs](docs/examples.md): oil, the tree, picker, git
  screen, which-key and dired in detail
- [Scripting](docs/lisp.md): panels, layouts, the bars, host functions,
  hooks, the buffer API and the Lisp itself

Try the Lisp without the editor: `cargo run -p med-lisp --example repl`.

## Crates

| Crate | What |
| --- | --- |
| `med-core` | text buffer, undo, coordinate conversions |
| `med-vim` | modal editing engine |
| `med-lsp` | JSON-RPC transport for language servers |
| `med-git` | wrapper around the `git` CLI |
| `med-lisp` | scripting language |
| `med` | the GPUI application |

## Releasing

[release-plz](https://release-plz.dev) keeps a release PR open on `main` that
bumps the version and `CHANGELOG.md` from conventional commits. Merging it
tags `v{version}`, and `.github/workflows/release.yml` builds the packages
with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager). To
package locally:

```sh
cargo install cargo-packager --locked
cargo build -p med --release
cargo packager -p med --release --formats app,dmg --out-dir "$PWD/dist"        # macOS
cargo packager -p med --release --formats deb,appimage --out-dir "$PWD/dist"   # Linux
```

A relative `--out-dir` is taken from `crates/med`. To rebuild a release's
packages, run the workflow by hand with its tag.

Each package carries `THIRD-PARTY-LICENSES.txt`, the licences of the crates
med is built from. cargo packager writes it first with
`scripts/third-party-licenses.sh`, so packaging needs
[cargo-about](https://github.com/EmbarkStudios/cargo-about) too (the flake has
it; else `cargo install cargo-about --locked --features cli`). A crate under
a licence not in `about.toml` stops the packaging until it is added there.

## Acknowledgements

- [GPUI](https://www.gpui.rs/) and [Zed](https://zed.dev/), which med is
  built on
- [Vim](https://www.vim.org/), whose modal editing `med-vim` follows
- [oil.nvim](https://github.com/stevearc/oil.nvim),
  [lazygit](https://github.com/jesseduffield/lazygit),
  [which-key.nvim](https://github.com/folke/which-key.nvim) and Emacs's
  dired, whose ideas the programs in [`examples/`](examples) take
- [tree-sitter](https://tree-sitter.github.io/) and its grammars
- [Catppuccin](https://github.com/catppuccin/palette), for the palette of
  the theme (MIT)

## License

MIT. See [LICENSE](LICENSE).
