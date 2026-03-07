# Loupe
`Loupe` is `Opal`'s build tool. It seeks to behave just like `Cargo` does for `Rust`. To get started simply run

```shell
loupe new hello_world
```

This will create a new directory `hello_world`, you can then run 

```shell
cd hello_world
loupe run
```

And you should see "Hello World" printed to stdout

If you look in `src/main.opal`, you'll see this:

```
(use std)

(let main {}
  (io/println "Hello, world!"))
```

In the next section we'll go through the language and see what all of this means
