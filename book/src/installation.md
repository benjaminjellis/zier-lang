# Installation

Currently the only way to install `Opal` is from the source

`Opal`'s compiler is written in Rust, to install it you'll need a `Rust` toolchain, to run `Opal` code you'll need to install `erlang` and to create a release you'll need `rebar3`

To install everything on arch linux run the following

```
sudo pacman -S rustup erlang rebar3
```

You should be able to do something similar on macOS with

```
brew install rustup erlang rebar3
```

Once you have those installed you can clone this repo and in the root run
```

cargo install --path loupe
```

Then you'll be able to use `Loupe`, `Opal`'s build tool.


