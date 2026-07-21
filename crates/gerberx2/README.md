# gerberx2

`gerberx2` parses and writes Gerber X2 fabrication layers. It preserves the
ordered command stream and X2 attributes while exposing typed apertures,
graphics state, and graphical objects.

The parser supports fixed-format coordinates, flashes, draws, arcs, regions,
step-and-repeat blocks, aperture macros, block apertures, polarity changes, and
file, aperture, and object attributes. The writer emits the corresponding
Gerber constructs without flattening native macros or block apertures.

This crate contains no CLI or IPC-2581 conversion policy. Higher-level export
and comparison logic belongs in the consuming crate.

Run the tests with:

```bash
cargo test -p gerberx2
```
