; The file tree.
;
; `panel` has :root-name and :focused. Each of its `entries` has :name,
; :status (git's letter for it, or ""), :depth, and the flags :selected,
; :dir, :expanded, :added and :modified.

(set-layout 'tree
  (fn (panel)
    (col :width 'full :height 'full
         :bg 'sidebar :color 'sidebar-foreground
      (row :height 32 :shrink false :padding-x 12 :align 'center
           :border-bottom 1 :border-color 'sidebar-border
           :bold true
           :color (if (get panel :focused) 'sidebar-foreground 'muted-foreground)
        (text (get panel :root-name)))
      (rows 'entries :grow true
        (fn (entry)
          (row :height 24 :width 'full :align 'center
               :padding-left (+ 10 (* 14 (get entry :depth)))
               :padding-right 8
               :nowrap true
               :color (cond ((get entry :added) 'success)
                            ((get entry :modified) 'warning))
               :bg (when (get entry :selected)
                     (if (get panel :focused) 'list-active 'list-hover))
               :hover (list :bg 'list-hover)
               :on-click "tree.open"
            (text (cond ((not (get entry :dir)) "  ")
                        ((get entry :expanded) "▾ ")
                        (true "▸ "))
                  :color 'muted-foreground)
            (text (get entry :name))))))))
