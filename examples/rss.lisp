; A feed reader, after Emacs's newsticker in its tree view: the feeds on
; the left, what each has on the right, and the one selected below it.
; It reads the feeds of Tsoding's channels on YouTube unless told
; otherwise.
;
;   (load "rss.lisp")
;
; :newsticker opens it. In it: j k move, h l (or tab) go to another feed,
; enter or o opens the video in the browser, J K scroll what it says of
; itself, g reads the feeds again, q closes. A feed is fetched with curl,
; and a video opened with `open` on macOS, `xdg-open` elsewhere.

(def newsticker-feeds
  '((:name "Tsoding Daily"
     :url "https://www.youtube.com/feeds/videos.xml?channel_id=UCrqM0Ym_NbK1fqeQG2VIohg")
    (:name "Tsoding"
     :url "https://www.youtube.com/feeds/videos.xml?channel_id=UCEbYhDd6c6vngsF5PQpFVWg")))

(def newsticker-news nil)     ; a feed's name to (:items ...) or (:error ...)
(def newsticker-feed 0)       ; which feed
(def newsticker-item 0)       ; which of its items
(def newsticker-read nil)     ; the links opened so far
(def newsticker-top 0)        ; the line of the description at its top
(def newsticker-fetched false)
(def newsticker-asked 0)      ; counts the fetches: only the last one's answers count
(def newsticker-waiting 0)    ; feeds the last fetch still waits for

; ---- reading a feed

(defn newsticker-unescape (text)
  (replace
    (replace (replace (replace (replace text "&quot;" "\"") "&#39;" "'")
                      "&lt;" "<")
             "&gt;" ">")
    "&amp;" "&"))

(defn newsticker-field (pattern entry)
  (let ((found (regex-match pattern entry)))
    (if found (newsticker-unescape (nth 1 found)) "")))

; An entry of an Atom feed, as YouTube writes them.
(defn newsticker-entry (entry)
  (list :title (newsticker-field "<title>([^<]*)</title>" entry)
        :link (newsticker-field "<link rel=\"alternate\" href=\"([^\"]*)\"" entry)
        :date (newsticker-field "<published>([0-9-]*)T" entry)
        :description (newsticker-field
                       "(?s)<media:description>(.*?)</media:description>" entry)))

; What comes before the first entry is the feed's own.
(defn newsticker-parse (xml)
  (map newsticker-entry (rest (split xml "<entry>"))))

; ---- fetching

(defn newsticker-store (name news)
  (set! newsticker-news (cons name (cons news newsticker-news))))

(defn newsticker-fetch-one (feed asked)
  (spawn "curl" (list "-sfL" "--max-time" "15" (get feed :url)) "/"
    (fn (ran why)
      (when (= asked newsticker-asked)
        (set! newsticker-waiting (- newsticker-waiting 1))
        (newsticker-store
          (get feed :name)
          (cond (why (list :error (str "curl: " why)))
                ((= (get ran :status) 0) (list :items (newsticker-parse (get ran :out))))
                (true (list :error (str "curl: exit " (get ran :status) " "
                                        (trim (get ran :err)))))))))))

(defn newsticker-fetch ()
  (set! newsticker-asked (+ newsticker-asked 1))
  (set! newsticker-news nil)
  (set! newsticker-waiting (length newsticker-feeds))
  (set! newsticker-fetched true)
  (let ((asked newsticker-asked))
    (map (fn (feed) (newsticker-fetch-one feed asked)) newsticker-feeds)))

; ---- what is selected

(defn newsticker-current-feed () (nth newsticker-feed newsticker-feeds))

(defn newsticker-news-of (feed) (get newsticker-news (get feed :name)))

(defn newsticker-items-of (feed) (get (newsticker-news-of feed) :items))

(defn newsticker-current ()
  (nth newsticker-item (newsticker-items-of (newsticker-current-feed))))

(defn newsticker-clamp (index count) (max 0 (min index (- count 1))))

(defn newsticker-select-feed (index)
  (set! newsticker-feed (mod index (length newsticker-feeds)))
  (set! newsticker-item 0)
  (set! newsticker-top 0))

(defn newsticker-select-item (index)
  (set! newsticker-item
        (newsticker-clamp index (length (newsticker-items-of (newsticker-current-feed)))))
  (set! newsticker-top 0))

(defn newsticker-description ()
  (let ((item (newsticker-current)))
    (when item (lines (get item :description)))))

(defn newsticker-scroll (delta)
  (set! newsticker-top
        (newsticker-clamp (+ newsticker-top delta) (length (newsticker-description)))))

(defn newsticker-open ()
  (let ((item (newsticker-current)))
    (when item
      (let ((link (get item :link)))
        (unless (member? link newsticker-read)
          (set! newsticker-read (cons link newsticker-read)))
        (spawn "sh"
               (list "-c"
                     "if [ \"$(uname)\" = Darwin ]; then open \"$1\"; else xdg-open \"$1\"; fi"
                     "sh" link)
               "/"
               (fn (ran why)
                 (cond (why (message "newsticker: " why))
                       ((= (get ran :status) 0) (message "Opened " link))
                       (true (message "newsticker: could not open " link)))))))))

; ---- how it looks

; A bordered box with a title, `size` being its width or share of the room.
(defn newsticker-box (title size child)
  (apply col
    (append size
            (list :border 1 :rounded 2 :border-color 'border :clip true
                  (text title :shrink false :padding-x 8 :nowrap true
                        :color 'muted-foreground)
                  child))))

(defn newsticker-feed-row (feed index)
  (let ((news (newsticker-news-of feed)))
    (row :height 22 :width 'full :align 'center :justify 'between
         :gap 8 :padding-x 8 :nowrap true
         :bg (when (= index newsticker-feed) 'list-active)
         :hover (list :bg 'list-hover)
         :on-click (fn () (newsticker-select-feed index))
      (text (get feed :name) :bold (= index newsticker-feed))
      (text (cond ((get news :error) "!")
                  ((get news :items) (str (length (get news :items))))
                  (true "…"))
            :color (if (get news :error) 'danger 'muted-foreground)))))

(defn newsticker-item-row (item index)
  (row :height 22 :width 'full :align 'center :gap 12 :padding-x 8 :nowrap true
       :bg (when (= index newsticker-item) 'list-active)
       :hover (list :bg 'list-hover)
       :on-click (fn ()
                   (if (= index newsticker-item)
                       (newsticker-open)
                       (newsticker-select-item index)))
    (text (get item :date) :shrink false :color 'muted-foreground)
    (text (get item :title) :grow true :clip true
          :bold (not (member? (get item :link) newsticker-read))
          :color (when (member? (get item :link) newsticker-read) 'muted-foreground))))

(defn newsticker-items-box ()
  (let ((feed (newsticker-current-feed))
        (news (newsticker-news-of feed)))
    (newsticker-box (get feed :name) '(:grow 1)
      (cond ((get news :error)
             (text (get news :error) :padding-x 8 :color 'danger))
            ((nil? news)
             (text "Fetching…" :padding-x 8 :color 'muted-foreground))
            (true
             (rows (get news :items) :grow true :reveal newsticker-item
               newsticker-item-row))))))

(defn newsticker-footer ()
  (if (> newsticker-waiting 0)
      (text (str "Fetching " newsticker-waiting " feeds…") :color 'warning)
      (text "j/k item  h/l feed  enter open  J/K scroll  g refresh  q close"
            :color 'muted-foreground)))

(defpanel 'newsticker :at 'full
  (fn (panel)
    (let ((item (newsticker-current)))
      (col :width 'full :height 'full
           :bg 'background :color 'foreground :mono true :size 12
        (row :grow true :gap 4 :padding-x 4 :padding-y 4
          (newsticker-box "Feeds" '(:width 240 :shrink false)
            (rows newsticker-feeds :grow true :reveal newsticker-feed
              newsticker-feed-row))
          (col :grow true :gap 4
            (newsticker-items-box)
            (newsticker-box (if item (get item :title) "") '(:grow 1)
              (rows (newsticker-description) :grow true :top newsticker-top
                (fn (line) (text line :height 18 :padding-x 8 :nowrap true))))))
        (row :height 24 :shrink false :padding-x 8 :align 'center
             :border-top 1 :border-color 'border :nowrap true :clip true
          (newsticker-footer))))))

; ---- what it does

(defcommand "newsticker" "Open or close the feed reader"
  (fn ()
    (if (panel-shown? 'newsticker)
        (hide-panel 'newsticker)
        (do (unless newsticker-fetched (newsticker-fetch))
            (focus-panel 'newsticker)))))
(defcommand "newsticker.close" "Leave the feed reader"
  (fn () (hide-panel 'newsticker)))
(defcommand "newsticker.refresh" "Read the feeds again" newsticker-fetch)
(defcommand "newsticker.down" "Select the next item"
  (fn () (newsticker-select-item (+ newsticker-item 1))))
(defcommand "newsticker.up" "Select the previous item"
  (fn () (newsticker-select-item (- newsticker-item 1))))
(defcommand "newsticker.next_feed" "Go to the next feed"
  (fn () (newsticker-select-feed (+ newsticker-feed 1))))
(defcommand "newsticker.prev_feed" "Go to the previous feed"
  (fn () (newsticker-select-feed (- newsticker-feed 1))))
(defcommand "newsticker.open" "Open the selected item in the browser"
  newsticker-open)
(defcommand "newsticker.scroll_down" "Scroll the description down"
  (fn () (newsticker-scroll 10)))
(defcommand "newsticker.scroll_up" "Scroll the description up"
  (fn () (newsticker-scroll -10)))

; ---- its keys

(defn newsticker-keys (command & keys)
  (map (fn (key) (bind 'newsticker key command)) keys))

(newsticker-keys "newsticker.down" "j" "n" "<Down>")
(newsticker-keys "newsticker.up" "k" "p" "<Up>")
(newsticker-keys "newsticker.next_feed" "l" "<Tab>" "<Right>")
(newsticker-keys "newsticker.prev_feed" "h" "<S-Tab>" "<Left>")
(newsticker-keys "newsticker.open" "<CR>" "o")
(newsticker-keys "newsticker.scroll_down" "J" "<PageDown>")
(newsticker-keys "newsticker.scroll_up" "K" "<PageUp>")
(newsticker-keys "newsticker.refresh" "g")
(newsticker-keys "newsticker.close" "q" "<Esc>")
