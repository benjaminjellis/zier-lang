# Functions
`Opal` let's users define functions at the top level of a file.

Let's say we have a file called `my_file.opal` with the following contents.
```
(let square {x}
  (* x x))
```

This is a perfectly valid `Opal` file which:
- defines a function `square`
- that takes one arg `x` (arguments to functions live inside the curly brackets `{}`)
- The body of the function is then defined in the second set of round brackets. This is how you invoke functions in `Opal`, using Polish Notation. The function comes first then the arguments
- returns `x` squared as `Opal` uses implicit return

So if you wanted to invoke `square` you could define a `main` function like this and call `square` in the body.

```
(let main {}
  (square 10))
```

Opal supports self-recursive functions (with tail call optimisation). So we can write a function to calculate the factorial of a number as below.

```
(let factorial {n}
  (if (= n 0)
    1
    (* n (factorial (- n 1)))))
```

By convention functions names are `snake_case`

Note that no type signature is required (Opal doesn't actually support them apart from when calling directly into Erlang, we'll cover that later). `Opal` uses a Hindley-Milner type system that infers types at compile times so no type signatures are eve needed.
