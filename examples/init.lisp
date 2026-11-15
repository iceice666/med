; An init file that has all the panels of this directory: the file tree,
; the picker, the git screen and the keys that can come next. Put a line
; like this in your own `init.lisp`, with the path of your checkout:
;
;   (load "/path/to/med/examples/init.lisp")
;
; or copy the files beside your `init.lisp`, take what you want of them,
; and change it: they are programs like any other you would write there.

(load "tree.lisp")
(load "picker.lisp")
(load "git.lisp")
(load "which-key.lisp")

; The leader keys of an editor work from the tree as well.
(bind 'tree "<Space>f" "picker.files")
(bind 'tree "<Space>/" "picker.search")
(bind 'tree "<Space>g" "git.toggle")
