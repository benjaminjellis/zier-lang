;; Error: `rec` has been removed from the language
;; Named functions are self-recursive by default — just write:
;;   (let countdown {n} ...)
(let rec countdown {n}
  (if (= n 0)
    0
    (countdown (- n 1))))
