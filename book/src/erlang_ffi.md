# Erlang FFI
Bind an Erlang function to an Opal name with `extern let`, providing a type signature and the `module/function` target:

```
(extern let system-time ~ (Unit -> Int) erlang/system_time)
```

`pub extern let` makes the binding importable by other modules — this is how large parts of the standard library are implemented:

```
(pub extern let println ~ (String -> Unit) io/format)
```

