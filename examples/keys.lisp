; The editor's keys beyond vim's: the ones that reach outside the buffer
; (the language server, tabs and panes, the jump list), flash, the `space`
; leader keys and the `cmd` shortcuts. Every right-hand side is a command name,
; and `:` and the name runs it as well, as in `:editor.hover`.
;
; Load it after `vim.lisp`: some of these keys are vim's own there, and
; the later binding wins.
;
;   (load "/path/to/med/examples/vim.lisp")
;   (load "/path/to/med/examples/keys.lisp")

; The language server.
(bind '(normal visual) "gd" "editor.goto_definition")
(bind '(normal visual) "gD" "editor.goto_definition")
(bind '(normal visual) "gr" "editor.references")
(bind '(normal visual) "gh" "editor.hover")
(bind '(normal visual) "K" "editor.hover")
(bind '(normal visual) "]d" "editor.next_diagnostic")
(bind '(normal visual) "[d" "editor.prev_diagnostic")
(bind 'insert "<C-n>" "editor.complete")
(bind 'insert "<C-p>" "editor.complete")
(bind 'insert "<C-Space>" "editor.complete")
(bind 'insert "<C-s>" "editor.signature_help")

; Tabs, and the places the cursor has been.
(bind '(normal visual) "gt" "workspace.next_tab")
(bind '(normal visual) "gT" "workspace.prev_tab")
(bind '(normal visual) "<C-o>" "workspace.jump_back")
(bind '(normal visual) "<C-i>" "workspace.jump_forward")
(bind '(normal visual) "<Tab>" "workspace.jump_forward")

; Panes, as vim's windows: ctrl-w, then a letter. A new pane has an empty
; buffer; a tab is in one pane, and opening it in another brings it over.
(bind 'normal "<C-w>v" "workspace.split_right")
(bind 'normal "<C-w>s" "workspace.split_down")
(bind 'normal "<C-w>h" "workspace.pane_left")
(bind 'normal "<C-w>j" "workspace.pane_down")
(bind 'normal "<C-w>k" "workspace.pane_up")
(bind 'normal "<C-w>l" "workspace.pane_right")
(bind 'normal "<C-w>w" "workspace.next_pane")
(bind 'normal "<C-w><C-w>" "workspace.next_pane")
(bind 'normal "<C-w>W" "workspace.prev_pane")
(bind 'normal "<C-w>c" "workspace.close_pane")
(bind 'normal "<C-w>o" "workspace.only_pane")
(bind 'normal "<C-w>q" "workspace.quit")

; To text on screen, by typing some of it and then its label. In place of
; vim's `s`, which `cl` and `c` also are; after an operator `s` is
; surround's, so there it is `z`.
(bind '(normal visual) "s" "motion.flash")
(bind 'operator "z" "motion.flash")
; The same for an operator to work there, and come back.
(bind 'operator "r" "motion.flash_remote")
; A syntax node around the cursor, by its label. In place of vim's `S`,
; which `cc` also is; on a selection `S` is surround's.
(bind '(normal operator) "S" "select.flash")
; `f` and `t` on every line on screen, with where they could go next
; marked, and their key going on from there.
(bind '(normal visual operator) "f" "motion.flash_find_forward")
(bind '(normal visual operator) "F" "motion.flash_find_backward")
(bind '(normal visual operator) "t" "motion.flash_till_forward")
(bind '(normal visual operator) "T" "motion.flash_till_backward")

; Leader keys. `space` alone still moves right, after a pause.
(bind '(normal visual) "<Space>a" "editor.code_action")
(bind 'normal "<Space>=" "editor.format")
(bind 'normal "<Space>w" "workspace.save")
(bind 'normal "<Space>q" "workspace.close_tab")
; The new name is typed on the command line.
(bind-keys 'normal "<Space>r" ":rename<Space>")

; Shortcuts of the window's, wherever the keyboard is.
(bind 'app "cmd-q" "workspace.quit_all")
(bind 'app "cmd-s" "workspace.save")
(bind 'app "cmd-w" "workspace.close_tab")
(bind 'app "cmd-shift-]" "workspace.next_tab")
(bind 'app "cmd-shift-[" "workspace.prev_tab")
(bind 'app "ctrl-tab" "workspace.next_tab")
(bind 'app "ctrl-shift-tab" "workspace.prev_tab")
(bind 'app "cmd-v" "editor.paste")
(bind 'app "cmd-c" "editor.copy")
