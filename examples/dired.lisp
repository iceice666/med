; A directory as text, to rename its files by editing their names (as
; Emacs's wdired).
;
;   (load "dired.lisp")
;
; space - lists the directory of the file being edited in a tab of its
; own, one name to a line, a directory's with / after it; in a listing it
; goes to the directory above. gf on a line opens its file or directory.
;
; Change a name and :w renames the file; a name with / in it moves it,
; into directories that are made if there are none. An empty line puts
; its file in the trash, which takes :w! to do. Nothing is done unless
; all of it can be: the lines must stay as many as the files, and no
; name may be one that is already there, another line's included, so
; that two files change names in two goes, by way of a third.
;
; There are no keys of a tab's own, so space - and gf are the editor's
; everywhere; away from a listing, gf only says so.

; A listing's tab is named after its directory, with a / after it.
(defn dired-tab-name (dir)
  (if (ends-with? dir "/") dir (str dir "/")))

; The directory of the listing being edited, or nil.
(defn dired-here ()
  (let ((name (scratch-name)))
    (when (and name (starts-with? name "/") (ends-with? name "/") (dir? name))
      (if (= name "/") name (dired-bare name)))))

; A line's name, without the / that says it is a directory.
(defn dired-bare (text)
  (let ((text (trim text)))
    (if (and (ends-with? text "/") (> (length (split text "/")) 1))
        (dired-bare (join (reverse (rest (reverse (split text "/")))) "/"))
        text)))

(defn dired-line (entry)
  (str (get entry :name) (if (get entry :dir) "/" "")))

; The lines as typed, without the empty ones at the end beyond the files.
(defn dired-typed (text count)
  (let ((typed (lines text)))
    (while (and (> (length typed) count) (= (trim (last typed)) ""))
      (set! typed (reverse (rest (reverse typed)))))
    typed))

; ---- the listing

; List `dir`, with the cursor on the line of `name` if it is there. A
; listing with changes not yet written is only gone back to, unless
; `again`, after they are.
(defn dired-show (dir name again)
  (open-scratch (dired-tab-name dir))
  (when (or again (not (modified?)))
    (let ((entries (list-dir dir true)))
      (open-scratch (dired-tab-name dir)
                    (join (map dired-line entries) "\n")
                    (fn (text) (dired-write dir entries text)))
      (let ((index (find-index (fn (entry) (= (get entry :name) name)) entries)))
        (set-cursor (+ 1 (or index 0)) 1)))))

; ---- writing it

; What the lines ask for, each (:from name :to name), :to "" to delete.
; A line left as it was asks for nothing, whatever spaces the name has.
(defn dired-changes (entries typed)
  (cond ((nil? entries) nil)
        (true
          (let ((entry (car entries))
                (line (car typed))
                (from (get entry :name))
                (to (dired-bare line))
                (rest-of (dired-changes (cdr entries) (cdr typed))))
            (if (or (= line (dired-line entry)) (= from to))
                rest-of
                (cons (list :from from :to to) rest-of))))))

; What is wrong with doing the changes, or nil.
(defn dired-problem (dir entries changes)
  (let ((names (map (fn (entry) (get entry :name)) entries))
        (targets (map (fn (change) (get change :to))
                      (filter (fn (change) (not (= (get change :to) ""))) changes)))
        (problem nil))
    (map (fn (change)
           (let ((from (get change :from))
                 (to (get change :to)))
             (cond (problem nil)
                   ((= to "") nil)
                   ((> (length (filter (fn (other) (= other to)) targets)) 1)
                    (set! problem (str "two lines say " to)))
                   ((member? to names)
                    (set! problem (str to " is another line's: rename it first, then this")))
                   ((or (= to ".") (= to ".."))
                    (set! problem (str to " is no name for a file")))
                   ; On a disk that does not tell a from A, a.txt is
                   ; there as A.txt; rename-file knows it from another.
                   ((and (file-exists? (path-join dir to))
                         (not (= (lowercase to) (lowercase from))))
                    (set! problem (str to " is there already"))))))
         changes)
    problem))

(defn dired-apply (dir changes)
  (let ((renamed 0) (trashed 0) (problem nil))
    (map (fn (change)
           (unless problem
             (let ((from (path-join dir (get change :from)))
                   (to (get change :to)))
               (if (= to "")
                   (let ((wrong (delete-file from)))
                     (if wrong (set! problem wrong) (set! trashed (+ trashed 1))))
                   (let ((to (path-join dir to))
                         (wrong (make-dir (path-parent to)))
                         (wrong (or wrong (rename-file from to))))
                     (if wrong (set! problem wrong) (set! renamed (+ renamed 1))))))))
         changes)
    (let ((done (str renamed " renamed, " trashed " to the trash")))
      (if problem (str problem " (" done ")") done))))

; :w in a listing, with what is written there.
(defn dired-write (dir entries text)
  (let ((typed (dired-typed text (length entries))))
    (if (not (= (length typed) (length entries)))
        (message (length typed) " lines for " (length entries)
                 " files: change names in place, and empty a line to delete")
        (let ((changes (dired-changes entries typed))
              (trash (length (filter (fn (change) (= (get change :to) "")) changes)))
              (problem (dired-problem dir entries changes)))
          (cond (problem (message problem))
                ((and (> trash 0) (not command-bang))
                 (message trash " to the trash: :w! to do it"))
                (true
                  (let ((said (dired-apply dir changes)))
                    ; Gone, after :wq; still up, it shows the disk, the
                    ; cursor on the name it was on.
                    (when (= (scratch-name) (dired-tab-name dir))
                      (dired-show dir (dired-bare (or (buffer-line (car (cursor))) "")) true))
                    (run "workspace.sync")
                    (message said))))))))

; ---- commands

(defcommand "dired.open" "List the directory of the file; in a listing, the one above"
  (fn ()
    (let ((here (dired-here)))
      (cond (here
              (let ((up (path-parent here)))
                (if up (dired-show up (path-name here) false) (message here " is the top"))))
            ((current-file)
             (dired-show (path-parent (current-file)) (path-name (current-file)) false))
            ((project-root) (dired-show (project-root) nil false))
            (true (message "No directory to list"))))))

(defcommand "dired.enter" "Open the file or directory on the line of a listing"
  (fn ()
    (let ((here (dired-here)))
      (if (not here)
          (message "Not a listing: space - makes one")
          (let ((name (dired-bare (or (buffer-line (car (cursor))) ""))))
            (cond ((= name "") nil)
                  ((not (file-exists? (path-join here name)))
                   (message name " is not there: :w first"))
                  ((dir? (path-join here name)) (dired-show (path-join here name) nil false))
                  (true (open-file (path-join here name)))))))))

(bind 'normal "<Space>-" "dired.open")
(bind 'normal "gf" "dired.enter")
