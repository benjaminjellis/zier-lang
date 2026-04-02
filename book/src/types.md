# Types
`Mond` is statically typed. There are two forms of types that you can express:

## Sum / Variant Types
e.g. `Result` and `Option`

```mond
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

Constructors can also take multiple positional payload values:

```mond
(type IpAddress
  [(IpV4 ~ Int Int Int Int)
   (IpV6 ~ Int Int Int Int Int Int Int Int)])

(let octet_sum {ip}
  (match ip
    (IpV4 a b c d) ~> (+ (+ a b) (+ c d))
    (IpV6 _ _ _ _ _ _ _ _) ~> 0))
```

## Product / Record Types
```mond
(type Point
  [(:x ~ Int)
   (:y ~ Int)])

(let origin {} (Point :x 0 :y 0))

(let x_coord {p} (:x p))
```

Record types have fields which, by convention, are `snake_case` identifiers. Each is prefixed with `:`. You can access a field with `(:field record)`. `~` is again used to specify what type each field is. Just like sum/variant types, you can use a polymorphic type like `'a`.
You can also destructure records in `match` using named field patterns (for example `(Person :age age)`).

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

### Field Constraints
When multiple records share a field label (for example both have `:selector`), `Mond` now infers a field constraint instead of committing to one record too early.

```mond
(type ContinuePayload [(:selector ~ Int)])
(type Initialised [(:selector ~ Int)])

(let read_selector {x} (:selector x))
```

The inferred type for `read_selector` is shown as a qualified type:

```mond
HasField :selector 'a Int => 'a -> Int
```

This means `read_selector` works for any record type that has a `:selector` field of type `Int`.

### Migration Notes
- If you see `unsatisfied field constraint ...`, no visible record instance matches that field label/type at the call site.
- If you see `ambiguous field constraint ...`, the value is still too polymorphic; add a concrete type constraint (for example with `match`, constructor use, or a more specific helper argument type).
- LSP hover, completion details, and signature help now include these inferred constraints so you can see exactly what a polymorphic field helper requires.
