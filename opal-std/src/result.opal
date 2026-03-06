(pub type ['e 'a] Result (
  (Ok ~ 'a)
  (Error ~ 'e)))

;; bnd for let?
(pub let bind {m f}
  (match m
    (Ok x)    ~> (f x)
    (Error e) ~> (Error e)))


