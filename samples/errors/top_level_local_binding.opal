;; Error: (let [x 42] ...) is a local binding, only valid inside a function body
;; Top-level definitions must be functions: (let name {args} body)
(let [x 42]
  (+ x 1))
