# Super Simple Defaults

A small native desktop app that sets, for you, which application opens each file type.

![Super Simple Defaults](screenshot.jpeg)

## Install

Add this repository as a flake input:

```nix
super-simple-defaults.url = "github:MiPSR/super-simple-defaults";
```

Then use the package, for example in `environment.systemPackages`:

```nix
inputs.super-simple-defaults.packages.x86_64-linux.default
```

## Run

```sh
nix run github:MiPSR/super-simple-defaults
```

If you ran it before and want the newest version:

```sh
nix run --refresh github:MiPSR/super-simple-defaults
```

## Development

```sh
nix develop
cargo test
```

## AI

This app is mostly written by the AI tool [OpenCode](https://github.com/anomalyco/opencode), with minor human changes.

## Contributing

Contributions must say which agent and tool were used.
