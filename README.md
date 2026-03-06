# opal
opal is an experimental, functional lisp-ish language with ml-ish semantics than runs on [BEAM](https://www.erlang.org/blog/a-brief-beam-primer/)

## Installing

Opal is written in Rust, to install it you'll need a Rust toolchain which you can get from https://rustup.rs/ and to run Opal code you'll need to install erlang and to create a release you'll need rebar3

To install both on arch linux run the following
```
sudo pacman -S rustup erlang rebar3
```

Once you have those installed you can clone this repo and in the route run 
```
 cargo install --path loupe
```

Then you'll be able to use loupe, opal's build tool. 

## Language tour

### Hello, world

Every binary project has a `src/main.opal` with a `main` function as the entry point.

```
(use std)

(let main {}
  (io/println "Hello, world!"))
```

Run it with `loupe run`.

### Functions

Top-level functions are declared with `let`. Arguments go in `{}`. All named functions are self-recursive by default.

```
(let square {x}
  (* x x))

(let factorial {n}
  (if (= n 0)
    1
    (* n (factorial (- n 1)))))
```

### Local bindings

`(let [name value] body)` binds a name for use in a function `body`. Bindings can be chained — each name is in scope for the rest.

```
(let circle-area {r}
  (let [pi   3.14159
        r-sq (*. r r)]
    (*. pi r-sq)))
```

### Primitive Types

Opal has `Int`, `Float`, `String`, `Bool`, and `Unit` as primitive type. `Int` and `Float` operators are distinct — float operators carry a `.` suffix.

```
(let add-ints   {a b} (+ a b))   ;; Int -> Int -> Int
(let add-floats {a b} (+. a b))  ;; Float -> Float -> Float
```

### If / else

```
(let abs {x}
  (if (< x 0)
    (- 0 x)
    x))
```

### Match

Pattern match with `match`. Use `~>` to separate each pattern from its result. `_` is a wildcard.

```
(let describe {n}
  (match n
    0 ~> "zero"
    1 ~> "one"
    _ ~> "many"))
```

Or-patterns let multiple cases share a branch:

```
(let is-weekend {day}
  (match day
    "Saturday" or "Sunday" ~> True
    _                      ~> False))
```

### Variant types (sum types)

```
(type ['a] Option
  (None
   (Some ~ 'a)))


(let greet {name}
  (match name
    None     ~> (io/println "Hello, stranger!")
    (Some n) ~> (io/println (string/append "Hello, " n))))
```

### Record types (product types)

Fields are prefixed with `:`. Access a field with `(:field record)`.

```
(type Point
  ((:x ~ Int)
   (:y ~ Int)))

(let origin {} (Point :x 0 :y 0))

(let x-coord {p} (:x p))
```

### Lambdas

Anonymous functions with `fn`:

```
(let apply {f x} (f x))

(let main {}
  (apply (fn {x} (* x 2)) 21))  ;; 42
```

### Lists

```
(let nums {} [1 2 3 4 5])
```

### Imports

`(use std)` brings in the standard library and lets you call functions as `module/function`:

```
(use std)

(let main {}
  (io/println "hello")
  (io/println (string/to_upper "hello")))
```

`(use std/io)` imports a single module and brings its functions into scope unqualified:

```
(use std/io)

(let main {}
  (println "hello"))
```

### Result bind

`let?` is syntactic sugar for monadic bind. It requires a `bind` function in scope and chains operations that return a `Result`, short-circuiting on the first error.

```
(let? [a (might-fail)
       b (also-might-fail a)]
  (Ok (+ a b)))
```

This desugars to `(bind (might-fail) (fn {a} (bind (also-might-fail a) (fn {b} (Ok (+ a b))))))`.

### Erlang FFI

Bind an Erlang function to an Opal name with `extern let`, providing a type signature and the `module/function` target:

```
(extern let system-time ~ (Unit -> Int) erlang/system_time)
```

`pub extern let` makes the binding importable by other modules — this is how the standard library is implemented:

```
(pub extern let println ~ (String -> Unit) io/format)
```

### Building and releasing

```
loupe build    # compile to target/debug/
loupe run      # compile and run
loupe release  # produce a standalone escript via rebar3 → target/release/
loupe clean    # wipe target/
```
