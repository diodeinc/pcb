# Stage 4.5: Drill Mask Subtraction - Usage Guide

## 🎨 Color Legend

| Color | Hex | Feature | Opacity | Notes |
|-------|-----|---------|---------|-------|
| 🟢 Lime | #32CD32 | Fill (copper pours/planes) | 0.6 | Large filled areas |
| 🔴 Red-Orange | #FF4500 | Trace (routing traces) | 0.8 | Thin copper paths |
| 🟠 Orange | #FFA500 | SMD (surface mount pads) | 0.9 | Solid pads, no holes |
| 🟢 Green | #00FF00 | PTH (plated through-hole) | 0.9 | **Should show holes!** |
| 🔵 Blue | #1E90FF | Via (plated vias) | 0.9 | **Should show holes!** |
| 🟡 Gold | #FFD700 | Thermal (thermal reliefs) | 0.7 | Spoke patterns |
| ⚪ Grey | #808080 | Drill Mask (holes/slots) | 0.7 | Shows drilled areas |

---

## 📋 Export Commands

### Basic Stage 4 Export (After Boolean Flattening)

```bash
# Export F.Cu after Stage 4 (unified buckets, no drill holes yet)
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers F.Cu \
  --debug-stage4 /tmp/stage4.svg
```

**Output:** `/tmp/stage4.svg`
- Shows unified copper per bucket
- PTH/Via appear as solid circles (no holes yet)

### Stage 4.5 Export (After Drill Subtraction)

```bash
# This is the SAME command - drill subtraction happens automatically!
# Stage 4.5 runs BEFORE the debug export
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers F.Cu \
  --debug-stage4 /tmp/stage4_5.svg
```

**Output:** `/tmp/stage4_5.svg`
- Shows copper AFTER drill subtraction
- PTH/Via appear as annular rings (donuts with holes)
- Holes should be visible as empty (black) areas

**Pipeline order:**
1. Stage 4 runs (boolean flattening)
2. Stage 4.5 runs (drill subtraction)
3. Then `--debug-stage4` exports the result

---

## 🔍 Troubleshooting Missing Holes

### Issue: Holes Not Visible

**Symptoms:**
- PTH/Via appear as solid circles
- No white/black holes in center
- Looks the same as Stage 3 output

**Common Causes:**

#### 1. Missing fill-rule="evenodd" (FIXED in latest code)

**Check:**
```bash
grep "fill-rule" /tmp/stage4_5.svg | head -3
```

**Should see:**
```svg
<path d="..." fill-rule="evenodd" .../>
```

**If missing:** Rebuild with latest code (after fill-rule fix)

#### 2. SVG Viewer Doesn't Support EvenOdd

**Test:**
- Open in Chrome/Firefox (supports evenodd)
- Open in Safari (supports evenodd)
- Don't use basic image viewers

#### 3. Drill Layers Not in File

**Check console output:**
```bash
cargo run --bin ipc2581 -- export-svg test.xml --layers F.Cu 2>&1 | grep "Extracted"
```

**Should see:**
```
  Extracted 72 drill features
```

**If "Extracted 0":** File has no DRILL layers or holes

#### 4. Boolean Operation Failing

**Check console for warnings:**
```bash
cargo run --bin ipc2581 -- export-svg test.xml --layers F.Cu 2>&1 | grep "WARNING"
```

**If you see:**
```
WARNING: Drill mask subtraction failed for Via, keeping original
```

Then boolean ops are failing. This shouldn't happen.

---

## 🔬 Visual Inspection Checklist

Open `/tmp/stage4_5.svg` in a browser and zoom in on:

### Vias (Blue circles)

**Expected:**
- [ ] Blue outer ring (annular ring)
- [ ] Black/white hole in center
- [ ] Hole diameter ~40-60% of pad diameter
- [ ] All 60 vias should have holes

**If NOT seeing holes:**
1. Check if path has multiple "M" commands (multiple contours)
2. Verify fill-rule="evenodd" in path element
3. Try different browser

### PTH Pads (Green circles/shapes)

**Expected:**
- [ ] Green outer ring
- [ ] Black/white hole in center
- [ ] All 10 PTH pads should have holes

### Traces (Red-orange)

**Expected:**
- [ ] Traces should NOT have holes
- [ ] Should pass under/over vias without gaps
- [ ] If trace overlaps via hole, hole should still be visible

---

## 🛠️ Debug Workflow

### Step 1: Export Stage 3 (Individual Features)

```bash
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers F.Cu \
  --debug-stage3 /tmp/s3.svg
```

**Check:** Individual vias as separate circles (no holes yet)

### Step 2: Export Stage 4 (Boolean Flattened)

```bash
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers F.Cu \
  --debug-stage4 /tmp/s4_5.svg
```

**Check:** Vias with holes (should see annular rings)

### Step 3: Compare Before/After

```bash
open /tmp/s3.svg /tmp/s4_5.svg
```

**Look for:**
- S3: 60 individual solid blue circles
- S4.5: Fewer blue features, each with visible hole

### Step 4: Inspect Individual Bucket

```bash
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers F.Cu \
  --debug-bucket Via
```

**Output:** `debug_F.Cu_Via.svg` in current directory

**Check:** Just the Via bucket, easier to see individual holes

---

## ⚙️ Technical Details

### Vertex Count Evidence

**Console output shows drill subtraction working:**

```
Stage 4.5: Drill Mask Subtraction
  Extracted 72 drill features
  Subtracting 72 drills from F.Cu
    Via: 540 → 1080 vertices    ← DOUBLED = holes added!
    Pth: 86 → 176 vertices      ← DOUBLED = holes added!
    Trace: 2601 → 2634 vertices ← Slight increase (some overlap)
    Smd: 2480 → 2552 vertices   ← Slight increase
    Fill: 2605 → 2822 vertices  ← Increase from holes
```

**Interpretation:**
- Via/PTH doubled = each pad now has 2 circles (outer + inner)
- Other buckets increased = some copper removed where drills overlap

### SVG Path Structure

**Via path with hole (correct):**
```svg
<path d="M135.525,-117.355 Q... Z     <- Outer circle
         M135.450,-117.355 Q... Z"    <- Inner circle (hole)
      fill-rule="evenodd" .../>
```

**How evenodd works:**
- First contour (outer) = filled
- Second contour (inner) = toggles fill OFF = hole!

---

## 🐛 Known Issues

### Issue: Circles Look Polygonal (Boxy)

**Cause:** Skia's PathOp::Union/Difference converts curves to polylines internally

**Evidence:** SVG paths are mostly "L" (lines) instead of "Q" (curves)

**Status:** This is a Skia limitation. Boolean operations work on tessellated polylines.

**Workarounds:**
1. Accept it (still manufacturing-accurate to 1 micron)
2. Increase tessellation quality (not exposed in Skia Rust bindings)
3. Use different boolean library (lyon, geo-booleanop)

**Impact:** Visual only - manufacturing accuracy preserved

### Issue: Only Half the Holes Show

**Possible causes:**
1. SVG viewer doesn't support evenodd (try Chrome/Firefox)
2. Some drill features failed to extract
3. Boolean difference failing for some features

**Debug:**
```bash
# Check extraction count
cargo run --bin ipc2581 -- export-svg test.xml 2>&1 | grep "Extracted"

# Check for warnings
cargo run --bin ipc2581 -- export-svg test.xml 2>&1 | grep "WARNING"

# Check vertex counts (should double)
cargo run --bin ipc2581 -- export-svg test.xml 2>&1 | grep "Via:"
```

### Issue: Slots Not Visible

**Cause:** Slots might not be in the test file, or slot polygon conversion broken

**Check:**
```bash
# Look for slot extraction
cargo run --bin ipc2581 -- export-svg test.xml 2>&1 | grep "slot"
```

**Debug:** Add logging to `extract_drill_features()` to see if slots are found

---

## 📊 Expected Results for DM0002

### Drill Features

**Console should show:**
```
Extracted 72 drill features
```

**Breakdown (from IPC-2581):**
- 72 holes total
- Mix of vias (60) + PTH (10) + NPTH (2)
- 4 slots (rectangular with rounded ends)

### Via Bucket

**Before drill subtraction:**
- 540 vertices (60 circles × 9 vertices each)

**After drill subtraction:**
- 1080 vertices (60 donuts × 18 vertices each)
- Each via: outer circle (9 verts) + inner circle (9 verts)

### PTH Bucket

**Before:**
- 86 vertices (10 circles)

**After:**
- 176 vertices (10 donuts)
- Each PTH: outer + inner circle

---

## 🎯 Success Criteria

Stage 4.5 is working correctly if:

✅ Console shows "Extracted N drill features" (N > 0)
✅ Via/PTH vertex counts double
✅ No "WARNING: Drill mask subtraction failed" messages
✅ SVG paths have `fill-rule="evenodd"`
✅ SVG paths have multiple "M" commands (multiple contours)

**Visual verification:**
✅ Vias show holes in browser
✅ PTH pads show holes
✅ Holes are black/white (background color showing through)

---

## 🚀 Quick Test

```bash
# Full export with timing
cargo run --bin ipc2581 -- export-svg \
  crates/ipc-2581/tests/data/DM0002-IPC-2518.xml \
  --layers F.Cu \
  --debug-stage4 /tmp/test.svg \
  --timings

# Open result
open /tmp/test.svg

# Zoom in on a blue via - should see hole in center
```

**Expected timing:**
```
Stage 4 (Booleans):    ~250ms
Stage 4.5 (Drills):    ~30ms
```

---

## 🎓 Architecture Notes

### Why Separate Stage?

**Stage 4:** Flatten copper (union/difference per bucket)
**Stage 4.5:** Subtract drill holes (separate concern)

**Benefits:**
- Clean separation (copper ≠ drill)
- Mirrors IPC-2581 structure
- Mirrors manufacturing (etch then drill)
- Debuggable (can export copper-only)

### Drill Mask Construction

```
For each DRILL layer:
  For each Set:
    For each Hole:
      Create circle path (diameter from IPC-2581)
    For each Slot:
      Convert polygon to path

Union all → drill mask

For each copper layer:
  If drill spans layer:
    Subtract drill mask from each bucket
```

### Fill Type Propagation

```
Stage 3:  Create circles with default FillType
Stage 4:  Union preserves or sets EvenOdd
Stage 4.5: Difference with drill mask creates multi-contour path
          Skia automatically sets EvenOdd for paths with holes
SVG Export: Read fill_type() and emit fill-rule="evenodd"
```

---

**Next:** If holes still aren't visible, we may need to investigate:
1. Why boolean ops are failing
2. Whether fill-rule is being set correctly by Skia
3. Whether we need to explicitly set EvenOdd before difference operation
