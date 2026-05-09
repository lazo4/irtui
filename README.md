# IRTUI

A terminal user interface for the [Neal.fun Internet Roadtrip](https://neal.fun/internet-roadtrip)

## About the roadtrip

If you're new to IRT, please check out the [(un)official guide](https://bit.ly/unofficial-guide)

## Demo

![Demo](demo/demo.gif)

Actual results may vary, according to fonts and terminal support.

## Features/TODO

- [x] Bare bones: pano rendering and vote counts
- [x] Support HiveChat
- [ ] Display the odometer
- [ ] Support honking
- [ ] Support voting (See [note](#a-note-about-voting))
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

## A note about voting

I'm not sure if i'll implement voting. IRT has had quite a few botting incidents (with Bar Harbor being the first and most famous) and the creators have since surrounded it with more and more secure anti-bot facilities. Implementing voting would mean bypassing the anti-bots, **but also** ensuring people can't steal my implementation to make more bots, while keeping the full code open source. It *may* be possible (I'm not sharing my theories yet), and if I do implement it, expect it by the end of summer at the earliest.

## Contributing

Any contributions are welcome, if you have a bug, feature request, or would like to submit more binaries, feel free to open an issue or PR.

## License

This project is licensed under the MIT license ([LICENSE] or <http://opensource.org/licenses/MIT>)

[LICENSE]: ./LICENSE
