# Primitive Types
`Mond` has `Int`, `Float`, `String`, `Bool`, and `Unit` as primitive types. 

## Int and Float
`Int` and `Float` operators are distinct. Float operators use a `.` suffix.

We can see this looking at the following two functions (more on those soon).

```mond
(let add_ints   {a b} (+ a b))   ;; Int -> Int -> Int
(let add_floats {a b} (+. a b))  ;; Float -> Float -> Float
```

`+.` works only for `Float` and `+` works only for `Int`.
`%` is available for integer modulo:

```mond
(let mod_two {x} (% x 2))
```

Signed numeric literals are also supported:

```mond
(let negative_int {} -1)
(let negative_float {} -1.5)
```

Numeric separators with `_` are supported for readability:

```mond
(let million {} 1_000_000)
(let tax_rate {} 12_500.25)
```

## Bool
`Bool` literals are `True` and `False`.

```mond
(let always_true {} True)
(let always_false {} False)
```

Boolean operators are `and`, `or`, and `not`:

```mond
(let can_enter {has_ticket is_member}
  (and has_ticket (not is_member)))
```

## String
`String` literals are enclosed in double quotes.

```mond
(let just_hello {} "Hello")
```
