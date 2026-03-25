# Pipe
To chain multiple function calls and pipe data through `Mond` has `|>`.

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

## Placeholder Pipe

By default, each pipe step receives the current value as its single argument:

```mond
(|> x f g)
```

This is equivalent to:

```mond
(g (f x))
```

If you need to place the piped value somewhere else in a step, use `_` as a
placeholder:

```mond
(|> 3
    (add 1 _)
    (mul _ 2))
```

This is equivalent to:

```mond
(mul (add 1 3) 2)
```

Rules:

- If a step has no `_`, `|>` keeps normal behavior and passes the value as a
  single argument.
- If a step has exactly one `_`, the piped value is inserted at `_`.
- If a step has more than one `_`, compilation fails.
