(type ['e 'a] Result (
  (Ok ~ 'a)
  (Error ~ 'e)))

(let bind {m f}
  (match m
    (Ok x)    ~> (f x)
    (Error e) ~> (Error e)))

(let safe_div {a b}
  (if (= b 0)
    (Error "division by zero")
    (Ok (/ a b))))

;; implicit sequencing in function body
(let log_and_compute {x y z}
  (safe_div x y)
  (let [some_list [1 2 3 4 5 6]])
  (safe_div y z)
  (let? [a (safe_div x y)
         b (safe_div a z)]
    (Ok b)))

;; implicit sequencing in let body
(let compute {x y z}
  (let [a (safe_div x y)]
    (safe_div x z)
    a))

(let main {}
  (log_and_compute 100 5 4))
