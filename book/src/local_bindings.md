# Local Bindings
You can bind local variables inside functions using `(let [name value] body)`. Bindings can be chained and each name is in scope for the body of the local binding.

As an example below we have a function called `circle_area` which: 
- takes one argument `r`
- binds data to two local variables `pi` and `r_sq` using a local `let` binding
- returns the area of the circle

```
(let circle_area {r}
  (let [pi   3.14159
        r_sq (*. r r)]
    (*. pi r_sq)))
```


Note:
- like function names local `let` bindings, by convention, use `snake_case` identifiers
- this function has the type `Float -> Float` which `Opal` infers because of the use of `*.`
