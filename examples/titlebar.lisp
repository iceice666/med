; The title bar, as a layout of its own: what is being edited in the
; middle, and the project and its branch at the right.
;
;   (load "titlebar.lisp")
;
; A panel at 'title is drawn in the window's title bar, which can still
; be dragged to move the window and double-clicked to zoom it. On macOS
; it starts after the window's buttons. Its `panel` has what the status
; bar's has (the README, under "The bars"), `:title` among it: the
; window's own title, "file — med".

(defpanel 'titlebar :at 'title
  (fn (panel)
    (row :width 'full :height 'full :padding-right 12 :gap 12
         :align 'center :size 12
      (text (or (get panel :path) (get panel :scratch) "med")
            :grow true :nowrap true :clip true
            :color (if (get panel :modified) 'warning 'muted-foreground))
      (text (path-name (get panel :root)) :color 'muted-foreground)
      (when (get panel :branch)
        (text (str " " (get panel :branch)) :color 'success)))))
