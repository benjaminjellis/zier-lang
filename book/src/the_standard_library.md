# The Standard Library
`Mond`'s standard library is just like any other dependency, it is specified in your `bahn.toml` and can be pinned to a specific version. 

The standard library is intended to be small but provide the essential building blocks for writing `mond`.

## Imports
To get started with the standard library, we first need to introduce a new concept: imports. `Mond` defines the keyword `use`. Like everything in `Mond`, this lives inside an S-expression.

`(use std/io)` at the top of the file brings the module `io` from `std` into scope.

Here's an example:

```mond
(use std/io)

(let main {}
  (io/println "hello")
  (io/println (string/to_upper "hello")))
```

All of `io`'s functions and type defs are in-scope using the `io/` qualifier.

If you only want to bring in a subset of what's defined in a module and use it in an unqualified manner, you can use square brackets to do so.

```mond
(use std/io [println])

(let main {}
  (println "hello"))
```

You can also import everything unqualified with `[*]`:

```mond
(use std/io [*])
```


## Monadic Types

The standard library also provides some useful types like `Option` and `Result`. It is idiomatic to import these in an unqualified manner. This also imports these types' constructors (i.e. `None`, `Some`, `Ok` and `Error`).

```mond
(use std/result [Result])
(use std/option [Option])
```

The language also provides syntactic sugar with `let?`. It chains operations that return a `Result`, short-circuiting on the first error. `let?` is built in, so no `bind` import is required.

```mond
(use std/result [Result])
(use std/io)

(let might_fail {} (Ok 10))

(let might_also_fail {x} (Ok (+ x 10)))

(let main {}
  (let? [a (might_fail) b (might_also_fail a)]
    (do (io/debug a)
        (io/debug b)
        (Ok (+ a b)))))
```

This desugars to:

`(match (might_fail) (Ok a) ~> (match (might_also_fail a) (Ok b) ~> (Ok (+ a b)) (Error e) ~> (Error e)) (Error e) ~> (Error e))`

If you run it, you'll see:

```shell
10
20
```

## Processes
Because `Mond` targets the `BEAM`, it can use Erlang processes directly via `std/process`.

```mond
(use std/process)
```

Core process primitives:

- `process/spawn` and `process/spawn_link` to start work concurrently
- `process/new_subject` to create a typed mailbox endpoint
- `process/send` and `process/receive_timeout` to exchange typed messages
- `process/new_name`, `process/register`, and `process/named_subject` for globally named mailboxes

Minimal subject round-trip:

```mond
(use std/process)
(use std/testing [assert_eq])

(test
  "subject send/receive"
  (let [subject (process/new_subject)
        _       (process/spawn
          (f {_} ->
            (do (process/sleep 10)
                (process/send subject "pong")
                ())))]
    (assert_eq (process/receive_timeout subject 1000) (Ok "pong"))))
```

Named subjects:

```mond
(use std/process)
(use std/result [Result])
(use std/testing [assert_eq])

(test
  "named subject"
  (let [name    (process/new_name "named-mailbox")
        subject (process/named_subject name)]
    (let? [_ (assert_eq (process/register name) (Ok ()))]
      (let? [_ (assert_eq (process/send subject "named-pong") "named-pong")]
        (assert_eq (process/receive_timeout subject 1000) (Ok "named-pong"))))))
```

`receive_timeout` returns `Result 'm Unit`:

- `Ok message` when a message with the right subject tag arrives
- `Error ()` on timeout

## Unknown
`std/unknown` is for decoding values from untyped boundaries (FFI, external data, dynamic payloads) into typed Mond values.

```mond
(use std/unknown [DecodeError])
```

The core flow is:

1. Start with an `Unknown` value (`unknown/from` or `unknown/from_string`)
2. Build a decoder (`unknown/int`, `unknown/string`, `unknown/list`, `unknown/field`, ...)
3. Run it with `unknown/run`

Simple decode:

```mond
(use std/testing [assert_eq])

(test
  "unknown/int success"
  (assert_eq (unknown/run (unknown/from 42) (unknown/int)) (Ok 42)))
```

Failure includes structured errors:

```mond
(use std/testing [assert_eq])

(test
  "unknown/int failure"
  (assert_eq
    (unknown/run (unknown/from "nope") (unknown/int))
    (Error [(DecodeError :expected "Int" :found "String")])))
```

Composed decoders:

```mond
(use std/map)
(use std/testing [assert_eq])

(let player_data {}
  (map/put "name" "Lucy" (map/new)))

(test
  "unknown/field success"
  (assert_eq
    (unknown/run
      (unknown/from (player_data))
      (unknown/field "name" (unknown/string)))
    (Ok "Lucy")))
```

`unknown/run` returns `Result 'a (List DecodeError)`, so decoding can be handled with normal `Result` control flow (`let?`, `bind`, `match`).
