# Currying
Like many functional languages `Mond` supports currying.

For example we can define a function that adds two numbers

```mond
(let add_two {x y} (+ x y))
```

We can reuse that function via partial application to create a new function that adds 10 toe a number

```mond
(let add_ten {x} (add_two 10 x))
```
