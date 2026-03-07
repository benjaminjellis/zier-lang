# The Standard Library


`let?` is syntactic sugar for monadic bind. It requires a `bind` function in scope and chains operations that return a `Result`, short-circuiting on the first error. This syntax can be used simply with `(use std/result)`.

```
(let? [a (might-fail)
       b (also-might-fail a)]
  (Ok (+ a b)))
```

This desugars to `(bind (might-fail) (fn {a} (bind (also-might-fail a) (fn {b} (Ok (+ a b))))))`.

