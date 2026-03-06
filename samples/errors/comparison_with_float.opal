;; Error: comparison operators `<` `>` etc. require Int, not Float
(let bigger {a b} (if (< a b) b a))

(let main {} (bigger 1.5 2.5))
