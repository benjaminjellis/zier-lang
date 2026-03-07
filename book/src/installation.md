# Installation

Currently the only way to install Opal is from the source

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


