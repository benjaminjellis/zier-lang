# Functions
`Mond` lets users define functions at the top level of a file.

Let's say we have a file called `my_file.mond` with the following contents:
```mond
(let square {x}
  (* x x))
```

This is a perfectly valid `Mond` file which:
- defines a function `square`
- that takes one arg `x` (arguments to functions live inside the curly brackets `{}`)
- The body of the function is then defined in the second set of round brackets. This is how you invoke functions in `Mond`, using Polish Notation. The function comes first then the arguments
- returns `x` squared as `Mond` uses implicit return

So if you wanted to invoke `square` you could define a `main` function like this and call `square` in the body.

```mond
(let main {}
  (square 10))
```

Mond supports self-recursive functions (with tail call optimisation). So we can write a function to calculate the factorial of a number as below.

```mond
(let factorial {n}
  (if (= n 0)
    1
    (* n (factorial (- n 1)))))
```

By convention, function names are `snake_case`.

Use `pub let` to export a function from a module:

```mond
(pub let square {x}
  (* x x))
```

`Mond` also has anonymous functions using `f`:

```mond
(let apply {func x}
  (func x))

(let main {}
  (apply (f {n} -> (+ n 1)) 10))
```

`Mond` uses a Hindley-Milner type system, this means that all types are inferred at compile time and no type signatures are required. Because of this `Mond` doesn't actually support type signatures.
