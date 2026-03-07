# Imports

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

