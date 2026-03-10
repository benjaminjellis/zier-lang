# Pipe
To chain multiple function calls and pipe data through `Mond` has `|>` 

```mond
(use std/io)

(let add_two {x y} (+ x y))

(let main {}
  (let [x (|> 10
              (add_two 1)
              (add_two 1)
              (add_two 1)
              (add_two 1)
              (add_two 1))]
    (io/debug x)))
```

Running this will print
```
15
```
