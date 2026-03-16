# Match

You can pattern match with the keyword `match`. Use `~>` to separate each pattern from its result. `_` is a wildcard.

Pattern matching works on single variables

```mond
(let describe {n}
  (match n
    0 ~> "zero"
    1 ~> "one"
    _ ~> "many"))
```


... on multiple variables

```mond
(let two_values {x y}
  (match x y
    10 12 ~> (io/println "matched")
    _ _ ~> (io/println "not matched")))
```

or on lists with the cons operator

```mond
(let iterate {list}
  (match list
    [] ~> (io/println "empty")
    [h | t] ~> (do (io/debug h)
                   (iterate t))))

```

List patterns also support fixed-length and mixed forms:

```mond
(let describe {list}
  (match list
    [x]          ~> "singleton"
    [a b | rest] ~> "at least two"
    []           ~> "empty"))
```

`[x]` is equivalent to `[x | []]`, and `[a b | rest]` is equivalent to `[a | [b | rest]]`.


For patterns with multiple cases on one branch you can use `|`

```mond
(let is_weekend {day}
  (match day
    "Saturday" | "Sunday" ~> True
    _                     ~> False))
```

You can also destructure records in `match` arms using named fields:

```mond
(type Person
  [(:name ~ String)
   (:age ~ Int)])

(let age_of {person}
  (match person
    (Person :age age) ~> age))
```

Record patterns can be partial (you do not need to list every field), and they can be nested:

```mond
(type Address
  [(:city ~ String)])

(type Person
  [(:name ~ String)
   (:address ~ Address)])

(let city_of {person}
  (match person
    (Person :address (Address :city city)) ~> city))
```


The list example above also introduces the `do` keyword. In some places, the compiler is expecting only one expression but you might like to do more. This gives us an opportunity to demonstrate something else about `Mond`: the friendly compiler errors. You may be tempted not to use `do`. You could write the example above as:


```mond
(let iterate {list} 
  (match list 
    [] ~> (io/println "empty")
    [h | t] ~> (
                (io/debug h)
                (iterate t))))
```

But if you try to compile this, the compiler would say:

```shell
error: type mismatch: expected `Unit`, found `('a -> 'b)`
  ┌─ main.mond:6:85
  │
6 │ (let iterate {list} (match list [] ~> (io/println "empty") [h | t] ~> ((io/debug h) (iterate t))))
  │                                                                                     ^^^^^^^^^^^ this argument has type `'a`
  │
  = expected `Unit`, found `('a -> 'b)`
  = hint: `Unit` is not a function — if you meant to sequence multiple expressions, use `(do expr1 expr2 ...)`
```
