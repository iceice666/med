# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1](https://github.com/iceice666/med/compare/v0.2.0...v0.2.1) - 2026-11-16

### Added

- *(med/lsp)* show the hover of every server
- *(med/lsp)* merge completions from every server
- *(med/lsp)* go to the declaration of a symbol
- *(med/workspace)* switch project from the title bar
- *(med/editor)* add sticky scroll
- *(med/workspace)* open each project in its own window

### Fixed

- *(med/workspace)* run the empty tab's commands once

## [0.2.0](https://github.com/iceice666/med/compare/v0.1.1...v0.2.0) - 2026-11-16

### Added

- *(med/panel)* let a program's bar draw the : cursor
- *(med/lsp)* set up language servers from the init file
- *(med/workspace)* say in a popup how to get keys
- *(med/editor)* draw indent guides
- *(med/editor)* show inlay hints
- *(med/editor)* add a theme option
- *(med-vim/motion)* move by rows of the screen with gj and gk
- *(med/editor)* add a wrap option
- *(med/editor)* keep undo history across runs
- *(med/editor)* add cursor-line and color-column options
- *(med/editor)* add a scroll-off option
- *(med/editor)* let a tab have options of its own
- *(med/editor)* add a relative-number option
- *(med/workspace)* restore a project's session
- *(med/editor)* add a tab-width option
- *(med/syntax)* highlight med-lisp files
- *(med/examples)* [**breaking**] name med-lisp files .mlisp
- *(med/examples)* edit directories as oil.nvim does
- *(med/script)* make empty files and read every scratch tab
- *(med/examples)* hide the file tree until asked for
- *(med/examples)* list a table's keys at once on ?
- *(med/script)* drive terminals from programs
- *(med/workspace)* open a terminal in a pane
- *(med-term/terminal)* run programs on a pty

### Fixed

- *(med/editor)* compose with the input method on the : line
- *(med/app)* let tab reach the editor in a window
- *(med-vim/visual)* keep gv on the lines edits moved
- *(med/vim)* match * and # in any case

## [0.1.1](https://github.com/iceice666/med/compare/v0.1.0...v0.1.1) - 2026-11-16

### Added

- *(med/workspace)* preview Markdown in a split pane
- *(med/panel)* render Markdown in panel layouts
- *(med/examples)* offer the next pane's listing in dired
- *(med/workspace)* split the window into panes
- *(med/cmdline)* move the cursor and recall history
- *(med/examples)* read Tsoding's feeds in a hidden newsticker
- *(med/env)* run servers in the project environment
- *(med/completion)* rank completions with frizbee
- *(med/cmdline)* complete the command line with tab

### Fixed

- *(med/app)* start from the launch dir in an AppImage
- *(med/app)* open home instead of / from Finder

### Other

- *(med/readme)* move the reference into docs

## [0.1.0](https://github.com/iceice666/med/releases/tag/v0.1.0) - 2026-11-15

### Added

- *(med/examples)* switch branches from the title bar
- *(med/workspace)* open an empty buffer with no file
- *(med/examples)* make the listing a full dired
- *(med/host)* add file, process and regex natives
- *(med/help)* add :help for commands
- *(med/panel)* let programs draw the status and title bars
- *(med/examples)* give listings keys of their own
- *(med/script)* give tabs keys of their own
- *(med/examples)* edit directories as text
- *(med/script)* add scratch tabs and file operations
- *(med/keys)* move every key binding into examples
- *(med/vim)* add the rest of flash
- *(med/vim)* jump to text on screen by label
- *(med/examples)* add signs on todo lines
- *(med/script)* let programs read and edit the file
- *(med/lsp)* add rename, code actions, formatting
- *(med/vim)* add marks, macros, blocks, surround
- *(med/examples)* add a which-key panel
- *(med/script)* let programs follow keys being typed
- *(med/panel)* add panels at the bottom of the window
- *(med/panel)* move built-in panels to lisp
- *(med/panel)* let scripts define sidebars and popups
- *(med/panel)* write panel layouts in lisp
- *(med/script)* script the editor in lisp
- *(med/panel)* draw the file tree from a layout file
- *(med/keymap)* bind keys to named commands
- *(med/app)* add gpui editor application

### Fixed

- *(med/app)* keep status message until next key

### Other

- *(med/packaging)* package med for macOS and Linux
