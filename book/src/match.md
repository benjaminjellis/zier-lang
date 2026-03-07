# Match

Pattern match with `match`. Use `~>` to separate each pattern from its result. `_` is a wildcard.

```
(let describe {n}
  (match n
    0 ~> "zero"
    1 ~> "one"
    _ ~> "many"))
```

Or-patterns let multiple cases share a branch:

```
(let is-weekend {day}
  (match day
    "Saturday" or "Sunday" ~> True
    _                      ~> False))
```

