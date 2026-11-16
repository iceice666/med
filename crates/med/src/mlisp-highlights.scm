; med-lisp, read with the Scheme grammar. For one node the first pattern
; that captures it is the one that counts, so the narrow ones come first.

(comment) @comment

(string) @string
(escape_sequence) @string.escape
(number) @number

((symbol) @boolean
  (#match? @boolean "^(true|false)$"))
((symbol) @constant.builtin
  (#eq? @constant.builtin "nil"))

; :keyword
((symbol) @string.special.symbol
  (#match? @string.special.symbol "^:"))

; 'name
(quote
  "'" @punctuation.special
  (symbol) @string.special.symbol)
(quote "'" @punctuation.special)

((symbol) @punctuation.special
  (#eq? @punctuation.special "&"))

; The special forms.
(list
  .
  (symbol) @keyword
  (#match? @keyword "^(quote|if|def|defn|fn|let|do|set!|and|or|cond|when|unless|while)$"))

; (defn name (parameters) ...), (fn (parameters) ...)
(list
  .
  (symbol) @_defn
  .
  (symbol) @function
  (#eq? @_defn "defn"))
(list
  .
  (symbol) @_defn
  .
  (symbol)
  .
  (list (symbol) @variable)
  (#eq? @_defn "defn"))
(list
  .
  (symbol) @_fn
  .
  (list (symbol) @variable)
  (#eq? @_fn "fn"))

; (def name value), (set! name value)
(list
  .
  (symbol) @_def
  .
  (symbol) @variable.special
  (#match? @_def "^(def|set!)$"))

; (let ((name value) ...) ...)
(list
  .
  (symbol) @_let
  .
  (list
    (list
      .
      (symbol) @variable))
  (#eq? @_let "let"))

; What a list calls.
(list
  .
  (symbol) @operator
  (#match? @operator "^(\\+|-|\\*|/|=|<|>|<=|>=)$"))
(list
  .
  (symbol) @function)

["(" ")"] @punctuation.bracket
