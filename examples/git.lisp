; The git screen, after lazygit: numbered panels on the left, a preview
; of what is selected on the right, a key for each thing to do.
;
;   (load "git.lisp")
;
; space g, or ⌘⇧G, opens and closes it. In it: 1-5 or tab go to a panel,
; j k move, J K scroll the preview, R reads the repository again, p pulls,
; P pushes, f fetches, q closes. In the files: space stages and unstages,
; a does so for all of them, c commits, A amends, d discards, s stashes,
; enter opens the file. In the branches: space checks out, n makes a new
; one, d deletes. In the stash: space applies, g pops, d drops.
;
; Git is slow enough to be asked off the main thread, so everything that
; reads the repository gives a function to be called with the answer.

(def git-sections '(status files branches commits stash))
(def git-section 'files)   ; the panel the keys go to

(def git-repo true)        ; false once git says this is no repository
(def git-head nil)         ; (:name :ahead :behind :root ...)
(def git-files nil)        ; what `git status` lists
(def git-branches nil)
(def git-commits nil)
(def git-stashes nil)
(def git-selected nil)     ; which row of each panel: (files 2 branches 0 ...)

(def git-preview nil)      ; the right-hand side, a line to a row
(def git-preview-title "")
(def git-preview-top 0)    ; the line at its top
(def git-preview-asked 0)  ; counts the previews asked for: the last one wins

(def git-busy nil)         ; what git is doing now, if anything
(def git-message nil)      ; what it said when done: (text failed?)
(def git-question nil)     ; waiting for a y or an n: (:text :yes)
(def git-prompt 'commit)   ; what the text being asked for is for

(def git-log-limit 300)

(def git-titles
  '(status "Status" files "Files" branches "Branches" commits "Commits"
    stash "Stash"))

(def git-hints
  '(status "p pull  P push  f fetch  R refresh"
    files "space stage  a all  c commit  A amend  d discard  s stash  enter open"
    branches "space checkout  n new  d delete"
    commits "J/K scroll diff"
    stash "space apply  g pop  d drop"))

; ---- what is selected

(defn git-items (section)
  (cond ((= section 'status) (when git-head (list git-head)))
        ((= section 'files) git-files)
        ((= section 'branches) git-branches)
        ((= section 'commits) git-commits)
        (true git-stashes)))

(defn git-index (section)
  (get git-selected section 0))

; The row the keys act on.
(defn git-current ()
  (nth (git-index git-section) (git-items git-section)))

; A list of names and values, with one of the values changed.
(defn git-put (pairs name value)
  (cond ((nil? pairs) (list name value))
        ((= (first pairs) name) (cons name (cons value (rest (rest pairs)))))
        (true (cons (first pairs)
                    (cons (nth 1 pairs)
                          (git-put (rest (rest pairs)) name value))))))

(defn git-clamp (section index)
  (max 0 (min index (- (length (git-items section)) 1))))

; ---- the preview

(defn git-show-preview (title text)
  (set! git-preview-title title)
  (set! git-preview (lines (replace text "\t" "    ")))
  (set! git-preview-top 0))

; A function for git to call with the text to show under `title`, unless
; something else has been asked for by then.
(defn git-preview-as (title)
  (let ((asked (set! git-preview-asked (+ git-preview-asked 1))))
    (fn (text problem)
      (when (= asked git-preview-asked)
        (git-show-preview title (or text problem ""))))))

; One half of a file's diff, under a heading, or nil if it has no such
; half. `then` is called with it.
(defn git-diff-half (file staged heading then)
  (if (get file (if staged :staged :unstaged))
      (git 'diff file staged
        (fn (text problem)
          (then (str heading "\n\n" (or text problem)))))
      (then nil)))

(defn git-commit-line (commit)
  (str (get commit :short) "  " (get commit :subject)
       "  (" (get commit :author) ", " (get commit :date) ")"))

; Show whatever the selection points at.
(defn git-load-preview ()
  (let ((item (git-current)))
    (cond
      ((= git-section 'status)
       (git 'diff-all (git-preview-as "Uncommitted changes")))
      ((nil? item)
       (git-show-preview (get git-titles git-section)
                         (cond ((= git-section 'files) "Working tree clean")
                               ((= git-section 'commits) "No commits yet")
                               ((= git-section 'stash) "No stashed changes")
                               (true ""))))
      ((= git-section 'files)
       (let ((show (git-preview-as (get item :file))))
         (git-diff-half item false "Unstaged changes"
           (fn (unstaged)
             (git-diff-half item true "Staged changes"
               (fn (staged)
                 (show (join (filter (fn (half) half) (list unstaged staged))
                             "\n")
                       nil)))))))
      ((= git-section 'branches)
       (let ((show (git-preview-as (get item :name))))
         (git 'log (get item :name) 100
           (fn (commits problem)
             (show (join (map git-commit-line commits) "\n") problem)))))
      ((= git-section 'commits)
       (git 'show (get item :hash)
         (git-preview-as (str (get item :short) " " (get item :subject)))))
      (true
       (git 'stash-show (get item :index)
         (git-preview-as (get item :message)))))))

(defn git-scroll-preview (delta)
  (set! git-preview-top
        (max 0 (min (+ git-preview-top delta) (- (length git-preview) 1)))))

; ---- moving about

(defn git-select (index)
  (let ((index (git-clamp git-section index)))
    (unless (= index (git-index git-section))
      (set! git-selected (git-put git-selected git-section index))
      (git-load-preview))))

(defn git-go (section)
  (unless (= section git-section)
    (set! git-section section)
    (git-load-preview)))

(defn git-cycle (delta)
  (let ((at (find-index (fn (section) (= section git-section)) git-sections)))
    (git-go (nth (mod (+ at delta) (length git-sections)) git-sections))))

; ---- reading the repository

; A panel's rows have been read again: its selection may be past the end
; now, and what it points at may have changed.
(defn git-loaded (section)
  (set! git-selected
        (git-put git-selected section (git-clamp section (git-index section))))
  (when (= section git-section) (git-load-preview)))

(defn git-refresh ()
  (git 'head
    (fn (head problem)
      (set! git-repo (not (nil? head)))
      (set! git-head head)
      (git-loaded 'status)))
  (git 'status
    (fn (files problem) (set! git-files files) (git-loaded 'files)))
  (git 'branches
    (fn (branches problem) (set! git-branches branches) (git-loaded 'branches)))
  (git 'log nil git-log-limit
    (fn (commits problem) (set! git-commits commits) (git-loaded 'commits)))
  (git 'stashes
    (fn (stashes problem) (set! git-stashes stashes) (git-loaded 'stash))))

; ---- changing it

; Have git do something, say how it went, and read everything again:
; (git-do "stage" 'stage "src/main.rs").
(defn git-do (label & request)
  (set! git-busy label)
  (set! git-message nil)
  (apply git
    (append request
      (list (fn (output problem)
              (set! git-busy nil)
              (set! git-message
                    (if problem
                        (list problem true)
                        (let ((line (trim (or (last (lines output)) ""))))
                          (list (str label ": " (if (= line "") "done" line))
                                false))))
              (git-refresh)
              ; The files on disk may not be what the editor has.
              (run "workspace.sync"))))))

; Do `yes` once the question is answered with a y.
(defn git-ask (question yes)
  (set! git-question (list :text question :yes yes)))

; Ask for a line of text, to commit with or to name a branch.
(defn git-ask-text (kind text)
  (set! git-prompt kind)
  (set-input 'git-prompt 'text text)
  (focus-panel 'git-prompt))

(defn git-submit (text)
  (hide-panel 'git-prompt)
  (let ((text (trim text)))
    (unless (= text "")
      (cond ((= git-prompt 'commit) (git-do "commit" 'commit text))
            ((= git-prompt 'amend) (git-do "amend" 'amend text))
            (true (git-do "branch" 'create-branch text))))))

(defn git-any? (files name)
  (find-index (fn (file) (get file name)) files))

; ---- how it looks

(defn git-row-of (section)
  (cond
    ((= section 'status)
     (fn (head)
       (list (text (path-name (get head :root)))
             (text "→" :color 'muted-foreground)
             (text (get head :name) :color 'success)
             (text (str (if (> (get head :ahead) 0) (str "↑" (get head :ahead) " ") "")
                        (if (> (get head :behind) 0) (str "↓" (get head :behind)) ""))
                   :color 'warning))))
    ((= section 'files)
     (fn (file)
       (list (row :shrink false
               (text (get file :index)
                     :color (if (get file :untracked) 'danger 'success))
               (text (get file :worktree) :color 'danger))
             (text (get file :file)))))
    ((= section 'branches)
     (fn (branch)
       (list (text (if (get branch :current) "*" "")
                   :width 10 :shrink false :color 'success)
             (text (get branch :name)
                   :color (when (get branch :current) 'success))
             (text (get branch :track) :color 'warning)
             (text (get branch :date) :color 'muted-foreground))))
    ((= section 'commits)
     (fn (commit)
       (list (text (get commit :short) :shrink false :color 'warning)
             (unless (= (get commit :refs) "")
               (text (str "(" (get commit :refs) ")")
                     :shrink false :color 'success))
             (text (get commit :subject)))))
    (true
     (fn (stash)
       (list (text (get stash :date) :shrink false :color 'muted-foreground)
             (text (get stash :message)))))))

; One of the panels on the left: a frame, a title, and its rows. `size`
; is the styles that say how much room it gets.
(defn git-box (section number focused size)
  (let ((current (= section git-section))
        (items (git-items section))
        (index (git-index section))
        (cells (git-row-of section)))
    (apply col
      (append size
        (list
          :border 1 :rounded 2
          :border-color (if current 'success 'border)
          :on-click (fn () (focus-panel 'git) (git-go section))
          (row :shrink false :justify 'between :padding-x 8
               :color (if current 'success 'muted-foreground)
            (text (str "[" number "] " (get git-titles section)))
            (text (if (or (nil? items) (= section 'status))
                      ""
                      (str (+ index 1) " of " (length items)))))
          (rows items :grow true :reveal index
            (fn (item at)
              (row :height 22 :width 'full :align 'center
                   :gap 8 :padding-x 8 :nowrap true
                   :bg (when (= at index)
                         (if (and current focused) 'list-active 'list-hover))
                   :on-click (fn ()
                               (focus-panel 'git)
                               (git-go section)
                               (git-select at))
                (cells item)))))))))

(defn git-line-color (line)
  (cond ((or (starts-with? line "+++") (starts-with? line "---")) nil)
        ((starts-with? line "+") 'success)
        ((starts-with? line "-") 'danger)
        ((starts-with? line "@@") 'info)
        ((or (starts-with? line "commit ")
             (starts-with? line "Unstaged changes")
             (starts-with? line "Staged changes"))
         'warning)
        ((or (starts-with? line "diff --git") (starts-with? line "index "))
         'muted-foreground)))

; The line at the bottom: a question, what git is doing, what it said, or
; the keys of the panel.
(defn git-footer ()
  (cond (git-question
         (text (str (get git-question :text) "  (y/n)") :color 'warning))
        (git-busy
         (text (str git-busy "…") :color 'warning))
        (git-message
         (text (first git-message)
               :color (if (nth 1 git-message) 'danger 'success)))
        (true
         (text (str (get git-hints git-section)
                    "  │  1-5 panel  j/k move  J/K scroll  q close")
               :color 'muted-foreground))))

(defpanel 'git :at 'full
  ; A panel's own keys first, then those of the whole screen. While a
  ; question waits, only its answers.
  :contexts (fn ()
              (if git-question
                  '("git.question")
                  (list (str "git." git-section) "git")))
  (fn (panel)
    (if (not git-repo)
        (col :width 'full :height 'full :align 'center :justify 'center
             :bg 'background :color 'muted-foreground
          (text "Not a git repository  ·  q to close"))
        (let ((focused (get panel :focused)))
          (col :width 'full :height 'full
               :bg 'background :color 'foreground :mono true :size 12
            (row :grow true :gap 4 :padding-x 4 :padding-y 4
              (col :grow 36 :gap 4
                (git-box 'status 1 focused (list :height 48 :shrink false))
                (git-box 'files 2 focused (list :grow 3))
                (git-box 'branches 3 focused (list :grow 2))
                (git-box 'commits 4 focused (list :grow 3))
                (git-box 'stash 5 focused (list :grow 1.2)))
              (col :grow 64 :border 1 :border-color 'border :rounded 2 :clip true
                (text git-preview-title :shrink false :padding-x 8
                      :nowrap true :color 'muted-foreground)
                (rows git-preview :grow true :top git-preview-top
                  (fn (line)
                    (text line :height 18 :padding-x 8 :nowrap true
                          :color (git-line-color line))))))
            (row :height 24 :shrink false :padding-x 8 :align 'center
                 :border-top 1 :border-color 'border :nowrap true :clip true
              (git-footer)))))))

(defpanel 'git-prompt :at 'popup :width 560
  (fn (panel)
    (col :width 'full :gap 4 :padding-x 8 :padding-y 8 :mono true :size 12
      (text (cond ((= git-prompt 'commit) "Commit message")
                  ((= git-prompt 'amend) "Amend last commit")
                  (true "New branch"))
            :color 'success)
      (row :border 1 :border-color 'border :rounded 4 :padding-x 4
        (input 'text :grow true
               :placeholder (if (= git-prompt 'branch)
                                "New branch name"
                                "Commit message")
               :on-submit git-submit))
      (text "enter confirm  ·  esc cancel" :color 'muted-foreground))))

; ---- what it does

; A command of the git screen. While git runs, the only way is out.
(defn git-command (name description action)
  (defcommand name description
    (fn () (unless git-busy (action)))))

(defcommand "git.toggle" "Open or close the git screen"
  (fn ()
    (if (panel-shown? 'git)
        (hide-panel 'git)
        (do (git-refresh)
            (focus-panel 'git)))))
(defcommand "git.close" "Leave the git screen"
  (fn () (hide-panel 'git)))

(git-command "git.focus_status" "Go to the status panel"
  (fn () (git-go 'status)))
(git-command "git.focus_files" "Go to the files panel"
  (fn () (git-go 'files)))
(git-command "git.focus_branches" "Go to the branches panel"
  (fn () (git-go 'branches)))
(git-command "git.focus_commits" "Go to the commits panel"
  (fn () (git-go 'commits)))
(git-command "git.focus_stash" "Go to the stash panel"
  (fn () (git-go 'stash)))
(git-command "git.next_panel" "Go to the next panel"
  (fn () (git-cycle 1)))
(git-command "git.prev_panel" "Go to the previous panel"
  (fn () (git-cycle -1)))

(git-command "git.down" "Select the next row"
  (fn () (git-select (+ (git-index git-section) 1))))
(git-command "git.up" "Select the previous row"
  (fn () (git-select (- (git-index git-section) 1))))
(git-command "git.first" "Select the first row"
  (fn () (git-select 0)))
(git-command "git.last" "Select the last row"
  (fn () (git-select (length (git-items git-section)))))

(git-command "git.preview_down" "Scroll the preview down"
  (fn () (git-scroll-preview 10)))
(git-command "git.preview_up" "Scroll the preview up"
  (fn () (git-scroll-preview -10)))
(git-command "git.preview_page_down" "Scroll the preview down by more"
  (fn () (git-scroll-preview 15)))
(git-command "git.preview_page_up" "Scroll the preview up by more"
  (fn () (git-scroll-preview -15)))

(git-command "git.refresh" "Read the repository again" git-refresh)
(git-command "git.pull" "Pull" (fn () (git-do "pull" 'pull)))
(git-command "git.push" "Push" (fn () (git-do "push" 'push)))
(git-command "git.fetch" "Fetch" (fn () (git-do "fetch" 'fetch)))

(git-command "git.toggle_stage"
  "Stage the selected file, or unstage it if fully staged"
  (fn ()
    (let ((file (git-current)))
      (when file
        (if (get file :unstaged)
            (git-do "stage" 'stage (get file :file))
            (git-do "unstage" 'unstage (get file :file)))))))

(git-command "git.toggle_stage_all"
  "Stage everything, or unstage everything if nothing is left"
  (fn ()
    (if (git-any? git-files :unstaged)
        (git-do "stage all" 'stage-all)
        (git-do "unstage all" 'unstage-all))))

(git-command "git.commit" "Commit what is staged"
  (fn ()
    (if (git-any? git-files :staged)
        (git-ask-text 'commit "")
        (set! git-message (list "Nothing staged to commit" true)))))

(git-command "git.amend" "Amend the last commit"
  (fn () (git-ask-text 'amend (get (first git-commits) :subject ""))))

(git-command "git.discard" "Discard the selected file's changes, after asking"
  (fn ()
    (let ((file (git-current)))
      (when file
        (git-ask (str "Discard all changes to " (get file :file)
                      "? This cannot be undone.")
                 (fn () (git-do "discard" 'discard file)))))))

(git-command "git.stash" "Stash all changes"
  (fn () (git-do "stash" 'stash)))

(git-command "git.open_file" "Open the selected file in an editor"
  (fn ()
    (let ((file (git-current)))
      (when file
        (hide-panel 'git)
        (open-file (get file :path))))))

(git-command "git.checkout" "Check out the selected branch"
  (fn ()
    (let ((branch (git-current)))
      (when (and branch (not (get branch :current)))
        (git-do "checkout" 'checkout (get branch :name))))))

(git-command "git.new_branch" "Create a branch"
  (fn () (git-ask-text 'branch "")))

(git-command "git.delete_branch" "Delete the selected branch, after asking"
  (fn ()
    (let ((branch (git-current)))
      (when (and branch (not (get branch :current)))
        (git-ask (str "Delete branch " (get branch :name) "?")
                 (fn () (git-do "delete branch"
                                'delete-branch (get branch :name))))))))

(git-command "git.stash_apply" "Apply the selected stash"
  (fn ()
    (let ((stash (git-current)))
      (when stash (git-do "stash apply" 'stash-apply (get stash :index))))))

(git-command "git.stash_pop" "Apply the selected stash and drop it"
  (fn ()
    (let ((stash (git-current)))
      (when stash (git-do "stash pop" 'stash-pop (get stash :index))))))

(git-command "git.stash_drop" "Drop the selected stash, after asking"
  (fn ()
    (let ((stash (git-current)))
      (when stash
        (git-ask (str "Drop stash \"" (get stash :message)
                      "\"? This cannot be undone.")
                 (fn () (git-do "stash drop"
                                'stash-drop (get stash :index))))))))

(defcommand "git.yes" "Go ahead with what was asked"
  (fn ()
    (let ((yes (get git-question :yes)))
      (set! git-question nil)
      (when yes (yes)))))
(defcommand "git.no" "Do not go ahead with what was asked"
  (fn () (set! git-question nil)))

; ---- its keys

(defn git-keys (table command & keys)
  (map (fn (key) (bind table key command)) keys))

(git-keys 'git "git.close" "q" "<Esc>")
(git-keys 'git "git.focus_status" "1")
(git-keys 'git "git.focus_files" "2")
(git-keys 'git "git.focus_branches" "3")
(git-keys 'git "git.focus_commits" "4")
(git-keys 'git "git.focus_stash" "5")
(git-keys 'git "git.next_panel" "<Tab>" "l" "<Right>")
(git-keys 'git "git.prev_panel" "<S-Tab>" "h" "<Left>")
(git-keys 'git "git.down" "j" "<Down>")
(git-keys 'git "git.up" "k" "<Up>")
(git-keys 'git "git.first" "<lt>")
(git-keys 'git "git.last" ">")
(git-keys 'git "git.preview_down" "J" "<PageDown>")
(git-keys 'git "git.preview_up" "K" "<PageUp>")
(git-keys 'git "git.preview_page_down" "<C-d>")
(git-keys 'git "git.preview_page_up" "<C-u>")
(git-keys 'git "git.refresh" "R")
(git-keys 'git "git.pull" "p")
(git-keys 'git "git.push" "P")
(git-keys 'git "git.fetch" "f")

(git-keys 'git.files "git.toggle_stage" "<Space>")
(git-keys 'git.files "git.toggle_stage_all" "a")
(git-keys 'git.files "git.commit" "c")
(git-keys 'git.files "git.amend" "A")
(git-keys 'git.files "git.discard" "d")
(git-keys 'git.files "git.stash" "s")
(git-keys 'git.files "git.open_file" "<CR>" "e" "o")

(git-keys 'git.branches "git.checkout" "<Space>" "<CR>")
(git-keys 'git.branches "git.new_branch" "n")
(git-keys 'git.branches "git.delete_branch" "d")

(git-keys 'git.stash "git.stash_apply" "<Space>")
(git-keys 'git.stash "git.stash_pop" "g")
(git-keys 'git.stash "git.stash_drop" "d")

(git-keys 'git.question "git.yes" "y" "Y" "<CR>")
(git-keys 'git.question "git.no" "n" "N" "q" "<Esc>")

(bind 'normal "<Space>g" "git.toggle")
(bind 'app "cmd-shift-g" "git.toggle")
