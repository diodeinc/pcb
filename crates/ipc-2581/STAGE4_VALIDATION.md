# Stage 4: Visual Validation Guide

## Quick Validation Commands

### Export Stage 3 (Pre-Boolean, Individual Features)
```bash
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers F.Cu \
  --debug-stage3 /tmp/debug_stage3.svg
```

### Export Stage 4 (Post-Boolean, Unified Paths)
```bash
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers F.Cu \
  --debug-stage4 /tmp/debug_stage4.svg
```

### Export Multi-Layer
```bash
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers "F.Cu,B.Cu" \
  --debug-stage4 /tmp/multi_layer.svg
# Creates: /tmp/multi_layer_F.Cu.svg and /tmp/multi_layer_B.Cu.svg
```

### Export Individual Bucket
```bash
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers F.Cu \
  --debug-bucket Trace
# Creates: debug_F.Cu_Trace.svg in current directory
```

## Visual Validation Checklist

### ✅ Basic Sanity (Stage 3)

Open `/tmp/debug_stage3.svg` and verify:

- [ ] **Pads appear** - Orange (SMD), Green (PTH), Blue (Via)
- [ ] **Traces visible** - Red-orange lines connecting pads
- [ ] **Fills present** - Large lime green copper pours
- [ ] **Board dimensions** - ~14.4mm × 38.0mm for DM0002 F.Cu
- [ ] **No missing features** - 643 features for F.Cu

### ✅ Boolean Operations (Stage 4)

Open `/tmp/debug_stage4.svg` and compare with Stage 3:

- [ ] **Overlapping pads merged** - Single unified path, no duplicate edges
- [ ] **Traces connected** - Multiple segments joined where they touch
- [ ] **Fills consolidated** - Large pours are single paths
- [ ] **Cleaner geometry** - Fewer vertices (643 features → 5 paths)
- [ ] **No artifacts** - No slivers, gaps, or weird spikes

### ✅ Transform Correctness

Check rotation/mirror by comparing F.Cu and B.Cu:

```bash
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers "F.Cu,B.Cu" \
  --debug-stage4 /tmp/multi_layer.svg

open /tmp/multi_layer_F.Cu.svg /tmp/multi_layer_B.Cu.svg
```

- [ ] **B.Cu mirrored** - Features should be flipped horizontally
- [ ] **Vias align** - Via positions same on both layers
- [ ] **Different features** - B.Cu has fewer features (82 vs 643)

### ✅ Geometry Accuracy

Export individual buckets to inspect details:

```bash
# Export traces only
cargo run --bin ipc2581 -- export-svg test.xml --debug-bucket Trace

# Export SMD pads only
cargo run --bin ipc2581 -- export-svg test.xml --debug-bucket Smd

# Export vias only
cargo run --bin ipc2581 -- export-svg test.xml --debug-bucket Via
```

Verify:
- [ ] **Circular pads are round** - Not badly polygonized
- [ ] **Rectangular pads have sharp corners** - Not rounded unless RectRound
- [ ] **Thermal reliefs show spokes** - Donut shape with radial gaps
- [ ] **Trace widths consistent** - No thin/thick variations within same net

## Color Legend

| Color | Bucket | Opacity | Description |
|-------|--------|---------|-------------|
| 🟢 Lime | Fill | 0.6 | Copper pours, planes |
| 🟠 Red-Orange | Trace | 0.8 | PCB traces |
| 🟠 Orange | Smd | 0.9 | SMD pads |
| 🟢 Green | Pth | 0.9 | Through-hole pads |
| 🔵 Blue | Via | 0.9 | Vias |
| 🟡 Gold | Thermal | 0.7 | Thermal reliefs |

## Expected Results for DM0002-IPC-2518.xml

### F.Cu (Front Copper)

**Stage 3 Statistics:**
- Total features: 643
- SMD pads: 204
- PTH pads: 10
- Vias: 60
- Traces: 362
- Fills: 7

**Stage 4 Statistics:**
- Buckets: 5 (Fill, Trace, Smd, Pth, Via)
- Total vertices: ~10,000
- Fill area: ~547 mm²
- Trace area: ~406 mm²
- SMD area: ~389 mm²

**Visual Expectations:**
- Dense routing with many fine traces
- Large ground pour (lime green) covering most of board
- SMD pads (orange) concentrated in specific areas
- 60 blue via circles distributed across board

### B.Cu (Back Copper)

**Stage 3 Statistics:**
- Total features: 82
- SMD pads: 7
- PTH pads: 10
- Vias: 60
- Traces: 4
- Fills: 1

**Visual Expectations:**
- Much sparser than F.Cu
- Single large ground pour
- Minimal routing (4 traces only)
- Same via positions as F.Cu

## Debugging Common Issues

### Issue: Pads not visible

**Symptoms:** Open SVG, see nothing but black background

**Causes:**
- Empty paths (all features filtered out)
- Bounding box calculation wrong
- ViewBox doesn't match content

**Debug:**
```bash
# Check console output for feature counts
cargo run --bin ipc2581 -- export-svg test.xml --debug-stage3 /tmp/debug.svg

# Should see: "✓ Exported debug SVG to /tmp/debug.svg"
#             "  643 features, 14.40×38.00mm"
```

### Issue: Geometry looks wrong

**Symptoms:** Pads in wrong places, rotated incorrectly

**Causes:**
- Stage 1 transform application bug
- Coordinate units not converted properly

**Debug:**
```bash
# Export Stage 1 debug info to inspect transforms
cargo run --bin ipc2581 -- export-svg test.xml --dump-stage1 /tmp/stage1.txt

# Check for transform values in output
grep "rotation\|mirror\|scale" /tmp/stage1.txt
```

### Issue: Boolean ops failed

**Symptoms:** Warnings in console: "Union failed, keeping partial result"

**Causes:**
- Degenerate geometry (zero-area paths)
- Self-intersecting paths
- Floating point precision issues

**Debug:**
- Check if coordinate snapping helped (should reduce failures)
- Export Stage 3 to see problematic features
- Inspect features with very small bounding boxes

## Performance Benchmarks

### Expected Timing (DM0002 test file)

| Stage | Operation | Time | Notes |
|-------|-----------|------|-------|
| 0 | Input readiness | ~5ms | Parse + dictionaries |
| 1 | Transform resolution | ~20ms | Flatten hierarchy |
| 2 | Padstack expansion | ~30ms | Expand 940 pads |
| 3 | Path conversion | ~65ms | Create Skia paths |
| 4 | Boolean flattening | ~440ms | Union + difference ops |
| **Total** | **End-to-end** | **~560ms** | All stages |

**Stage 4 Breakdown (F.Cu):**
- Fill union: 12ms (7 features → 1 path)
- Trace union: 141ms (362 features → 1 path)
- SMD union: 78ms (204 features → 1 path)
- Via union: 3ms (60 features → 1 path)

**Vertex Reduction:**
- Input (Stage 3): ~643 paths, ~5000 total vertices
- Output (Stage 4): 5 paths, ~10,000 vertices (boolean ops create intermediate vertices)

## Advanced Validation

### Compare Stage 3 vs Stage 4

Open both side-by-side to verify boolean ops didn't lose features:

```bash
cargo run --bin ipc2581 -- export-svg test.xml \
  --layers F.Cu \
  --debug-stage3 /tmp/s3.svg \
  --debug-stage4 /tmp/s4.svg

open /tmp/s3.svg /tmp/s4.svg
```

**Expected differences:**
- Stage 3: Individual features visible with slight overlaps
- Stage 4: Cleaner, unified geometry (overlaps resolved)
- Same overall shape and area

### Validate Negative Features

For boards with antipads/cutouts:

```bash
# Look for difference operations in console
cargo run --bin ipc2581 -- export-svg test.xml | grep "Difference time"
```

Should see messages like:
```
Fill: 10 positive, 5 negative
Difference time: 8ms
```

In SVG, negative features should appear as holes in copper.

## Known Limitations

1. **Area calculation is approximate** - Uses `tight_bounds()` instead of true polygon area
   - Overestimates by ~5-10% for complex shapes
   - Good enough for validation, not for manufacturing specs

2. **Coordinate snapping** - 1 micron grid may cause sub-micron shifts
   - Well below manufacturing tolerance (typically 25-50 microns)
   - Trade-off: accuracy vs boolean op reliability

3. **Conic curves** - Approximated as quadratic in SVG export
   - Visual difference minimal
   - Actual paths in Skia are accurate

## Success Criteria

Stage 4 is validated if:

✅ All test files parse and export without errors
✅ Visual inspection shows correct geometry
✅ Boolean operations complete without excessive failures (<1%)
✅ Performance acceptable (<1 second for typical boards)
✅ Statistics reasonable (area, vertex counts, timings)

## Next: Implement Stages 5-6

After validation passes, continue with:

- **Stage 5**: Apply production styling (proper colors, stroke widths)
- **Stage 6**: Generate final SVG with layers, metadata, legends
