;; Error: condition must be Bool, not Int
(let safe_div {n d}
  (if d
    (/ n d)
    0))
