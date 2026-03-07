# Match

You can pattern match with keyword `match`. Use `~>` to separate each pattern from its result. `_` is a wildcard.

Pattern matching works on single variables

```opal
(let describe {n}
  (match n
    0 ~> "zero"
    1 ~> "one"
    _ ~> "many"))
```


... on multiple variables

```opal
(let two_values {x y}
  (match x y
    10 12 ~> (io/println "matched")
    _ _ ~> (io/println "not matched")))
```

or on lists with the cons operator

```opal
(let iterate {list}
  (match list
    [] ~> (io/println "empty")
    [h | t] ~> (do (io/debug h)
                   (iterate t))))

```

Or-patterns let multiple cases share a branch:

```opal
(let is_weekend {day}
  (match day
    "Saturday" or "Sunday" ~> True
    _                      ~> False))
```


The list example above also introduces the `do` keyword. In some places the compile is expecting only one expression. This gives us an opportunity to demonstrate something else about `Opal`, the friendlily compiler errors. You may be tempted to not use `do`. You could the write the example above as 


```opal
(let iterate {list} 
  (match list 
    [] ~> (io/println "empty")
    [h | t] ~> (
                (io/debug h)
                (iterate t))))
```

But if you try and compile this, the compiler would say: 

```shell
error: type mismatch: expected `Unit`, found `('a -> 'b)`
  ┌─ main.opal:6:85
  │
6 │ (let iterate {list} (match list [] ~> (io/println "empty") [h | t] ~> ((io/debug h) (iterate t))))
  │                                                                                     ^^^^^^^^^^^ this argument has type `'a`
  │
  = expected `Unit`, found `('a -> 'b)`
  = hint: `Unit` is not a function — if you meant to sequence two expressions, use `(do expr1 expr2)`
```


