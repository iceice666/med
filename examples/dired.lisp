; Dired: a directory as text, as in Emacs, with wdired to rename files
; by editing their names.
;
;   (load "dired.lisp")
;
; space - lists the directory of the file being edited, in a tab of its
; own: a name to a line, a directory's with / after it, and after each
; name what `ls -l` says of it. The listing's keys are those of Emacs's
; dired, in the table `dired`; j k, / and the other motions stay vim's,
; and the text is not to be edited until i.
;
;   enter a o   open the file or directory on the line
;   - ^         the directory above        > <   the next, the last directory
;   g r         read the disk again        s     by name, or newest first
;   (           the details, or not        q     close the listing
;
; Marks, beside the line numbers: m marks a file (*), d flags it for
; deletion (D), u takes either off, backspace the one above's, U all of
; them, and t turns the marks around. A count before m, d or u is lines.
;   * *  * /  * @   mark the executables, the directories, the links
;   * .             the names with an extension
;   % m  % d  * %   mark, or flag, the names a regular expression matches
;   ~  #            flag backups, and autosaves
;
; What is done is done to the marked files, or else to the one on the
; line:
;   C R       copy, or rename and move: to a name, or into a directory;
;             with `dired-dwim-target`, the directory offered is that of
;             the listing in the next pane, if one shows another
;   S H       a symbolic link to each, or a hard one
;   % C  % R  copy or rename by a regular expression and its replacement
;   D         delete; x deletes the flagged ones: to the trash, unless
;             `dired-delete-forever`
;   M O       chmod (755, or u+x) and chown (user, user:group or :group)
;   T Z       touch; compress, or uncompress
;   + =       make a directory; diff with another file
;   ! & X     a shell command on the files: * in it alone stands for all
;             of them, ? for each in turn, and without either they go
;             after it
;
; i, or C-x C-q, is wdired: vim's keys are back, and :w, C-x C-s or
; C-x C-q renames what was changed; a name with / in it moves the file, into
; directories that are made if need be. An empty line puts its file in
; the trash, which takes :w!. Nothing is done unless all of it can be:
; the lines must stay as many as the files, and no name may be one that
; is there already, another line's included, so that two files trade
; names in two goes. C-c C-k gives the changes up.

(def dired-delete-forever false)   ; D and x: true to skip the trash
(def dired-dwim-target false)      ; C R S H: true to offer the next pane's listing

(defkeys 'dired)
(defkeys 'wdired)

; ---- what is kept of each listing

; By directory, (:entries :marks :editing :sort :details :shown): the
; files as file-info has them, in the order of the lines; the marks,
; ((name mark)...); whether the names are being edited; 'name or 'time;
; whether the details are on show; and the text as it was put up, to
; tell an edit of it by.
(def dired-states nil)

(defn dired-assoc (pairs key value)
  (cond ((nil? pairs) (list key value))
        ((= (first pairs) key) (cons key (cons value (rest (rest pairs)))))
        (true (cons (first pairs)
                    (cons (nth 1 pairs) (dired-assoc (rest (rest pairs)) key value))))))

(defn dired-state (dir)
  (or (get dired-states dir)
      (list :entries nil :marks nil :editing false :sort 'name :details true :shown "")))

(defn dired-get (dir key)
  (get (dired-state dir) key))

(defn dired-put (dir key value)
  (set! dired-states
        (dired-assoc dired-states dir (dired-assoc (dired-state dir) key value))))

(defn dired-known? (dir)
  (not (nil? (get dired-states dir))))

(defn dired-editing? (dir)
  (dired-get dir :editing))

; ---- names and lines

; A listing's tab is named after its directory, with a / after it.
(defn dired-tab-name (dir)
  (if (ends-with? dir "/") dir (str dir "/")))

; The directory of a listing, by the name of its tab; nil for another tab.
(defn dired-dir-of (name)
  (when (and name (starts-with? name "/") (ends-with? name "/") (dir? name))
    (if (= name "/") name (dired-bare name))))

; The directory of the listing being edited, or nil.
(defn dired-here ()
  (dired-dir-of (scratch-name)))

; The directories listed in the other panes, from the pane after this one
; round.
(defn dired-shown-elsewhere ()
  (let ((before nil) (after nil) (past false))
    (map (fn (pane)
           (let ((dir (dired-dir-of (get pane :scratch))))
             (cond ((get pane :focused) (set! past true))
                   ((nil? dir) nil)
                   (past (set! after (cons dir after)))
                   (true (set! before (cons dir before))))))
         (panes))
    (append (reverse after) (reverse before))))

; Where C, R, S and H offer to put the files of `dir`, with
; `dired-dwim-target`, as Emacs's: the first other directory listed in
; another pane, or nil.
(defn dired-dwim-dir (dir)
  (when dired-dwim-target
    (first (filter (fn (other) (not (= other dir))) (dired-shown-elsewhere)))))

; A line's name, without the / that says it is a directory.
(defn dired-bare (text)
  (let ((text (trim text)))
    (if (and (ends-with? text "/") (> (length (split text "/")) 1))
        (dired-bare (join (reverse (rest (reverse (split text "/")))) "/"))
        text)))

(defn dired-line (entry)
  (str (get entry :name) (if (get entry :dir) "/" "")))

(defn dired-names (entries)
  (map (fn (entry) (get entry :name)) entries))

(defn dired-say-files (entries)
  (if (= (length entries) 1)
      (get (first entries) :name)
      (str (length entries) " files")))

; A path typed for a listing: from the top, from home, or from the
; listing's directory.
(defn dired-expand (dir text)
  (let ((text (trim text)))
    (cond ((starts-with? text "/") text)
          ((and dired-home (= text "~")) dired-home)
          ((and dired-home (starts-with? text "~/")) (str dired-home (regex-replace "^~" text "")))
          (true (path-join dir text)))))

; ---- what the system says

(def dired-home nil)          ; $HOME, for ~
(def dired-utc-offset 0)      ; how far local time is from UTC, in seconds
(def dired-env-asked false)
(def dired-users nil)         ; ("uid" "name"...), as ls tells them
(def dired-groups nil)        ; ("gid" "name"...)
(def dired-owners-asked nil)  ; the directories ls was asked about

(def dired-digits '("0" "1" "2" "3" "4" "5" "6" "7" "8" "9"))

(defn dired-number (text)
  (let ((number 0)
        (digit (regex-match "^(\\d)(.*)$" text)))
    (while digit
      (set! number (+ (* number 10) (find-index (fn (d) (= d (nth 1 digit))) dired-digits)))
      (set! digit (regex-match "^(\\d)(.*)$" (nth 2 digit))))
    number))

; date +%z's +0800 as seconds.
(defn dired-offset (text)
  (let ((parts (regex-match "^([+-])(\\d\\d)(\\d\\d)$" (trim (or text "")))))
    (if parts
        (* (if (= (nth 1 parts) "-") -1 1)
           (+ (* 3600 (dired-number (nth 2 parts))) (* 60 (dired-number (nth 3 parts)))))
        0)))

(defn dired-redecorate ()
  (let ((dir (dired-here)))
    (when (and dir (dired-known? dir))
      (dired-decorate dir))))

; Home and the time zone, asked once.
(defn dired-learn-env ()
  (unless dired-env-asked
    (set! dired-env-asked true)
    (spawn "sh" (list "-c" "printf '%s\\n%s\\n' \"$HOME\" \"$(date +%z)\"") "/"
      (fn (done problem)
        (when done
          (let ((said (lines (get done :out))))
            (set! dired-home (first said))
            (set! dired-utc-offset (dired-offset (nth 1 said)))
            (dired-redecorate)))))))

(defn dired-fields (line)
  (filter (fn (field) (not (= field ""))) (split line " ")))

(defn dired-ls-lines (text)
  (filter (fn (line) (and (not (starts-with? line "total"))
                          (> (length (dired-fields line)) 4)))
          (lines text)))

; From ls -lan and ls -la of one directory, line by line, the names of
; the owners and groups their numbers stand for.
(defn dired-learn-from (out)
  (let ((halves (split out "::med::\n"))
        (numbers (dired-ls-lines (first halves)))
        (names (dired-ls-lines (or (nth 1 halves) ""))))
    (while (and numbers names)
      (let ((number (dired-fields (first numbers)))
            (name (dired-fields (first names))))
        (when (nil? (get dired-users (nth 2 number)))
          (set! dired-users (append (list (nth 2 number) (nth 2 name)) dired-users)))
        (when (nil? (get dired-groups (nth 3 number)))
          (set! dired-groups (append (list (nth 3 number) (nth 3 name)) dired-groups))))
      (set! numbers (rest numbers))
      (set! names (rest names)))))

(defn dired-unknown-owner? (entry)
  (or (and (get entry :uid) (nil? (get dired-users (str (get entry :uid)))))
      (and (get entry :gid) (nil? (get dired-groups (str (get entry :gid)))))))

(defn dired-learn-owners (dir entries)
  (when (and (find-index dired-unknown-owner? entries)
             (not (member? dir dired-owners-asked)))
    (set! dired-owners-asked (cons dir dired-owners-asked))
    (spawn "sh" (list "-c" "ls -lan \"$1\"; echo ::med::; ls -la \"$1\"" "sh" dir) dir
      (fn (done problem)
        (when done
          (dired-learn-from (get done :out))
          (when (and (= (dired-here) dir) (not (dired-editing? dir)))
            (dired-decorate dir)))))))

; ---- reading the disk

(defn dired-merge (a b before?)
  (let ((out nil))
    (while (and a b)
      (if (before? (first b) (first a))
          (do (set! out (cons (first b) out)) (set! b (rest b)))
          (do (set! out (cons (first a) out)) (set! a (rest a)))))
    (append (reverse out) a b)))

; The items in order, the ones the same keeping theirs.
(defn dired-sort (items before?)
  (if (< (length items) 2)
      items
      (let ((half (/ (length items) 2))
            (front nil)
            (back items))
        (while (> half 0)
          (set! front (cons (first back) front))
          (set! back (rest back))
          (set! half (- half 1)))
        (dired-merge (dired-sort (reverse front) before?) (dired-sort back before?) before?))))

(defn dired-newer? (a b)
  (> (or (get a :modified) 0) (or (get b :modified) 0)))

; What is in `dir` now, the marks kept on the names still there.
(defn dired-read (dir)
  (let ((entries (filter (fn (entry) (not (nil? entry)))
                         (map (fn (entry) (file-info (get entry :path))) (list-dir dir true))))
        (entries (if (= (dired-get dir :sort) 'time) (dired-sort entries dired-newer?) entries))
        (names (dired-names entries)))
    (dired-put dir :entries entries)
    (dired-put dir :marks (filter (fn (mark) (member? (first mark) names)) (dired-get dir :marks)))
    (dired-learn-owners dir entries)))

; ---- how it looks

(defn dired-spaces (count)
  (let ((out ""))
    (while (> count 0)
      (set! out (str out " "))
      (set! count (- count 1)))
    out))

(defn dired-pad-left (text width)
  (str (dired-spaces (- width (length text))) text))

(defn dired-pad-right (text width)
  (str text (dired-spaces (- width (length text)))))

(defn dired-widest (texts)
  (reduce (fn (widest text) (max widest (length text))) 0 texts))

(defn dired-two (number)
  (if (< number 10) (str "0" number) (str number)))

(defn dired-bit? (mode bit)
  (= (mod (/ mode bit) 2) 1))

(defn dired-rwx (bits special on off)
  (let ((bits (mod bits 8))
        (x (dired-bit? bits 1)))
    (str (if (dired-bit? bits 4) "r" "-")
         (if (dired-bit? bits 2) "w" "-")
         (cond (special (if x on off))
               (x "x")
               (true "-")))))

; -rwxr-xr-x, as ls has it.
(defn dired-mode-string (entry)
  (let ((mode (get entry :mode))
        (kind (get entry :kind)))
    (str (cond ((= kind 'dir) "d") ((= kind 'link) "l") ((= kind 'file) "-") (true "?"))
         (if (nil? mode)
             "?????????"
             (str (dired-rwx (/ mode 64) (dired-bit? mode 2048) "s" "S")
                  (dired-rwx (/ mode 8) (dired-bit? mode 1024) "s" "S")
                  (dired-rwx mode (dired-bit? mode 512) "t" "T"))))))

(defn dired-executable? (entry)
  (let ((mode (or (get entry :mode) 0)))
    (and (= (get entry :kind) 'file)
         (or (dired-bit? mode 64) (dired-bit? mode 8) (dired-bit? mode 1)))))

; 934, 1.2K, 34M, as ls -h has it.
(defn dired-size (bytes)
  (if (< bytes 1024)
      (str bytes)
      (let ((units '("K" "M" "G" "T" "P"))
            (scale 1024))
        (while (and (rest units) (>= bytes (* scale 1024)))
          (set! scale (* scale 1024))
          (set! units (rest units)))
        (let ((tenths (/ (+ (* bytes 10) (/ scale 2)) scale)))
          (if (< tenths 100)
              (str (/ tenths 10) "." (mod tenths 10) (first units))
              (str (/ (+ tenths 5) 10) (first units)))))))

; 2026-11-15 10:22, in local time.
(defn dired-date (seconds)
  (if (nil? seconds)
      "????-??-?? ??:??"
      (let ((local (max 0 (+ seconds dired-utc-offset)))
            (days (/ local 86400))
            (in-day (mod local 86400))
            ; Days to a date, as Howard Hinnant's civil_from_days.
            (z (+ days 719468))
            (era (/ z 146097))
            (doe (- z (* era 146097)))
            (yoe (/ (- (+ (- doe (/ doe 1460)) (/ doe 36524)) (/ doe 146096)) 365))
            (doy (- doe (- (+ (* 365 yoe) (/ yoe 4)) (/ yoe 100))))
            (mp (/ (+ (* 5 doy) 2) 153))
            (day (+ (- doy (/ (+ (* 153 mp) 2) 5)) 1))
            (month (if (< mp 10) (+ mp 3) (- mp 9)))
            (year (+ yoe (* era 400) (if (<= month 2) 1 0))))
        (str year "-" (dired-two month) "-" (dired-two day) " "
             (dired-two (/ in-day 3600)) ":" (dired-two (mod (/ in-day 60) 60))))))

(defn dired-owner (known id)
  (if (nil? id) "?" (or (get known (str id)) (str id))))

; For each entry, what ls -l says of it, put out of the way of the names
; so that the columns line up.
(defn dired-details (entries)
  (let ((names (map (fn (entry) (length (dired-line entry))) entries))
        (name-width (min 40 (reduce max 0 names)))
        (links (map (fn (entry) (str (or (get entry :links) "?"))) entries))
        (owners (map (fn (entry) (dired-owner dired-users (get entry :uid))) entries))
        (groups (map (fn (entry) (dired-owner dired-groups (get entry :gid))) entries))
        (sizes (map (fn (entry) (dired-size (get entry :size))) entries))
        (widths (map dired-widest (list links owners groups sizes)))
        (out nil))
    (while entries
      (let ((entry (first entries)))
        (set! out (cons (str (dired-spaces (- name-width (first names)))
                             (dired-mode-string entry) " "
                             (dired-pad-left (first links) (nth 0 widths)) " "
                             (dired-pad-right (first owners) (nth 1 widths)) " "
                             (dired-pad-right (first groups) (nth 2 widths)) " "
                             (dired-pad-left (first sizes) (nth 3 widths)) " "
                             (dired-date (get entry :modified))
                             (if (get entry :target) (str " -> " (get entry :target)) ""))
                        out)))
      (set! entries (rest entries))
      (set! names (rest names))
      (set! links (rest links))
      (set! owners (rest owners))
      (set! groups (rest groups))
      (set! sizes (rest sizes)))
    (reverse out)))

(defn dired-mark-of (dir name)
  (let ((mark (first (filter (fn (mark) (= (first mark) name)) (dired-get dir :marks)))))
    (when mark (nth 1 mark))))

(defn dired-color (entry mark)
  (cond ((= mark "*") 'warning)
        ((= mark "D") 'danger)
        ((= (get entry :kind) 'link) 'link)
        ((= (get entry :kind) 'dir) 'info)
        ((dired-executable? entry) 'success)
        (true nil)))

; The marks beside the lines, the colors of the names, and the details
; after them: of the listing that is up.
(defn dired-decorate (dir)
  (let ((entries (dired-get dir :entries))
        (details (when (dired-get dir :details) (dired-details entries)))
        (line 0)
        (marks nil))
    (while entries
      (let ((entry (first entries))
            (mark (dired-mark-of dir (get entry :name)))
            (color (dired-color entry mark)))
        (set! line (+ line 1))
        (when mark
          (set! marks (cons (list :line line :sign mark :color color) marks)))
        (when color
          (set! marks (cons (list :start (list line 1)
                                  :end (list line (+ 1 (length (dired-line entry))))
                                  :color color)
                            marks)))
        (when details
          (set! marks (cons (list :line line :text (first details) :color 'muted-foreground) marks))
          (set! details (rest details))))
      (set! entries (rest entries)))
    (decorate 'dired marks)))

; ---- the listing

(defn dired-text (dir)
  (join (map dired-line (dired-get dir :entries)) "\n"))

; Put up what is kept of `dir`, the cursor on `line`.
(defn dired-render (dir line)
  (let ((text (dired-text dir)))
    (dired-put dir :shown text)
    (dired-put dir :editing false)
    (open-scratch (dired-tab-name dir) text (fn (text) (dired-write dir text)))
    (set-buffer-keys '(dired))
    (dired-decorate dir)
    (set-cursor (max 1 (min line (length (dired-get dir :entries)))) 1)))

(defn dired-line-of (dir name)
  (+ 1 (or (find-index (fn (entry) (= (get entry :name) name)) (dired-get dir :entries)) 0)))

; The entry on the cursor's line.
(defn dired-current (dir)
  (nth (- (car (cursor)) 1) (dired-get dir :entries)))

; List `dir`, with the cursor on the line of `name` if it is there. A
; listing whose names are being edited, with changes not yet written, is
; only gone back to.
(defn dired-show (dir name)
  (dired-learn-env)
  (open-scratch (dired-tab-name dir))
  (if (and (dired-editing? dir) (modified?))
      (set-buffer-keys '(wdired))
      (do (dired-read dir)
          (dired-render dir (dired-line-of dir name)))))

; Read the disk again, the cursor staying on its file.
(defn dired-refresh (dir)
  (let ((entry (dired-current dir))
        (line (car (cursor))))
    (dired-read dir)
    (dired-render dir (if entry
                          (let ((at (find-index (fn (other) (= (get other :name) (get entry :name)))
                                                (dired-get dir :entries))))
                            (if at (+ at 1) line))
                          line))))

; Read the disk again for every listing a pane shows, but one whose names
; are being edited: what was done may show in a listing beside this one,
; and after something done in the background the listing may be gone, or
; another tab up. In its pane, so that the keyboard stays where it is,
; in a panel or in another pane.
(defn dired-refresh-shown ()
  (let ((index 0))
    (map (fn (pane)
           (let ((dir (dired-dir-of (get pane :scratch))))
             (when (and dir (dired-known? dir) (not (dired-editing? dir)))
               (with-pane index (fn () (dired-refresh dir)))))
           (set! index (+ index 1)))
         (panes))))

; ---- asking

(def dired-asking nil)   ; (:label :then): what the prompt is for

(defn dired-ask (label initial then)
  (set! dired-asking (list :label label :then then))
  (set-input 'dired-prompt 'text initial)
  (focus-panel 'dired-prompt))

(defn dired-answer (text)
  (let ((then (get dired-asking :then)))
    (set! dired-asking nil)
    (hide-panel 'dired-prompt)
    (focus-editor)
    (when then (then text))))

(defn dired-confirm (question yes)
  (dired-ask (str question " (y or n)") ""
    (fn (text)
      (if (starts-with? (lowercase (trim text)) "y")
          (yes)
          (message "Nothing done")))))

(defpanel 'dired-prompt :at 'popup :width 640
  (fn (panel)
    (col :width 'full :gap 4 :padding-x 8 :padding-y 8 :mono true :size 12
      (text (get dired-asking :label "") :color 'success)
      (row :border 1 :border-color 'border :rounded 4 :padding-x 4
        (input 'text :grow true :on-submit dired-answer))
      (text "enter confirm  ·  esc cancel" :color 'muted-foreground))))

; ---- marks

(defn dired-set-mark (dir name mark)
  (let ((others (filter (fn (other) (not (= (first other) name))) (dired-get dir :marks))))
    (dired-put dir :marks (if mark (cons (list name mark) others) others))))

(defn dired-marked (dir mark)
  (filter (fn (entry) (= (dired-mark-of dir (get entry :name)) mark))
          (dired-get dir :entries)))

; The marked files, or else the one on the line.
(defn dired-targets (dir)
  (let ((marked (dired-marked dir "*")))
    (if marked
        marked
        (let ((entry (dired-current dir)))
          (when entry (list entry))))))

; Mark as many lines as the count from the cursor's down, and go past.
(defn dired-mark-lines (dir mark)
  (let ((times (or command-count 1))
        (line (car (cursor)))
        (entries (dired-get dir :entries)))
    (while (and (> times 0) (<= line (length entries)))
      (dired-set-mark dir (get (nth (- line 1) entries) :name) mark)
      (set! line (+ line 1))
      (set! times (- times 1)))
    (dired-decorate dir)
    (set-cursor (max 1 (min line (length entries))) 1)))

(defn dired-mark-where (dir mark holds)
  (let ((count 0))
    (map (fn (entry)
           (when (holds entry)
             (dired-set-mark dir (get entry :name) mark)
             (set! count (+ count 1))))
         (dired-get dir :entries))
    (dired-decorate dir)
    (message count (if (= mark "D") " flagged" " marked"))))

(defn dired-mark-matching (dir mark)
  (dired-ask (if (= mark "D") "Flag for deletion (regexp):" "Mark (regexp):") ""
    (fn (pattern)
      (dired-mark-where dir mark (fn (entry) (regex-match pattern (get entry :name)))))))

; ---- doing things

; Say how it went, with the listings that are up read again.
(defn dired-finish (dir what done problems)
  (dired-refresh-shown)
  (message what " " done
           (if problems (str ", failed " (length problems) ": " (first problems)) "")))

; Do `act` to each item, which gives nil or what went wrong.
(defn dired-each (dir what items act)
  (let ((done 0) (problems nil))
    (map (fn (item)
           (let ((wrong (act item)))
             (if wrong
                 (set! problems (cons wrong problems))
                 (set! done (+ done 1)))))
         items)
    (dired-finish dir what done problems)))

; Run the commands, each (program argument...), in `dir`, one after
; another.
(defn dired-run-each (dir what commands done problems)
  (if (nil? commands)
      (dired-finish dir what done problems)
      (let ((command (first commands)))
        (spawn (first command) (rest command) dir
          (fn (ran problem)
            (let ((wrong (cond (problem problem)
                               ((= (get ran :status) 0) nil)
                               (true (let ((said (trim (str (get ran :err) (get ran :out)))))
                                       (if (= said "")
                                           (str (first command) ": exit " (get ran :status))
                                           (first (lines said))))))))
              (dired-run-each dir what (rest commands)
                              (if wrong done (+ done 1))
                              (if wrong (cons wrong problems) problems))))))))

(defn dired-copy-each (dir pairs done problems)
  (if (nil? pairs)
      (dired-finish dir "copied" done problems)
      (copy-file (first (first pairs)) (nth 1 (first pairs))
        (fn (problem)
          (dired-copy-each dir (rest pairs)
                           (if problem done (+ done 1))
                           (if problem (cons problem problems) problems))))))

; Where the files go, for what was typed: ((from to)...), or a problem.
; Many files, or a name with / after it, go into a directory, made if it
; is not there; one file goes into a directory, or else to the name.
(defn dired-destinations (dir files text)
  (let ((typed (trim text))
        (to (dired-expand dir typed)))
    (cond ((= typed "") (list :problem "No place given"))
          ((or (dir? to) (ends-with? typed "/") (> (length files) 1))
           (let ((made (cond ((dir? to) nil)
                             ((ends-with? typed "/") (make-dir to))
                             (true (str to " is not a directory")))))
             (if made
                 (list :problem made)
                 (list :pairs (map (fn (entry) (list (get entry :path) (path-join to (get entry :name))))
                                   files)))))
          (true (list :pairs (list (list (get (first files) :path) to)))))))

; Ask where the files go, then `act` on the pairs.
(defn dired-to (dir verb preposition act)
  (let ((files (dired-targets dir)))
    (when files
      (dired-ask (str verb " " (dired-say-files files) " " preposition ":")
                 (dired-tab-name (or (dired-dwim-dir dir) dir))
        (fn (text)
          (let ((to (dired-destinations dir files text)))
            (if (get to :problem)
                (message (get to :problem))
                (act (get to :pairs)))))))))

; Ask for a regular expression and its replacement, then `act` on the
; pairs for the files whose names it matches.
(defn dired-by-regex (dir verb act)
  (let ((files (dired-targets dir)))
    (when files
      (dired-ask (str verb " from (regexp):") ""
        (fn (pattern)
          (dired-ask (str verb " " pattern " to:") ""
            (fn (replacement)
              (let ((matching (filter (fn (entry) (regex-match pattern (get entry :name))) files)))
                (if (nil? matching)
                    (message "No name matches " pattern)
                    (act (map (fn (entry)
                                (list (get entry :path)
                                      (path-join dir (regex-replace pattern (get entry :name) replacement))))
                              matching)))))))))))

(defn dired-rename-pairs (dir pairs)
  (dired-each dir "renamed" pairs (fn (pair) (rename-file (first pair) (nth 1 pair)))))

(defn dired-delete (dir files)
  (if (nil? files)
      (message "Nothing to delete")
      (dired-confirm (str (if dired-delete-forever "Delete " "Trash ") (dired-say-files files) "?")
        (fn ()
          (dired-each dir (if dired-delete-forever "deleted" "to the trash") files
            (fn (entry) (delete-file (get entry :path) dired-delete-forever)))))))

(defn dired-compress-command (entry)
  (let ((name (get entry :name)))
    (cond ((= (get entry :kind) 'dir) (list "tar" "-czf" (str name ".tar.gz") name))
          ((regex-match "\\.(tar\\.gz|tgz)$" name) (list "tar" "-xzf" name))
          ((ends-with? name ".gz") (list "gunzip" name))
          ((ends-with? name ".zip") (list "unzip" name))
          (true (list "gzip" name)))))

(defn dired-quote (name)
  (str "'" (replace name "'" "'\\''") "'"))

(def dired-alone-star "(^|\\s)\\*(\\s|$)")
(def dired-alone-mark "(^|\\s)\\?(\\s|$)")

(defn dired-put-in (command pattern names)
  (regex-replace pattern command (str "${1}" (replace names "$" "$$") "${2}")))

; The shell's command line for `command` on the files named.
(defn dired-shell-script (command names)
  (let ((all (join (map dired-quote names) " ")))
    (cond ((regex-match dired-alone-star command) (dired-put-in command dired-alone-star all))
          ((regex-match dired-alone-mark command)
           (join (map (fn (name) (dired-put-in command dired-alone-mark (dired-quote name))) names)
                 "; "))
          (true (str command " " all)))))

; What a program printed: nothing said, a line in the status bar, or
; more in a tab of its own.
(defn dired-show-output (title ran)
  (let ((out (str (get ran :out) (get ran :err)))
        (said (lines (trim out))))
    (cond ((= (trim out) "")
           (message title (if (= (get ran :status) 0) ": done" (str ": exit " (get ran :status)))))
          ((= (length said) 1) (message (first said)))
          (true (open-scratch (str "*" title "*") out)))))

(defn dired-shell (dir)
  (let ((files (dired-targets dir)))
    (when files
      (dired-ask (str "! on " (dired-say-files files) ":") ""
        (fn (command)
          (unless (= (trim command) "")
            (spawn "sh" (list "-c" (dired-shell-script (trim command) (dired-names files))) dir
              (fn (ran problem)
                (dired-refresh-shown)
                (if problem (message problem) (dired-show-output "shell" ran))))))))))

; ---- writing the names, as wdired

; The lines as typed, without the empty ones at the end beyond the files.
(defn dired-typed (text count)
  (let ((typed (lines text)))
    (while (and (> (length typed) count) (= (trim (last typed)) ""))
      (set! typed (reverse (rest (reverse typed)))))
    typed))

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
  (let ((names (dired-names entries))
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
                   (let ((mark (dired-mark-of dir (get change :from)))
                         (path (path-join dir to))
                         (wrong (make-dir (path-parent path)))
                         (wrong (or wrong (rename-file from path))))
                     (if wrong
                         (set! problem wrong)
                         (do (set! renamed (+ renamed 1))
                             ; The mark goes with the name.
                             (when mark
                               (dired-set-mark dir (get change :from) nil)
                               (dired-set-mark dir to mark)))))))))
         changes)
    (let ((done (str renamed " renamed, " trashed " to the trash")))
      (if problem (str problem " (" done ")") done))))

; :w in a listing, with what is written there.
(defn dired-write (dir text)
  (let ((entries (dired-get dir :entries))
        (typed (dired-typed text (length entries))))
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
                    (dired-put dir :editing false)
                    ; Gone, after :wq; still up, it shows the disk, the
                    ; cursor on the name it was on.
                    (when (= (scratch-name) (dired-tab-name dir))
                      (let ((name (dired-bare (or (buffer-line (car (cursor))) ""))))
                        (dired-read dir)
                        (dired-render dir (dired-line-of dir name))))
                    (run "workspace.sync")
                    (message said))))))))

; ---- commands

; A command of a listing's, given its directory; while the names are
; being edited, the keys are vim's and these wait.
(defn dired-command (name description action)
  (defcommand name description
    (fn ()
      (let ((dir (dired-here)))
        (cond ((or (not dir) (not (dired-known? dir)))
               (message "Not a listing: dired.open makes one"))
              ((dired-editing? dir)
               (message "Editing the names: :w renames, C-c C-k gives up"))
              (true (action dir)))))))

(defcommand "dired.open" "List the directory of the file; in a listing, the one above"
  (fn ()
    (let ((here (dired-here)))
      (cond (here
              (let ((up (path-parent here)))
                (if up (dired-show up (path-name here)) (message here " is the top"))))
            ((current-file)
             (dired-show (path-parent (current-file)) (path-name (current-file))))
            ((project-root) (dired-show (project-root) nil))
            (true (message "No directory to list"))))))

(dired-command "dired.enter" "Open the file or directory on the line"
  (fn (dir)
    (let ((entry (dired-current dir)))
      (cond ((nil? entry) nil)
            ((not (file-exists? (get entry :path)))
             (message (get entry :name) " is not there: g r reads the disk again"))
            ((get entry :dir) (dired-show (get entry :path) nil))
            (true (open-file (get entry :path)))))))

(dired-command "dired.refresh" "Read the directory again"
  (fn (dir) (dired-refresh dir)))

(dired-command "dired.sort" "List by name, or newest first"
  (fn (dir)
    (dired-put dir :sort (if (= (dired-get dir :sort) 'time) 'name 'time))
    (dired-refresh dir)
    (message (if (= (dired-get dir :sort) 'time) "Newest first" "By name"))))

(dired-command "dired.details" "Show what ls -l says of the files, or not"
  (fn (dir)
    (dired-put dir :details (not (dired-get dir :details)))
    (dired-decorate dir)))

(defn dired-next-dir (dir step)
  (let ((entries (dired-get dir :entries))
        (line (+ (car (cursor)) step)))
    (while (and (>= line 1) (<= line (length entries))
                (not (get (nth (- line 1) entries) :dir)))
      (set! line (+ line step)))
    (if (and (>= line 1) (<= line (length entries)))
        (set-cursor line 1)
        (message "No more directories"))))

(dired-command "dired.next_dir" "Go to the next directory"
  (fn (dir) (dired-next-dir dir 1)))
(dired-command "dired.prev_dir" "Go to the directory before"
  (fn (dir) (dired-next-dir dir -1)))

(dired-command "dired.read_only" "Say how to edit the names"
  (fn (dir) (message "The listing is not edited: i edits the names")))

; Marks.
(dired-command "dired.mark" "Mark the file"
  (fn (dir) (dired-mark-lines dir "*")))
(dired-command "dired.flag" "Flag the file for deletion"
  (fn (dir) (dired-mark-lines dir "D")))
(dired-command "dired.unmark" "Take the mark off the file"
  (fn (dir) (dired-mark-lines dir nil)))
(dired-command "dired.unmark_back" "Take the mark off the file above"
  (fn (dir)
    (let ((line (car (cursor))))
      (when (> line 1)
        (set-cursor (- line 1) 1)
        (dired-set-mark dir (get (dired-current dir) :name) nil)
        (dired-decorate dir)))))
(dired-command "dired.unmark_all" "Take every mark off"
  (fn (dir)
    (let ((count (length (dired-get dir :marks))))
      (dired-put dir :marks nil)
      (dired-decorate dir)
      (message count " unmarked"))))
(dired-command "dired.toggle_marks" "Mark what is not marked, and unmark what is"
  (fn (dir)
    (map (fn (entry)
           (let ((mark (dired-mark-of dir (get entry :name))))
             (cond ((= mark "*") (dired-set-mark dir (get entry :name) nil))
                   ((nil? mark) (dired-set-mark dir (get entry :name) "*")))))
         (dired-get dir :entries))
    (dired-decorate dir)))
(dired-command "dired.mark_executables" "Mark the executable files"
  (fn (dir) (dired-mark-where dir "*" dired-executable?)))
(dired-command "dired.mark_dirs" "Mark the directories"
  (fn (dir) (dired-mark-where dir "*" (fn (entry) (= (get entry :kind) 'dir)))))
(dired-command "dired.mark_links" "Mark the symbolic links"
  (fn (dir) (dired-mark-where dir "*" (fn (entry) (= (get entry :kind) 'link)))))
(dired-command "dired.mark_extension" "Mark the names with an extension"
  (fn (dir)
    (dired-ask "Mark extension:" ""
      (fn (text)
        (let ((ending (str "." (regex-replace "^\\." (trim text) ""))))
          (dired-mark-where dir "*" (fn (entry) (ends-with? (get entry :name) ending))))))))
(dired-command "dired.mark_regex" "Mark the names a regular expression matches"
  (fn (dir) (dired-mark-matching dir "*")))
(dired-command "dired.flag_regex" "Flag the names a regular expression matches"
  (fn (dir) (dired-mark-matching dir "D")))
(dired-command "dired.flag_backups" "Flag the backups, names ending in ~"
  (fn (dir) (dired-mark-where dir "D" (fn (entry) (ends-with? (get entry :name) "~")))))
(dired-command "dired.flag_autosaves" "Flag the autosaves, #names#"
  (fn (dir) (dired-mark-where dir "D" (fn (entry) (regex-match "^#.*#$" (get entry :name))))))

; Doing things.
(dired-command "dired.copy" "Copy the files"
  (fn (dir) (dired-to dir "Copy" "to" (fn (pairs) (dired-copy-each dir pairs 0 nil)))))
(dired-command "dired.rename" "Rename or move the files"
  (fn (dir) (dired-to dir "Rename" "to" (fn (pairs) (dired-rename-pairs dir pairs)))))
(dired-command "dired.symlink" "Make symbolic links to the files"
  (fn (dir)
    (dired-to dir "Symlink" "as"
      (fn (pairs) (dired-each dir "linked" pairs (fn (pair) (make-link (first pair) (nth 1 pair))))))))
(dired-command "dired.hardlink" "Make hard links to the files"
  (fn (dir)
    (dired-to dir "Hard link" "as"
      (fn (pairs) (dired-each dir "linked" pairs (fn (pair) (make-link (first pair) (nth 1 pair) true)))))))
(dired-command "dired.copy_regex" "Copy the files by a regular expression"
  (fn (dir) (dired-by-regex dir "Copy" (fn (pairs) (dired-copy-each dir pairs 0 nil)))))
(dired-command "dired.rename_regex" "Rename the files by a regular expression"
  (fn (dir) (dired-by-regex dir "Rename" (fn (pairs) (dired-rename-pairs dir pairs)))))
(dired-command "dired.delete" "Delete the files"
  (fn (dir) (dired-delete dir (dired-targets dir))))
(dired-command "dired.execute" "Delete the files flagged for deletion"
  (fn (dir) (dired-delete dir (dired-marked dir "D"))))
(dired-command "dired.mkdir" "Make a directory"
  (fn (dir)
    (dired-ask "Make directory:" (dired-tab-name dir)
      (fn (text)
        (let ((wrong (make-dir (dired-expand dir text))))
          (dired-finish dir "made" (if wrong 0 1) (when wrong (list wrong))))))))
(dired-command "dired.chmod" "Change the permissions of the files"
  (fn (dir)
    (let ((files (dired-targets dir)))
      (when files
        (dired-ask (str "Change mode of " (dired-say-files files)
                        (if (= (length files) 1) (str " (" (dired-mode-string (first files)) ")") "")
                        " to:")
                   ""
          (fn (text)
            (let ((mode (trim text)))
              (if (regex-match "^[0-7]{1,4}$" mode)
                  (dired-each dir "changed" files (fn (entry) (set-file-mode (get entry :path) mode)))
                  (dired-run-each dir "changed" (list (cons "chmod" (cons mode (dired-names files))))
                                  0 nil)))))))))
(dired-command "dired.chown" "Change the owner, or the group, of the files"
  (fn (dir)
    (let ((files (dired-targets dir)))
      (when files
        (dired-ask (str "Change owner of " (dired-say-files files) " to (user:group):") ""
          (fn (text)
            (dired-run-each dir "changed" (list (cons "chown" (cons (trim text) (dired-names files))))
                            0 nil)))))))
(dired-command "dired.touch" "Touch the files"
  (fn (dir)
    (let ((files (dired-targets dir)))
      (when files
        (dired-run-each dir "touched" (list (cons "touch" (dired-names files))) 0 nil)))))
(dired-command "dired.compress" "Compress the files, or uncompress them"
  (fn (dir)
    (let ((files (dired-targets dir)))
      (when files
        (dired-run-each dir "compressed" (map dired-compress-command files) 0 nil)))))
(dired-command "dired.shell" "Run a shell command on the files"
  (fn (dir) (dired-shell dir)))
(dired-command "dired.diff" "Diff the file with another"
  (fn (dir)
    (let ((entry (dired-current dir))
          (other (first (filter (fn (marked) (not (= (get marked :name) (get entry :name))))
                                (dired-marked dir "*")))))
      (when entry
        (dired-ask (str "Diff " (get entry :name) " with:") (if other (get other :name) "")
          (fn (text)
            (spawn "diff" (list "-u" (dired-expand dir text) (get entry :path)) dir
              (fn (ran problem)
                (if problem (message problem) (dired-show-output "diff" ran))))))))))
(dired-command "dired.quit" "Close the listing"
  (fn (dir) (run "workspace.close_tab")))

; wdired.
(dired-command "dired.edit" "Edit the names, as wdired"
  (fn (dir)
    (dired-put dir :editing true)
    (set-buffer-keys '(wdired))
    (message "Editing the names: :w or C-x C-s renames, C-c C-k gives up")))

(defn wdired-command (name description action)
  (defcommand name description
    (fn ()
      (let ((dir (dired-here)))
        (if (and dir (dired-editing? dir))
            (action dir)
            (message "Not editing the names of a listing"))))))

(wdired-command "dired.finish" "Rename as the names now say"
  (fn (dir) (run "w")))
(wdired-command "dired.abort" "Give up the changes to the names"
  (fn (dir)
    (dired-render dir (car (cursor)))
    (message "The names are as they were")))

; ---- keeping the listing as it is

; What is not edited stays as it was put up, whatever keys did to it.
(add-hook 'buffer-changed
  (fn ()
    (let ((dir (dired-here)))
      (when (and dir (dired-known? dir) (not (dired-editing? dir))
                 (not (= (join (lines (buffer-text)) "\n") (dired-get dir :shown))))
        (dired-render dir (car (cursor)))
        (message "The listing is not edited: i edits the names")))))

(add-hook 'mode-changed
  (fn (mode)
    (let ((dir (dired-here)))
      (when (and (= mode 'insert) dir (dired-known? dir) (not (dired-editing? dir)))
        (feed-keys "<Esc>")
        (message "The listing is not edited: i edits the names")))))

(add-hook 'files-changed
  (fn () (dired-refresh-shown)))

; ---- keys

(defn dired-bind (table pairs)
  (while pairs
    (bind table (first pairs) (nth 1 pairs))
    (set! pairs (rest (rest pairs)))))

(bind 'normal "<Space>-" "dired.open")

(dired-bind 'dired
  '("<CR>" "dired.enter"  "a" "dired.enter"  "o" "dired.enter"
    "-" "dired.open"  "^" "dired.open"
    ">" "dired.next_dir"  "<lt>" "dired.prev_dir"
    "gr" "dired.refresh"  "s" "dired.sort"  "(" "dired.details"  "q" "dired.quit"
    "m" "dired.mark"  "d" "dired.flag"  "u" "dired.unmark"  "<BS>" "dired.unmark_back"
    "U" "dired.unmark_all"  "t" "dired.toggle_marks"
    "**" "dired.mark_executables"  "*/" "dired.mark_dirs"  "*@" "dired.mark_links"
    "*." "dired.mark_extension"  "*%" "dired.mark_regex"  "*!" "dired.unmark_all"
    "%m" "dired.mark_regex"  "%d" "dired.flag_regex"
    "%C" "dired.copy_regex"  "%R" "dired.rename_regex"
    "~" "dired.flag_backups"  "#" "dired.flag_autosaves"
    "C" "dired.copy"  "R" "dired.rename"  "S" "dired.symlink"  "H" "dired.hardlink"
    "D" "dired.delete"  "x" "dired.execute"  "+" "dired.mkdir"
    "M" "dired.chmod"  "O" "dired.chown"  "T" "dired.touch"  "Z" "dired.compress"
    "!" "dired.shell"  "&" "dired.shell"  "X" "dired.shell"  "=" "dired.diff"
    "i" "dired.edit"  "<C-x><C-q>" "dired.edit"
    "p" "motion.up"
    ; Vim's edits that are none of dired's.
    "A" "dired.read_only"  "I" "dired.read_only"  "c" "dired.read_only"
    "r" "dired.read_only"  "P" "dired.read_only"  "J" "dired.read_only"
    "." "dired.read_only"))

; Not C-c C-c: a C-c, as an escape, gives up the keys typed before it.
(dired-bind 'wdired
  '("<C-x><C-s>" "dired.finish"  "<C-x><C-q>" "dired.finish"  "<C-c><C-k>" "dired.abort"))
