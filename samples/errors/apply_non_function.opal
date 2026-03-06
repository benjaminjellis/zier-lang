;; Error: trying to call an Int argument as if it were a function
(let call_it {f x} (f x))

(let main {} (call_it 42 1))
