# gerberx2

`gerberx2` parses and writes Gerber X2 fabrication layers. Parsing yields the
ordered graphical objects with their typed apertures and X2 attributes;
step-and-repeat blocks and block apertures stay hierarchy rather than being
expanded.

The parser supports fixed-format coordinates, flashes, draws, arcs, regions,
step-and-repeat blocks, aperture macros, block apertures, polarity changes, and
file, aperture, and object attributes, and reads the deprecated constructs
older CAD output is full of wherever they leave the image unchanged. The
writer emits the corresponding Gerber constructs without flattening native
macros or block apertures.

This crate contains no CLI or IPC-2581 conversion policy. Higher-level export
and comparison logic belongs in the consuming crate.

Parsing strings, extracting PCB IR artwork, and writing Gerber are available
on `wasm32-unknown-unknown`. Only the native `parse_file` convenience method
requires filesystem access.

Run the tests with:

```bash
cargo test -p gerberx2
```
