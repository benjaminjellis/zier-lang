;; Error: `:x` is a field accessor for Point, not applicable to an Int
(type Point (
  (:x ~ Int)
  (:y ~ Int)))

(let main {}
  (:x 99))
