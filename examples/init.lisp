; An init file that has most of what is in this directory: the vim keys
; and the editor's, directories as buffers to edit (as oil.nvim), the
; picker, the git screen, the keys that can come next, the signs on the
; lines that say TODO, and the status and title bars. Put a line like this
; in your own `init.lisp`, with the path of your checkout:
;
;   (load "/path/to/med/examples/init.lisp")
;
; or copy the files beside your `init.lisp`, take what you want of them,
; and change it: they are programs like any other you would write there.
; The file tree in a sidebar (tree.lisp), Emacs's dired (dired.lisp) and
; the feed reader (rss.lisp) are here too, but not loaded: a (load ...)
; of one after this line has it.

(load "vim.lisp")
(load "keys.lisp")
(load "oil.lisp")
(load "picker.lisp")
(load "git.lisp")
(load "which-key.lisp")
(load "todo.lisp")
(load "statusline.lisp")
(load "titlebar.lisp")

; g ? in a listing shows all of its keys at once. Last, so that every key
; bound to them above is there.
(which-key-help-key 'oil "g?")
