# Primitive Types
`Opal` has `Int`, `Float`, `String`, `Bool`, and `Unit` as primitive types. `Int` and `Float` operators are distinct, float operators use a `.` suffix.

We can see this looking at the following two functions (more on those soon).

```
(let add_ints   {a b} (+ a b))   ;; Int -> Int -> Int
(let add_floats {a b} (+. a b))  ;; Float -> Float -> Float
```


`+.` works only for `Float` and `+` works only for `Int`.

`Bool` literals are `True` and `False`

```
(let always_true {} True)
(let always_false {} False)
```

`String` literals are encased in double quotes 

```
(let just_hello {} "Hello")
```

