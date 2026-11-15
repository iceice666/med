; The file tree: a sidebar that lists the project's files.
;
;   (load "tree.lisp")
;
; space e, or ⌘⇧E, goes to the tree and back; ⌘B shows and hides it. In
; the tree: j k move, h l fold and unfold, enter opens, R reads the disk
; again, q goes back to the editor. A click does what enter does.
;
; Everything here is an ordinary program: what the tree shows is kept in
; the names below, the commands change them, and the layout draws them.

(def tree-root nil)      ; the project's directory
(def tree-open nil)      ; the directories that are unfolded, by path
(def tree-entries nil)   ; the rows on show, each (:name :path :dir :depth)
(def tree-selected 0)    ; which row
(def tree-marks nil)     ; git's letter for a path: (path letter path letter...)

; ---- what is on show

; The rows for a directory: what is in it, and under each unfolded
; directory what is in that.
(defn tree-rows (dir depth)
  (reduce
    (fn (rows entry)
      (append rows
              (list (append entry (list :depth depth)))
              (when (and (get entry :dir) (member? (get entry :path) tree-open))
                (tree-rows (get entry :path) (+ depth 1)))))
    nil
    (list-dir dir)))

(defn tree-entry ()
  (nth tree-selected tree-entries))

(defn tree-select (index)
  (set! tree-selected (max 0 (min index (- (length tree-entries) 1)))))

(defn tree-index-of (path)
  (find-index (fn (entry) (= (get entry :path) path)) tree-entries))

; A changed file is marked with git's letter for it, and every directory
; above it with M.
(defn tree-mark-above (marks dir)
  (if (or (nil? dir) (= dir tree-root) (not (starts-with? dir tree-root)))
      marks
      (tree-mark-above (if (get marks dir) marks (append (list dir "M") marks))
                       (path-parent dir))))

(defn tree-mark (marks file)
  (let ((path (get file :path))
        (letter (cond ((get file :untracked) "?")
                      ((= (get file :worktree) " ") (get file :index))
                      (true (get file :worktree)))))
    (append (list path letter) (tree-mark-above marks (path-parent path)))))

; Read the directories again, keeping the selection on the path it is on,
; and ask git what has changed.
(defn tree-refresh ()
  (when tree-root
    (let ((path (get (tree-entry) :path)))
      (set! tree-entries (tree-rows tree-root 0))
      (tree-select (or (tree-index-of path) tree-selected)))
    (git 'status
      (fn (files problem)
        (set! tree-marks (reduce tree-mark nil files))))))

(defn tree-toggle (entry)
  (let ((path (get entry :path)))
    (set! tree-open
          (if (member? path tree-open)
              (filter (fn (open) (not (= open path))) tree-open)
              (cons path tree-open)))
    (tree-refresh)))

; Open the file, or fold or unfold the directory, that is selected.
(defn tree-activate ()
  (let ((entry (tree-entry)))
    (cond ((nil? entry) nil)
          ((get entry :dir) (tree-toggle entry))
          (true (open-file (get entry :path))))))

; Unfold down to a file and select it.
(defn tree-reveal (path)
  (when (and path tree-root (starts-with? path tree-root))
    (let ((dir (path-parent path)))
      (while (and dir (not (= dir tree-root)))
        (unless (member? dir tree-open)
          (set! tree-open (cons dir tree-open)))
        (set! dir (path-parent dir))))
    (tree-refresh)
    (let ((index (tree-index-of path)))
      (when index (tree-select index)))))

; ---- how it looks

(defpanel 'tree :at 'left :width 260 :shown true
  (fn (panel)
    (col :width 'full :height 'full
         :bg 'sidebar :color 'sidebar-foreground
      (row :height 32 :shrink false :padding-x 12 :align 'center
           :border-bottom 1 :border-color 'sidebar-border
           :bold true
           :color (if (get panel :focused) 'sidebar-foreground 'muted-foreground)
        (text (path-name (get panel :root))))
      (rows tree-entries :grow true :reveal tree-selected
        (fn (entry index)
          (let ((mark (get tree-marks (get entry :path))))
            (row :height 24 :width 'full :align 'center
                 :padding-left (+ 10 (* 14 (get entry :depth)))
                 :padding-right 8
                 :nowrap true
                 :color (cond ((member? mark '("?" "A")) 'success)
                              (mark 'warning))
                 :bg (when (= index tree-selected)
                       (if (get panel :focused) 'list-active 'list-hover))
                 :hover (list :bg 'list-hover)
                 :on-click (fn ()
                             (focus-panel 'tree)
                             (tree-select index)
                             (tree-activate))
              (text (cond ((not (get entry :dir)) "  ")
                          ((member? (get entry :path) tree-open) "▾ ")
                          (true "▸ "))
                    :color 'muted-foreground)
              (text (get entry :name)))))))))

; ---- what it does

(defn tree-move (delta)
  (tree-select (+ tree-selected delta)))

(defcommand "tree.down" "Select the next entry" (fn () (tree-move 1)))
(defcommand "tree.up" "Select the previous entry" (fn () (tree-move -1)))
(defcommand "tree.page_down" "Move the selection down ten entries"
  (fn () (tree-move 10)))
(defcommand "tree.page_up" "Move the selection up ten entries"
  (fn () (tree-move -10)))
(defcommand "tree.first" "Select the first entry" (fn () (tree-select 0)))
(defcommand "tree.last" "Select the last entry"
  (fn () (tree-select (length tree-entries))))

(defcommand "tree.collapse" "Fold the directory, or go to its parent"
  (fn ()
    (let ((entry (tree-entry)))
      (if (and (get entry :dir) (member? (get entry :path) tree-open))
          (tree-toggle entry)
          (let ((parent (tree-index-of (path-parent (get entry :path "")))))
            (when parent (tree-select parent)))))))

(defcommand "tree.expand" "Unfold the directory, or open the file"
  (fn ()
    (let ((entry (tree-entry)))
      (unless (and (get entry :dir) (member? (get entry :path) tree-open))
        (tree-activate)))))

(defcommand "tree.open" "Open the file, or fold or unfold the directory"
  tree-activate)
(defcommand "tree.refresh" "Read the directories again" tree-refresh)
(defcommand "tree.close" "Go back to the editor" focus-editor)

(defcommand "tree.focus" "Go to the file tree, or back from it"
  (fn ()
    (if (panel-focused? 'tree)
        (focus-editor)
        (do (tree-reveal (current-file))
            (focus-panel 'tree)))))

(defcommand "tree.toggle" "Show or hide the file tree"
  (fn () (toggle-panel 'tree)))

; ---- its keys

(defn tree-keys (command & keys)
  (map (fn (key) (bind 'tree key command)) keys))

(tree-keys "tree.down" "j" "<Down>")
(tree-keys "tree.up" "k" "<Up>")
(tree-keys "tree.page_down" "<C-d>")
(tree-keys "tree.page_up" "<C-u>")
(tree-keys "tree.collapse" "h" "<Left>")
(tree-keys "tree.expand" "l" "<Right>")
(tree-keys "tree.open" "<CR>" "o")
(tree-keys "tree.first" "gg")
(tree-keys "tree.last" "G")
(tree-keys "tree.refresh" "R")
(tree-keys "tree.close" "q" "<Esc>")
(tree-keys "tree.focus" "<Space>e")

(bind 'normal "<Space>e" "tree.focus")
(bind 'app "cmd-shift-e" "tree.focus")
(bind 'app "cmd-b" "tree.toggle")

; ---- when to read the disk

(defn tree-start ()
  (set! tree-root (project-root))
  (tree-refresh))

(add-hook 'opened tree-start)
(add-hook 'files-changed tree-refresh)
; Loaded with a window already up: there is a project to show right away.
(when (project-root) (tree-start))
