; The status bar, as a layout of its own: the same bar the window draws
; when no program gives it one, to change as you like.
;
;   (load "statusline.lisp")
;
; A panel at 'status is drawn in place of the window's status bar, and
; its `panel` says what that bar would show: `:mode`, `:path`, `:line`,
; `:col`, `:cmdline` and the rest, listed in docs/lisp.md under "The bars".
; Whatever else it shows is the program's own, as in any panel. A layout
; that fails gives way to the window's bar, which says why.
;
; Draw `:cmdline` somewhere: it is the command line being typed, and
; without it `:` is typed blind.

(def statusline-height 26)

; The mode's color, behind its name.
(defn statusline-mode-color (mode)
  (cond
    ((= mode 'insert) 'success)
    ((= mode 'command) 'primary)
    ((or (= mode 'visual) (= mode 'visual-line) (= mode 'visual-block)) 'warning)
    (true 'info)))

; What is being edited, and whether it has changes not written.
(defn statusline-name (panel)
  (str (or (get panel :path) (get panel :scratch) "[No Name]")
       (if (get panel :modified) " [+]" "")))

; The left of the bar: the command line while it is open, or else the
; file and what there is to say about it.
(defn statusline-left (panel)
  (if (get panel :cmdline)
    (text (str (get panel :cmdline) "▏") :grow true :nowrap true :clip true)
    (row :grow true :gap 12 :nowrap true :clip true
      (text (statusline-name panel) :nowrap true)
      (text (or (get panel :message) (get panel :diagnostic) "")
            :color 'muted-foreground :nowrap true :clip true :grow true))))

(defn statusline-diagnostics (panel)
  (when (> (+ (get panel :errors) (get panel :warnings)) 0)
    (row :gap 8
      (text (str "E:" (get panel :errors)) :color 'danger)
      (text (str "W:" (get panel :warnings)) :color 'warning))))

(defpanel 'statusline :at 'status
  (fn (panel)
    (row :width 'full :height statusline-height :padding-x 8 :gap 12
         :align 'center :bg 'status-bar :border-top 1 :border-color 'border
         :size 12 :mono true
      (if (nil? (get panel :mode))
        ; No file open.
        (text (or (get panel :message) "med") :color 'muted-foreground :grow true)
        (list
          (text (get panel :mode-label) :padding-x 8 :rounded 3
                :bg (statusline-mode-color (get panel :mode)) :color 'background)
          (statusline-left panel)
          (when (get panel :recording)
            (text (str "recording @" (get panel :recording)) :color 'danger))
          (when (get panel :pending)
            (text (get panel :pending) :color 'warning))
          (statusline-diagnostics panel)
          (when (get panel :lsp)
            (text (get panel :lsp) :color 'muted-foreground))
          (when (get panel :branch)
            (text (str " " (get panel :branch)) :color 'success))
          (text (get panel :language) :color 'muted-foreground)
          (text (str (get panel :line) ":" (get panel :col))))))))
