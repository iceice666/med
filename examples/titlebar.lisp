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
;
; A click on the branch lists the project's branches, and a click on one
; checks it out; so does the command `titlebar.branches`.

(defpanel 'titlebar :at 'title
  (fn (panel)
    (row :width 'full :height 'full :padding-right 12 :gap 12
         :align 'center :size 12
      (text (or (get panel :path) (get panel :scratch) "med")
            :grow true :nowrap true :clip true
            :color (if (get panel :modified) 'warning 'muted-foreground))
      (text (path-name (get panel :root)) :color 'muted-foreground)
      (when (get panel :branch)
        (row :padding-x 6 :rounded 4 :hover (list :bg 'list-hover)
             :on-click "titlebar.branches"
          (text (str " " (get panel :branch)) :color 'success))))))

; ---- the branches

(def titlebar-branches nil)   ; each (:name :current :upstream :subject ...)
(def titlebar-row 26)         ; how tall a row is
(def titlebar-rows 12)        ; and how many are on show at once

(defpanel 'titlebar-branches :at 'popup :width 420
  (fn (panel)
    (col :width 'full :size 12
      (text "Switch branch" :color 'muted-foreground :padding-x 12 :padding-y 6
            :border-bottom 1 :border-color 'border)
      (rows titlebar-branches
            :height (* titlebar-row (min titlebar-rows (length titlebar-branches)))
        (fn (branch)
          (row :height titlebar-row :width 'full :align 'center
               :gap 12 :padding-x 12 :nowrap true
               :hover (list :bg 'list-hover)
               :on-click (fn () (titlebar-checkout branch))
            (text (str (if (get branch :current) "● " "  ") (get branch :name))
                  :shrink false :mono true
                  :color (when (get branch :current) 'success))
            (text (or (get branch :subject) "")
                  :grow true :clip true :color 'muted-foreground)))))))

(defn titlebar-checkout (branch)
  (hide-panel 'titlebar-branches)
  (unless (get branch :current)
    (git 'checkout (get branch :name)
      (fn (output problem)
        (if problem
            (message problem)
            ; The branch in the bar, and the files on disk, have changed.
            (run "workspace.sync"))))))

(defcommand "titlebar.branches" "Switch to another branch of the project"
  (fn ()
    (git 'branches
      (fn (branches problem)
        (if problem
            (message problem)
            (do (set! titlebar-branches branches)
                (show-panel 'titlebar-branches)))))))
