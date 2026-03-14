# Installation

Currently, the only way to install `Mond` is from source.

`Mond`'s compiler is written in Rust. To install it, you'll need a `Rust` toolchain. To run `Mond` code, you'll need to install `erlang`, and to create a release you'll need `rebar3`.

To install everything on Arch Linux, run the following:

```
sudo pacman -S rustup erlang rebar3
```

You should be able to do something similar on macOS with:

```
brew install rustup erlang rebar3
```

Once you have those installed, you have two options:

Clone and install

```
git clone git@github.com:benjaminjellis/mond.git 
cd mond
cargo install --path bahn 
```

Or without cloning

```
cargo install --git https://github.com/benjaminjellis/mond.git --tag 0.0.1 bahn
```


To verify installation
```

bahn --help
```

And when you see something like below you should be all set

```shell
the build tool for the mond programming language

Usage: bahn <COMMAND>

Commands:
  run
  test
  deps
  lsp
  format
  new
  build
  release
  clean
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```
