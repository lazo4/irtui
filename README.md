# IRTUI

A terminal user interface for the [Neal.fun Internet Roadtrip](https://neal.fun/internet-roadtrip)

## About the roadtrip

If you're new to IRT, please check out the [(un)official guide](https://bit.ly/unofficial-guide)

## Screenshots

![Demo](demo/demo.gif)

Ngl, these screenshots are optimistic. I ran them on macos, inside the vscode terminal, but terminal support and fonts vary wildly, so the image may display differently and and be kinda glitchy. Sadly, I haven't found a way to get chafa working on windows, so for now windows users will just see the image rendered with halfblocks.

## Features/TODO

- [x] Bare bones: pano rendering and vote counts
- [ ] Support honking
- [ ] Support HiveChat
- [ ] Support voting
- [ ] Display the odometer
- [ ] Display the minimap
- [ ] Display and play the radio
- [ ] Add a link to the main site and to the discord
- [ ] Maybe support [custom glyphs](https://rapha.land/introducing-glyph-protocol-for-terminals/), for the vote options icons

## Quickstart

On macOS, you'll have to install chafa first:
```zsh
brew install chafa
```
You can download prebuild binaries for macos, linux and windows in the [releases section](https://github.com/lazo4/irtui/releases). Once you downloaded the right one, just put it in the install directory of your choice! (and maybe rename it to just `irtui`)

Now just run it with:
```bash
irtui
```

## Build from source

If your platform isn't available, or if you'd like to run the HEAD version, you can build from source.

### Prerequisites
You'll need:
- [Rust](rustup.rs)
- Chafa:
  Macos: `brew install chafa`
  Linux: `sudo apt install libchafa-dev libglib2.0-dev`
- Pkg-Config: only for linux
- [CMake](https://cmake.org/download/)

### Features
You'll have to choose a way of linking chafa, based on your platform:
- `chafa-dyn`: Dynamically link to libchafa, supported on macos and linux
- `chafa-static`: Statically link to libchafa, only supported on linux, requires `libsysprof-capture-4-dev`

If no features are specified, chafa won't be used, and the image will be rendered with halfblocks.

### Compiling
Run:
```
cargo build --release --features <build-features>
```
The binary is now in `target/release/irtui`

## Contributing

Any contributions are welcome, if you have a bug, feature request, or would like to submit more binaries, feel free to open an issue or PR.

## License

This project is licensed under the MIT license ([LICENSE] or <http://opensource.org/licenses/MIT>)

[LICENSE]: ./LICENSE
