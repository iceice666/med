; Oil: a directory as a buffer to edit, as in oil.nvim.
;
;   (load "oil.lisp")
;
; space - lists the directory of the file being edited, in a tab called
; oil:///that/directory/, the cursor on the file's line: a name to a
; line, a directory's with / after it. space e does the same, and in a
; listing closes it again; so does ⌘⇧E. Oil.nvim's own key for it is -,
; which vim.lisp gives to a motion; to have it the oil way:
;
;   (bind 'normal "-" "oil.open")
;
; The listing is text like any other, to be edited with vim's keys, and
; :w does to the disk what the edits ask for, once a popup has listed it
; all and y (or enter) has said yes; n, q or escape does nothing:
;
;   a name changed      moves the file: in place, or with a / in the name
;                       to a directory below (sub/a.txt) or above
;                       (../a.txt), made if need be
;   a line taken out    puts the file in the trash, or with
;                       `oil-delete-forever` does away with it
;   a line put in       makes an empty file, or with / after it a directory
;   a line cut (dd) and put (p) in another listing
;                       moves the file there; yy and p copy it
;
; Each line is known to be the file it was put up as while it is edited,
; the way oil.nvim knows it by a number hidden on the line: a line changed
; in place is still its file, a line taken out and put back (dd p, or an
; undo) is too, and a name put in that no file had is a new file. A :w
; writes every listing with changes, not only the one it is typed in, so
; that a file cut from one and put in another is moved, not lost.
; Nothing is done unless all of it can be: no two lines may say the same
; name, nor one say a name that is there already, unless that file is
; taken out in the same write.
;
; In a listing (the table `oil`):
;
;   enter      open the file, or list the directory
;   -          the directory above        _       the project's root
;   g .        the hidden files too, or not
;   C-l        read the disk again, giving up any changes (it asks first)
;   g x        open the file with the system's program for it
;   C-c        close the listing
;   g ?        all of these keys, with which-key.lisp (as init.lisp has it)
;
; C-c, or space e, goes back to the file the listing was opened from.
; Going to another file or directory closes the listing it is gone from,
; as oil.nvim's buffer gives its window up, unless it has changes not yet
; written: those stay in their tab, to be written with the next :w.

(def oil-show-hidden false)       ; g .: what `oil-hidden?` says is hidden, too
(def oil-delete-forever false)    ; true to skip the trash
(def oil-from nil)                ; the file listed from, for C-c to go back to

; What is hidden until g .: the names that start with a dot.
(defn oil-hidden? (entry)
  (starts-with? (get entry :name) "."))

(defkeys 'oil)

; ---- lists

(defn oil-take (count items)
  (let ((out nil))
    (while (and (> count 0) items)
      (set! out (cons (first items) out))
      (set! items (rest items))
      (set! count (- count 1)))
    (reverse out)))

(defn oil-drop (count items)
  (while (and (> count 0) items)
    (set! items (rest items))
    (set! count (- count 1)))
  items)

; How many items from the front of a and b are the same, up to `limit`.
(defn oil-same-count (a b limit)
  (let ((count 0))
    (while (and (< count limit) (= (first a) (first b)))
      (set! count (+ count 1))
      (set! a (rest a))
      (set! b (rest b)))
    count))

(defn oil-assoc (pairs key value)
  (cond ((nil? pairs) (list key value))
        ((= (first pairs) key) (cons key (cons value (rest (rest pairs)))))
        (true (cons (first pairs)
                    (cons (nth 1 pairs) (oil-assoc (rest (rest pairs)) key value))))))

; ---- names

(defn oil-tab-name (dir)
  (str "oil://" (if (ends-with? dir "/") dir (str dir "/"))))

; The directory of a listing, by the name of its tab; nil for another tab.
(defn oil-dir-of (name)
  (when (and name (starts-with? name "oil:///"))
    (let ((dir (regex-replace "^oil://" name "")))
      (if (= dir "/") dir (regex-replace "/+$" dir "")))))

; The directory of the listing up, or nil.
(defn oil-here ()
  (oil-dir-of (scratch-name)))

(defn oil-line (entry)
  (str (get entry :name) (if (get entry :dir) "/" "")))

; A line as written: (:name :dir :text), the name without the / after it,
; or nil for a line with nothing on it.
(defn oil-parse (line)
  (let ((text (trim (or line ""))))
    (unless (= text "")
      (list :name (regex-replace "/+$" text "")
            :dir (ends-with? text "/")
            :text text))))

; The line the cursor is on, as `oil-parse` has it.
(defn oil-at-cursor ()
  (oil-parse (buffer-line (car (cursor)))))

; Where a name written in the listing of `dir` is, its . and .. taken
; away: sub/a.txt, ../a.txt.
(defn oil-path (dir name)
  (let ((parts nil))
    (map (fn (part)
           (cond ((or (= part "") (= part ".")) nil)
                 ((= part "..") (set! parts (rest parts)))
                 (true (set! parts (cons part parts)))))
         (split (path-join dir name) "/"))
    (str "/" (join (reverse parts) "/"))))

; A path as the popup shows it: from the project's root, if it is in it.
(defn oil-say (path dir?)
  (let ((root (project-root))
        (inside (and root (starts-with? path (str root "/")))))
    (str (if inside (replace path (str root "/") "") path) (if dir? "/" ""))))

; ---- what is kept of each listing

; By directory, (:listed :lines :ids): the entries as they were put up;
; the lines as they were last seen; and for each of those lines, the
; file it is, an entry of this listing or of another, or nil for a line
; put in.
(def oil-states nil)

; The files whose lines were taken out, the last first, for a line put
; in with one of their names to be that file again.
(def oil-cut nil)

(defn oil-state (dir)
  (get oil-states dir))

(defn oil-put (dir listed lines ids)
  (set! oil-states (oil-assoc oil-states dir (list :listed listed :lines lines :ids ids))))

(defn oil-same? (a b)
  (and a b (= (get a :path) (get b :path))))

; The entry named so among `ids`, or nil.
(defn oil-find (name ids)
  (let ((found nil))
    (while (and ids (not found))
      (when (and (first ids) (= (get (first ids) :name) name))
        (set! found (first ids)))
      (set! ids (rest ids)))
    found))

; The file cut last by that name, taken off the list of those cut.
(defn oil-uncut (name)
  (let ((found (oil-find name oil-cut)))
    (when found
      (set! oil-cut (filter (fn (id) (not (oil-same? id found))) oil-cut)))
    found))

(defn oil-anywhere (name)
  (let ((found nil) (states oil-states))
    (while (and states (not found))
      (set! found (oil-find name (get (nth 1 states) :ids)))
      (set! states (rest (rest states))))
    found))

; Which file a line put in is: one cut by its name, or one listed by it
; (a line copied), or none.
(defn oil-resolve (text ids)
  (let ((line (oil-parse text)))
    (when line
      (or (oil-uncut (get line :name))
          (oil-find (get line :name) ids)
          (oil-anywhere (get line :name))))))

; The ids of the lines that took the place of others. A line that has the
; name of one of the files it took the place of is that file; one that
; has the name of a file cut is that file; the rest are the files in the
; same places, as long as there are any; and any lines left over are put
; in, or any files left over cut.
(defn oil-replaced (gone come ids)
  (let ((names (map (fn (id) (when id (get id :name))) gone))
        (named (map (fn (text)
                      (let ((line (oil-parse text)))
                        (when (and line (member? (get line :name) names))
                          (oil-find (get line :name) gone))))
                    come))
        (taken (map (fn (id) (get id :path)) (filter (fn (id) (not (nil? id))) named)))
        (left (filter (fn (id) (not (and id (member? (get id :path) taken)))) gone))
        (out nil))
    (while come
      (let ((text (first come))
            (id (first named))
            (line (oil-parse (first come))))
        (cond (id nil)
              ((and line (oil-find (get line :name) oil-cut))
               (set! id (oil-uncut (get line :name))))
              (left
               (set! id (first left))
               (set! left (rest left)))
              (true (set! id (oil-resolve text ids))))
        (set! out (cons id out)))
      (set! come (rest come))
      (set! named (rest named)))
    (map (fn (id) (when id (set! oil-cut (cons id oil-cut)))) left)
    (reverse out)))

; Follow the text of the listing of `dir` from the lines last seen to
; these: the lines the same at the front and at the back are the files
; they were, and what is between took the place of what was.
(defn oil-track (dir text)
  (let ((state (oil-state dir)))
    (when state
      (let ((old (get state :lines))
            (ids (get state :ids))
            (new (lines text))
            (old-count (length old))
            (new-count (length new))
            (front (oil-same-count old new (min old-count new-count)))
            (back (oil-same-count (reverse old) (reverse new)
                                  (- (min old-count new-count) front)))
            (gone (oil-take (- old-count front back) (oil-drop front ids)))
            (come (oil-take (- new-count front back) (oil-drop front new))))
        (unless (and (nil? gone) (nil? come))
          (oil-put dir (get state :listed) new
                   (append (oil-take front ids)
                           (oil-replaced gone come ids)
                           (oil-drop (- old-count back) ids))))))))

(defn oil-sync ()
  (let ((dir (oil-here)))
    (when dir (oil-track dir (buffer-text)))))

; ---- what is on show

(defn oil-read (dir)
  (let ((entries (list-dir dir true)))
    (if oil-show-hidden
        entries
        (filter (fn (entry) (not (oil-hidden? entry))) entries))))

(defn oil-line-of (entries name)
  (+ 1 (or (find-index (fn (entry) (= (get entry :name) name)) entries) 0)))

; The directories in color, as the text is now.
(defn oil-decorate ()
  (let ((line 0) (marks nil))
    (map (fn (text)
           (set! line (+ line 1))
           (when (and (ends-with? (trim text) "/") (> (length (trim text)) 1))
             (set! marks (cons (list :start (list line 1)
                                     :end (list line (+ 1 (length text)))
                                     :color 'info)
                               marks))))
         (lines (buffer-text)))
    (decorate 'oil marks)))

; Read `dir` and put it up, the cursor on the line of `name`.
(defn oil-render (dir name)
  (let ((entries (oil-read dir))
        (shown (map oil-line entries)))
    (open-scratch (oil-tab-name dir) (join shown "\n") (fn (text) (oil-write dir text)))
    (oil-put dir entries shown entries)
    (set-buffer-keys '(oil))
    (oil-decorate)
    (set-cursor (oil-line-of entries name) 1)))

; Go to the listing of `dir`; one with changes not yet written is only
; gone back to.
(defn oil-show (dir name)
  (open-scratch (oil-tab-name dir))
  (unless (and (modified?) (oil-state dir))
    (oil-render dir name)))

; Read the disk again, the cursor staying on the name it is on.
(defn oil-refresh (dir)
  (oil-render dir (get (oil-at-cursor) :name)))

; Go elsewhere in the place of the listing up: `shown` puts up what is
; next, and `back` goes back to it. The listing is closed, unless it has
; changes; it is closed from its own tab, after what is next is up, so
; that a pane with nothing else in it stays.
(defn oil-replace (shown back)
  (oil-sync)
  (let ((old (scratch-name))
        (clean (not (modified?))))
    (shown)
    (when (and old clean (not (= (scratch-name) old)))
      (open-scratch old)
      (run "workspace.close_tab")
      (back))))

(defn oil-go-dir (dir name)
  (oil-replace (fn () (oil-show dir name))
               (fn () (open-scratch (oil-tab-name dir)))))

; Do `f` with the directory of each listing a pane shows, in its pane.
(defn oil-each-shown (f)
  (let ((index 0))
    (map (fn (pane)
           (let ((dir (oil-dir-of (get pane :scratch))))
             (when dir (with-pane index (fn () (f dir)))))
           (set! index (+ index 1)))
         (panes))))

(defn oil-refresh-shown ()
  (oil-each-shown (fn (dir) (unless (modified?) (oil-refresh dir)))))

; ---- what the edits ask for

; The listings up in tabs, each followed to the text it has now, those
; with changes not yet written first; the others are forgotten.
(defn oil-live ()
  (let ((changed nil) (states nil))
    (map (fn (tab)
           (let ((dir (oil-dir-of (get tab :name))))
             (when (and dir (oil-state dir))
               (oil-track dir (get tab :text))
               (set! states (append (list dir (oil-state dir)) states))
               (when (get tab :modified)
                 (set! changed (cons dir changed))))))
         (scratch-tabs))
    (set! oil-states states)
    (reverse changed)))

(defn oil-action (op from to entry-dir? label)
  (list :op op :from from :to to :dir entry-dir? :label label))

; What the listings of `dirs` ask to be done, as (:actions) or
; (:problem): every file listed in them is where its lines are, or
; deleted if there are none; a line of a file from another listing is a
; copy, unless the file's own line is gone from it, and the first such
; line is the file moved; a line of no file is a new one.
(defn oil-plan (dirs)
  (let ((listed (reduce (fn (paths dir)
                          (append paths (map (fn (entry) (get entry :path))
                                             (get (oil-state dir) :listed))))
                        nil dirs))
        (stays nil)       ; the paths of files with a line where they are
        (going nil)       ; (:entry :to :line) of the lines of files elsewhere
        (made nil)
        (problem nil))
    (map (fn (dir)
           (let ((state (oil-state dir))
                 (texts (get state :lines))
                 (ids (get state :ids)))
             (while texts
               (let ((line (oil-parse (first texts)))
                     (id (first ids)))
                 (when line
                   (let ((name (get line :name))
                         (to (oil-path dir name)))
                     (cond (problem nil)
                           ((or (= name "") (regex-match "(^|/)\\.\\.?$" name)
                                (contains? name "//") (starts-with? name "/"))
                            (set! problem (str (get line :text) " is no name for a file")))
                           ((and id (= (get id :path) to))
                            (if (member? to stays)
                                (set! problem (str "two lines say " (oil-say to false)))
                                (set! stays (cons to stays))))
                           (id (set! going (cons (list :entry id :to to :line line) going)))
                           (true
                            (set! made (cons (oil-action (if (get line :dir) 'mkdir 'touch)
                                                         nil to (get line :dir)
                                                         (oil-say to (get line :dir)))
                                             made)))))))
               (set! texts (rest texts))
               (set! ids (rest ids)))))
         dirs)
    (let ((moved nil)
          (moves nil)
          (copies nil)
          (deletes nil))
      (map (fn (item)
             (let ((entry (get item :entry))
                   (from (get entry :path))
                   (to (get item :to))
                   (label (str (oil-say from (get entry :dir)) " → "
                               (oil-say to (get (get item :line) :dir)))))
               (if (or (member? from stays) (member? from moved) (not (member? from listed)))
                   (set! copies (cons (oil-action 'copy from to (get entry :dir) label) copies))
                   (do (set! moved (cons from moved))
                       (set! moves (cons (oil-action 'move from to (get entry :dir) label) moves))))))
           (reverse going))
      (map (fn (dir)
             (map (fn (entry)
                    (let ((path (get entry :path)))
                      (unless (or (member? path stays) (member? path moved))
                        (set! deletes (cons (oil-action 'delete path nil (get entry :dir)
                                                        (oil-say path (get entry :dir)))
                                            deletes)))))
                  (get (oil-state dir) :listed)))
           dirs)
      (let ((actions (append (reverse deletes) (reverse made) (reverse copies) (reverse moves)))
            (freed (map (fn (action) (get action :from)) deletes))
            (targets stays))
        (map (fn (action)
               (let ((to (get action :to))
                     (from (get action :from)))
                 (cond (problem nil)
                       ((nil? to) nil)
                       ((member? to targets)
                        (set! problem (str "two lines say " (oil-say to false))))
                       ((and (= (get action :op) 'copy) (not (file-exists? from)))
                        (set! problem (str (oil-say from false) " is not there any more")))
                       ; On a disk that does not tell a from A, a.txt is
                       ; there as A.txt; rename-file knows it from another.
                       ((and (file-exists? to)
                             (not (member? to freed))
                             (not (and from (= (get action :op) 'move)
                                       (= (lowercase from) (lowercase to)))))
                        (set! problem (str (oil-say to false) " is there already")))
                       (true (set! targets (cons to targets))))))
             actions)
        (if problem (list :problem problem) (list :actions actions))))))

; ---- asking

(def oil-asking nil)   ; (:title :actions :then): what the popup asks

(defn oil-ask (title actions then)
  (set! oil-asking (list :title title :actions actions :then then))
  (focus-panel 'oil-confirm))

(defn oil-verb (action)
  (let ((op (get action :op)))
    (cond ((= op 'delete) (if oil-delete-forever "DELETE" "TRASH"))
          ((= op 'move) "MOVE")
          ((= op 'copy) "COPY")
          (true "CREATE"))))

(defn oil-verb-color (action)
  (let ((op (get action :op)))
    (cond ((= op 'delete) 'danger)
          ((= op 'move) 'warning)
          ((= op 'copy) 'info)
          (true 'success))))

(def oil-ask-rows 12)   ; how many of the actions the popup lists

(defpanel 'oil-confirm :at 'popup :width 560
  (fn (panel)
    (let ((actions (get oil-asking :actions)))
      (col :width 'full :gap 4 :padding-x 12 :padding-y 10 :mono true :size 12
        (text (get oil-asking :title "") :bold true)
        (map (fn (action)
               (row :gap 8 :align 'center :nowrap true
                 (row :width 56 :shrink false
                   (text (oil-verb action) :bold true :color (oil-verb-color action)))
                 (text (get action :label))))
             (oil-take oil-ask-rows actions))
        (when (> (length actions) oil-ask-rows)
          (text (str "and " (- (length actions) oil-ask-rows) " more") :color 'muted-foreground))
        (text "y yes  ·  n no" :color 'muted-foreground)))))

(defcommand "oil.yes" "Do what the popup of the listings asks"
  (fn ()
    (let ((then (get oil-asking :then)))
      (set! oil-asking nil)
      (hide-panel 'oil-confirm)
      (when then (then)))))

(defcommand "oil.no" "Do nothing of what the popup of the listings asks"
  (fn ()
    (set! oil-asking nil)
    (hide-panel 'oil-confirm)
    (message "Nothing done")))

; ---- writing

(defn oil-do (action)
  (let ((op (get action :op))
        (from (get action :from))
        (to (get action :to)))
    (cond ((= op 'delete) (delete-file from oil-delete-forever))
          ((= op 'mkdir) (make-dir to))
          ((= op 'move) (or (make-dir (path-parent to)) (rename-file from to)))
          ((= op 'copy) (or (make-dir (path-parent to)) (copy-file from to)))
          (true (or (make-dir (path-parent to)) (make-file to))))))

(defn oil-count (count what)
  (when (> count 0) (str count " " what)))

; The listings of `dirs` as the disk is now: those that are up read
; again, and the others known by their names to be the files on disk,
; to be read again once they are gone back to.
(defn oil-after (dirs)
  (set! oil-cut nil)
  ; Listed are only the files a line is: one that is on disk and in no
  ; line (made elsewhere, or shown by g . since) is not to be deleted.
  (map (fn (dir)
         (let ((state (oil-state dir))
               (fresh (oil-read dir))
               (ids (map (fn (text)
                           (let ((line (oil-parse text)))
                             (when line (oil-find (get line :name) fresh))))
                         (get state :lines))))
           (oil-put dir (filter (fn (id) (not (nil? id))) ids) (get state :lines) ids)))
       dirs)
  (oil-each-shown (fn (dir) (when (member? dir dirs) (oil-refresh dir))))
  (let ((here (oil-here)))
    (when (member? here dirs) (oil-refresh here)))
  (run "workspace.sync"))

; Do the actions, in the order they are in, and say how it went.
(defn oil-apply (dirs actions)
  (let ((made 0) (moved 0) (copied 0) (gone 0) (problem nil))
    (map (fn (action)
           (unless problem
             (let ((wrong (oil-do action))
                   (op (get action :op)))
               (cond (wrong (set! problem wrong))
                     ((= op 'delete) (set! gone (+ gone 1)))
                     ((= op 'move) (set! moved (+ moved 1)))
                     ((= op 'copy) (set! copied (+ copied 1)))
                     (true (set! made (+ made 1)))))))
         actions)
    (oil-after dirs)
    (let ((done (join (filter (fn (said) (not (nil? said)))
                              (list (oil-count made "made") (oil-count moved "moved")
                                    (oil-count copied "copied")
                                    (oil-count gone (if oil-delete-forever "deleted" "to the trash"))))
                      ", ")))
      (message (if problem
                   (str problem (if (= done "") "" (str " (" done ")")))
                   done)))))

; :w in a listing: every listing with changes is written.
(defn oil-write (dir text)
  (oil-track dir text)
  (let ((changed (oil-live))
        (dirs (if (member? dir changed) changed (cons dir changed)))
        (plan (oil-plan dirs))
        (actions (get plan :actions)))
    (cond ((get plan :problem) (message (get plan :problem)))
          ((nil? actions)
           (oil-after dirs)
           (message "Nothing to change"))
          (true
           (oil-ask (if (= (length dirs) 1)
                        (str "In " (oil-say dir true) ":")
                        (str "In " (length dirs) " listings:"))
                    actions
                    (fn () (oil-apply dirs actions)))))))

; ---- commands

; A command of a listing's, given its directory.
(defn oil-command (name description action)
  (defcommand name description
    (fn ()
      (let ((dir (oil-here)))
        (if dir
            (action dir)
            (message "Not a listing: oil.open makes one"))))))

(defcommand "oil.open" "List the directory of the file, as oil.nvim; in a listing, the one above"
  (fn ()
    (let ((here (oil-here))
          (file (current-file)))
      (cond (here (run "oil.parent"))
            (file
              (set! oil-from file)
              (oil-show (path-parent file) (path-name file)))
            ((project-root) (oil-show (project-root) nil))
            (true (message "No directory to list"))))))

(defcommand "oil.toggle" "List the directory of the file, or close the listing"
  (fn () (run (if (oil-here) "oil.close" "oil.open"))))

(oil-command "oil.select" "Open the file on the line, or list the directory"
  (fn (dir)
    (let ((line (oil-at-cursor)))
      (when line
        (let ((path (oil-path dir (get line :name))))
          (cond ((dir? path) (oil-go-dir path nil))
                ((file-exists? path)
                 (oil-replace (fn () (open-file path)) (fn () (open-file path))))
                (true (message (get line :text) " is not on disk: :w makes it"))))))))

(oil-command "oil.parent" "List the directory above"
  (fn (dir)
    (let ((up (path-parent dir)))
      (if up
          (oil-go-dir up (path-name dir))
          (message dir " is the top")))))

(oil-command "oil.open_root" "List the project's root"
  (fn (dir)
    (if (project-root)
        (oil-go-dir (project-root) nil)
        (message "There is no project"))))

(oil-command "oil.refresh" "Read the directory again, giving up the changes"
  (fn (dir)
    (if (modified?)
        (oil-ask (str "Give up the changes to " (oil-say dir true) "?") nil
          (fn () (oil-refresh dir)))
        (oil-refresh dir))))

(oil-command "oil.toggle_hidden" "Show the hidden files too, or not"
  (fn (dir)
    (set! oil-show-hidden (not oil-show-hidden))
    (if (modified?)
        (message "Hidden files " (if oil-show-hidden "shown" "hidden")
                 " from the next read: :w or C-l first")
        (do (oil-refresh dir)
            (message (if oil-show-hidden "Hidden files shown" "Hidden files hidden"))))))

(oil-command "oil.open_external" "Open the file on the line with the system's program for it"
  (fn (dir)
    (let ((line (oil-at-cursor)))
      (when line
        (let ((path (oil-path dir (get line :name))))
          (spawn "open" (list path) dir
            (fn (done problem)
              (when problem
                (spawn "xdg-open" (list path) dir
                  (fn (done problem) (when problem (message problem))))))))))))

(oil-command "oil.close" "Close the listing, back to the file it was opened from"
  (fn (dir)
    (let ((tab (scratch-name)))
      (run "workspace.close_tab")
      (when (and oil-from (not (= (scratch-name) tab)) (file-exists? oil-from))
        (open-file oil-from)))))

; ---- keeping up

; Each change to a listing is followed, to know its lines' files by, and
; the directories stay in color.
(add-hook 'buffer-changed
  (fn ()
    (when (oil-here)
      (oil-sync)
      (oil-decorate))))

; A listing gone back to is read again, if it has no changes; one whose
; changes were all written along with another's is too.
(add-hook 'buffer-entered
  (fn (path)
    (let ((dir (oil-here)))
      (when (and dir (oil-state dir))
        (oil-sync)
        (when (or (not (modified?))
                  (let ((plan (oil-plan (list dir))))
                    (and (nil? (get plan :problem)) (nil? (get plan :actions)))))
          (oil-refresh dir))))))

(add-hook 'files-changed oil-refresh-shown)

; ---- keys

(defn oil-bind (table pairs)
  (while pairs
    (bind table (first pairs) (nth 1 pairs))
    (set! pairs (rest (rest pairs)))))

(oil-bind 'oil
  '("<CR>" "oil.select"  "-" "oil.parent"  "_" "oil.open_root"
    "g." "oil.toggle_hidden"  "<C-l>" "oil.refresh"  "gx" "oil.open_external"
    "<C-c>" "oil.close"))

(oil-bind 'oil-confirm
  '("y" "oil.yes"  "<CR>" "oil.yes"  "n" "oil.no"  "q" "oil.no"))

(bind 'normal "<Space>-" "oil.open")
(bind 'normal "<Space>e" "oil.toggle")
(bind 'app "cmd-shift-e" "oil.toggle")
