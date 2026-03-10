# Erlang FFI
One of the big benefits of targeting the BEAM is being able to leverage a rich and mature ecosystem.

We can do so by binding Erlang functions to `Mond` names with `extern let`. We said earlier that `Mond` does not use type signatures, this was a little white lie. `extern` declarations are the only place where `Mond` uses type signatures.

```
(extern let system-time ~ (Unit -> Int) erlang/system_time)
```

`pub extern let` makes the binding importable by other modules — this is how large parts of the standard library are implemented e.g.

```
(pub extern let println ~ (String -> Unit) io/format)
```

We can do something similar for opaque foreign-backed types.

```mond
(pub extern type Pid)
(pub extern type ['k 'v] Map maps/map)
```

The trailing `module/type` target on `extern type` is optional metadata. Use it
when it helps document the foreign runtime type, but plain opaque declarations
such as `(pub extern type Pid)` are also valid.
