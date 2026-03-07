# The Standard Library

`(use std)` brings in the standard library and it's modules and lets you call functions as `module/function`:

```
(use std)

(let main {}
  (io/println "hello")
  (io/println (string/to_upper "hello")))
```

`(use std/io)` imports a single module and brings its public functions into scope unqualified:

```
(use std/io)

(let main {}
  (println "hello"))
```


`let?` is syntactic sugar for monadic bind. It requires a `bind` function in scope and chains operations that return a `Result`, short-circuiting on the first error. This syntax can be used simply with `(use std/result)`.

```
(let? [a (might-fail)
       b (also-might-fail a)]
  (Ok (+ a b)))
```

This desugars to `(bind (might-fail) (fn {a} (bind (also-might-fail a) (fn {b} (Ok (+ a b))))))`.

