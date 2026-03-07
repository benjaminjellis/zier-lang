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

### Lambdas

Anonymous functions with `fn`:

```
(let apply {f x} (f x))

(let main {}
  (apply (fn {x} (* x 2)) 21))  ;; 42
```


### Erlang FFI

Bind an Erlang function to an Opal name with `extern let`, providing a type signature and the `module/function` target:

```
(extern let system-time ~ (Unit -> Int) erlang/system_time)
```

`pub extern let` makes the binding importable by other modules — this is how large parts of the standard library are implemented:

```
(pub extern let println ~ (String -> Unit) io/format)
```

### Building and releasing

opal's build tool (and soon to be package manager) is called `loupe`. 

```
loupe build    # compile to target/debug/
loupe run      # compile and run
loupe release  # produce a standalone escript via rebar3 → target/release/
loupe clean    # wipe target/
```
