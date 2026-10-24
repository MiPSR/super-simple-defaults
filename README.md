# Super Simple Defaults

A small native Wayland app to set default applications for the current user.

It only changes user-level defaults (via `mimeapps.list`), never system-wide settings.

## Run

```sh
nix run github:MiPSR/super-simple-defaults
```

Optional refresh:

```sh
nix run --refresh github:MiPSR/super-simple-defaults
```

![Preview](preview.jpeg)