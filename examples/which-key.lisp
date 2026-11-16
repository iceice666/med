; Which key: once a command is half typed and nothing follows for a
; moment, the keys that can come next, at the bottom of the window.
;
;   (load "which-key.lisp")
;
; It works wherever keys are looked up in tables: in an editor (after
; `g`, `z`, the space of a leader key) and in a panel that has the
; keyboard. Escape abandons the command, and the list goes with it.
;
; A key that only starts longer bindings is listed as a group, by the
; name it was given, or else by how many keys follow it:
;
;   (which-key-name "<Space>g" "git")
;
; A key can also ask for the list straight away, with every key of a
; table on it: `?` in the tree, and in a listing of dired.
;
;   (which-key-help-key 'tree "?")

(def which-key-delay 300)        ; how long to wait before showing, in ms
(def which-key-columns 4)        ; how many columns the keys are laid out in
(def which-key-row 22)           ; how tall a row is
; Whether to list every motion after an operator (`d`), which is a lot.
(def which-key-after-operator false)

(def which-key-names nil)        ; names of groups: (keys name keys name...)
(def which-key-typed "")         ; the keys of the command so far
(def which-key-items nil)        ; what is on show, each (:key :label :group)
(def which-key-timer nil)        ; the wait before showing
(def which-key-help-keys nil)    ; the keys that show it without waiting

; A key that is a binding by itself and starts longer ones (`space`) would
; act after a second, and take the list away with it. Have it wait for the
; next key instead; escape gives up on it.
(set-option 'key-timeout nil)

(defn which-key-name (keys name)
  (set! which-key-names (append (list keys name) which-key-names)))

; ---- what is on show

; The keys that can follow, from every table the next key is looked up
; in: a key that one table does not know goes on to the next, so where
; two know the same key the earlier one's is the one shown.
(defn which-key-next (tables keys)
  (reduce
    (fn (found table)
      (append found
              (filter (fn (next)
                        (not (find-index (fn (seen) (= (get seen :key) (get next :key)))
                                         found)))
                      (bindings table keys))))
    nil
    tables))

; Whether there is anything to list for a half-typed command: not for a
; key that is taken as it is (after `f`, or `"`), not while typing text,
; and after a count or an operator only if that was asked for.
(defn which-key-wanted? (pending)
  ; The last of an editor's tables is its mode's.
  (let ((table (last (get pending :tables))))
    (cond
      ((nil? pending) false)
      ((get pending :awaiting) false)
      ((= table "insert") false)
      ((= (get pending :keys) "") (and which-key-after-operator
                                       (= table "operator")))
      (true true))))

; What a key does, in a few words: what its command says of itself, or
; the command's name, or the keys it stands for.
(defn which-key-label (next)
  (cond
    ((get next :description) (get next :description))
    ((get next :command) (get next :command))
    ((get next :to) (str "→ " (get next :to)))
    (true nil)))

; The name of the bindings that go on from a key: the one they were
; given, or how many they are.
(defn which-key-group (tables next)
  (when (get next :prefix)
    (str "+" (or (get which-key-names (get next :keys))
                 (str (length (which-key-next tables (get next :keys)))
                      " keys")))))

(defn which-key-show (pending)
  (let ((tables (get pending :tables)))
    (set! which-key-typed (get pending :typed))
    (set! which-key-items
          (map (fn (next)
                 (list :key (get next :key)
                       :label (which-key-label next)
                       :group (which-key-group tables next)))
               (which-key-next tables (get pending :keys))))
    (if which-key-items
        (show-panel 'which-key)
        (hide-panel 'which-key))))

(add-hook 'keys-pending
  (fn (pending)
    (cancel which-key-timer)
    (cond
      ((not (which-key-wanted? pending)) (hide-panel 'which-key))
      ; Already up, or asked for: the next key is not waited for again.
      ((or (panel-shown? 'which-key)
           (member? (get pending :keys) which-key-help-keys))
       (which-key-show pending))
      (true (set! which-key-timer
                  (after which-key-delay (fn () (which-key-show pending))))))))

; ---- a key that asks for the list

; `key` in `table` starts every binding of the table over again: typed,
; it waits for the next key with all of them on show, and that key does
; what it does in the table. Escape puts the list away. The bindings are
; those of the table when this is called: one made later wants another
; call.

(defcommand "which-key.close" "Put the list of keys away"
  (fn () (hide-panel 'which-key)))

(defn which-key-copy (table key keys)
  (map (fn (next)
         (let ((from (get next :keys))
               (to (str key from)))
           ; Not the key itself, nor escape, which is to give up.
           (unless (or (= from key) (= from "<Esc>"))
             (when (get next :command) (bind table to (get next :command)))
             (when (get next :to) (bind-keys table to (get next :to)))
             (when (get next :prefix) (which-key-copy table key from)))))
       (bindings table keys)))

(defn which-key-help-key (table key)
  (which-key-copy table key "")
  (bind table (str key "<Esc>") "which-key.close")
  ; Bound by itself as well: a key the table does not have then does
  ; what it does without `key`, rather than after the mode's `key` (vim's
  ; `?`, a search backwards).
  (bind table key "which-key.close")
  (unless (member? key which-key-help-keys)
    (set! which-key-help-keys (cons key which-key-help-keys))))

; ---- how it looks

(defn which-key-take (count items)
  (if (or (= count 0) (nil? items))
      nil
      (cons (car items) (which-key-take (- count 1) (cdr items)))))

(defn which-key-drop (count items)
  (if (or (= count 0) (nil? items))
      items
      (which-key-drop (- count 1) (cdr items))))

; The items in so many columns, read down and then across. The last
; columns may have nothing in them: a key is where it was the last time.
(defn which-key-split (columns rows items)
  (if (= columns 0)
      nil
      (cons (which-key-take rows items)
            (which-key-split (- columns 1) rows (which-key-drop rows items)))))

(defn which-key-entry (item)
  (row :height which-key-row :gap 10 :align 'center :nowrap true
    (text (get item :key) :mono true :bold true :color 'primary
          :width 56 :shrink false)
    ; A key that is a command and starts others says both.
    (when (get item :label) (text (get item :label) :clip true))
    (when (get item :group)
      (text (get item :group) :color 'muted-foreground :clip true))))

(defpanel 'which-key :at 'bottom
  (fn (panel)
    (let ((rows (/ (+ (length which-key-items) which-key-columns -1)
                   which-key-columns)))
      (col :width 'full :bg 'sidebar :padding-x 12 :padding-y 6 :gap 2
        (row :gap 10 :align 'center :height which-key-row
          (text which-key-typed :mono true :bold true)
          (text "esc to cancel" :size 11 :color 'muted-foreground))
        (row :width 'full :gap 16
          (map (fn (column) (col :grow true (map which-key-entry column)))
               (which-key-split which-key-columns rows which-key-items)))))))
