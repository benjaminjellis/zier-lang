# If / else
If/else can be used for control flow as below.

```mond
(let abs {x}
  (if (< x 0)
    (- 0 x)
    x))
```

## `if let`

Use `if let` when you want to test one pattern and fall back to an else branch.
The syntax is:

```mond
(if let [<pattern> <value>]
  <then-branch>
  <else-branch>)
```

Example:

```mond
(let selector_or_default {initialised subject}
  (if let [(Some selector) (:selector initialised)]
    selector
    (process/select (process/new_selector) subject)))
```

`if let` is shorthand for a `match` with one explicit arm and `_` fallback:

```mond
(if let [(Some x) maybe]
  x
  0)

;; equivalent to

(match maybe
  (Some x) ~> x
  _ ~> 0)
```
