;; Error: match arms must all return the same type
;; first arm returns Int (x + 1), second arm returns Bool (False)
(type ['a] Option ( None (Some ~ 'a) ))

(let unwrap {opt} (match opt (Some x) ~> (+ x 1) None ~> False))
