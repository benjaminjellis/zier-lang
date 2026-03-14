# Types
`Mond` is statically typed. There are two forms of types that you can express:

## Sum / Variant Types
e.g. `Result` and `Option`

```
(type ['a] Option
  [None
   (Some ~ 'a)])


(let greet {name}
  (match name
    None     ~> (io/println "Hello, stranger!")
    (Some n) ~> (io/println (string/append "Hello, " n))))
```

Sum types have two components:
1. the name of the type (`Option` in the case above)
2. the constructors (`None` and `Some`)

The constructors can be nullary (like `None`) or encompass data (like `Some`). The `~` is used to provide a type that `Some` encompasses. This can be a concrete type like `Int` or, as above, it could be a polymorphic type like `'a`. By convention variant / sum type constructors use `PascalCase` identifiers.

## Product / Record Types
```
(type Point
  [(:x ~ Int)
   (:y ~ Int)])

(let origin {} (Point :x 0 :y 0))

(let x_coord {p} (:x p))
```

Record types have fields which, by convention, are `snake_case` identifiers. Each is prefixed with `:`. You can access a field with `(:field record)`. `~` is again used to specify what type each field is. Just like sum/variant types, you can use a polymorphic type like `'a`.

e.g.
```mond
(type ['a] Point
  [(:x ~ 'a)
   (:y ~ 'a)])
```
By convention all type names are `PascalCase` identifiers.

### Record update 
`Mond` allows for easy updating of record with the keyword `with`.

```mond
(let update_x {point new_x}
  (with point
    :x new_x))

(let main {}
  (let [my_point (Point :x 10 :y 12)]
    (io/debug my_point)
    (let [updated_point (update_x my_point 50)]
      (io/debug updated_point))))
```
