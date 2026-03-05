;; Error: function definitions require {args} — `(let f 42)` is not valid
;; Use `(let f {} 42)` for a zero-arg function or `(let [f 42] ...)` for a binding
(let f 42)
