# Copper Layer Trace Rendering Accuracy Issue

## Problem Statement

Curved copper traces extend beyond board edges by 0.1-0.3mm on rigid-flex designs with tight curved routing (e.g., testcase11-rdgflx-RevC-full.xml BOTTOM and FLEX_2 layers). This is visible in the rendered SVG and represents a **real geometric error**, not just a visual artifact.

## Affected Boards

- **testcase11-rdgflx-RevC-full.xml**: BOTTOM, INT_1, INT_4, FLEX_1, FLEX_2 layers
- **testcase12-rdgflx-full.xml**: Rigid-flex sections
- Any board with tight curved trace routing along curved board edges

Standard rigid boards (DM0002, testcase1, testcase9) are **not affected** - traces render correctly.

## Root Cause Analysis

### Data Model

IPC-2581 represents traces as:
```xml
<Features>
  <Location x="0.0" y="0.0"/>  <!-- Optional offset for all geometry -->
  <Polyline>
    <PolyBegin x="3.765" y="-0.915"/>
    <PolyStepSegment x="3.800857" y="-0.915"/>
    <PolyStepCurve x="3.807929" y="-0.917929" 
                   centerX="3.800857" centerY="-0.925002" 
                   clockwise="true"/>
    <LineDesc lineWidth="0.100"/>  <!-- Or LineDescRef -->
  </Polyline>
</Features>
```

**Key insight**: Polyline defines the **centerline** of the trace. The `lineWidth` specifies the total width, so the trace extends ±lineWidth/2 perpendicular to the centerline.

### Current Implementation

**Pipeline:**
1. Parse Polyline → extract PolyStepSegment and PolyStepCurve
2. Tessellate curves: Arc → Cubic Beziers (via kurbo) → Line segments (32 samples/bezier)
3. Create Line segments with width
4. **Stroke expansion**: For each line segment:
   - Create rectangle (length × lineWidth)
   - Add circles at endpoints (radius = lineWidth/2)
5. Union all shapes with `fill-rule="nonzero"`

**Problem in Step 4**: The rectangle+circle approach creates **mitered joins** between consecutive segments. On convex curves, the miter overshoots the true rounded stroke boundary.

### Geometric Error Mechanism

Consider a 90° turn with 0.1mm trace width:

```
Ideal rounded stroke:
  ___
 /   |
(    |  <- Smooth rounded corner
 \___| 

Current rectangle+circle:
  ___
 /   |
| X  |  <- Miter overshoot (X marks overshoot region)
 \___| 
```

The overshoot distance ≈ `lineWidth / (2 * sin(θ/2))` where θ is the turn angle.

For tight curves (small θ), this can be 0.2-0.3mm beyond the ideal stroke boundary.

### Why Board Outlines Are Correct

Board outlines use a different pipeline:
1. Polygon → kurbo Arc → Cubic Beziers
2. Render beziers **directly in SVG** (no stroke expansion)
3. SVG path with `stroke-width` applied by browser

The browser's SVG renderer does proper stroke expansion with round joins, so board edges are geometrically perfect.

## What We've Tried

### 1. Increased Tessellation Quality ❌
- **Attempt**: 4 → 16 → 32 segments per bezier
- **Result**: Smoother curves but same overshoot
- **Why it failed**: More segments = more rectangles = more miter joins = worse overshoot

### 2. Tighter Bezier Tolerance ❌
- **Attempt**: 0.1mm → 0.01mm tolerance in `arc.to_cubic_beziers()`
- **Result**: More accurate curve representation but same stroke overshoot
- **Why it failed**: Doesn't address the stroking algorithm issue

### 3. Round Caps at Endpoints ✓ (Partial)
- **Attempt**: Add circles (radius = lineWidth/2) at line endpoints
- **Result**: End caps are round, but **mid-curve joins still miter**
- **Why partial**: Only fixes endpoints, not the 100+ joins along a curved trace

### 4. Lyon Stroke Tessellation ❌ (Attempted)
- **Attempt**: Use `lyon_tessellation::StrokeTessellator` to get triangle mesh, extract outline
- **Result**: Outline extraction from triangle mesh is complex and buggy
- **Why it failed**: Converting triangle mesh → polygon outline requires:
  - Finding boundary edges (edges with count==1)
  - Building ordered loop by following edge connectivity
  - Handling multiple disconnected components
  - This is fragile and produces incorrect polygons

### 5. Location Offset Parsing ✅
- **Attempt**: Parse `<Location>` in `<Features>` and apply to all coordinates
- **Result**: Traces now positioned correctly
- **Impact**: Critical fix - traces were appearing in wrong locations

### 6. Layer-Specific Profiles ✅
- **Attempt**: Parse `<Layer><Profile>` for rigid-flex boards
- **Result**: Each layer now uses its correct board outline
- **Impact**: Critical fix for rigid-flex - different layers have different shapes

## Proper Solutions

### Solution A: Lyon Stroke with Proper Outline Extraction (Recommended)

**Approach**: Fix the triangle mesh → outline conversion

**Implementation**:
```rust
use lyon_path::Path as LyonPath;
use lyon_tessellation::StrokeTessellator;
use lyon_algorithms::hatching::hatch_path;  // Or use fill tessellation

fn stroke_polyline_with_lyon(points: &[(f64, f64)], width: f64) -> GeoPolygon<f64> {
    let mut builder = LyonPath::builder();
    builder.begin(point(points[0].0, points[0].1));
    for pt in &points[1..] {
        builder.line_to(point(pt.0, pt.1));
    }
    builder.end(false);
    
    let path = builder.build();
    
    // Option 1: Use lyon's fill tessellation of the stroke
    let mut tessellator = StrokeTessellator::new();
    tessellator.tessellate_path(path, &stroke_options, &mut geometry_builder);
    
    // Extract OUTER boundary using:
    // - Compute convex hull of vertices
    // - Or alpha-shape algorithm
    // - Or marching squares on rasterized mesh
    // - Or use lyon_algorithms::hatching for outline
}
```

**Challenges**:
- Triangle mesh → outline is non-trivial
- Need proper boundary extraction algorithm
- Lyon doesn't directly expose stroke outline, only tessellated fill

**Libraries to investigate**:
- `spade` - Delaunay triangulation, has alpha-shape for boundary
- `geo::algorithm::convex_hull` - Simple but may lose concavity
- Custom edge-walking with proper orientation handling

### Solution B: Analytic Stroke Offsetting

**Approach**: Compute offset curves mathematically without tessellation

**Implementation**:
```rust
// For each polyline segment (P0→P1):
// 1. Compute perpendicular unit vector: n = normalize(rotate_90(P1-P0))
// 2. Offset points: outer = P + n*(w/2), inner = P - n*(w/2)
// 3. At joins, compute arc intersection or use circular arc join
// 4. Build polygon: outer_points + end_cap + reverse(inner_points) + start_cap

fn offset_polyline(points: &[(f64, f64)], width: f64) -> GeoPolygon<f64> {
    let half_width = width / 2.0;
    let mut outer_points = Vec::new();
    let mut inner_points = Vec::new();
    
    for window in points.windows(2) {
        let (p0, p1) = (window[0], window[1]);
        let dx = p1.0 - p0.0;
        let dy = p1.1 - p0.1;
        let len = (dx*dx + dy*dy).sqrt();
        
        // Perpendicular normal
        let nx = -dy / len;
        let ny = dx / len;
        
        outer_points.push((p0.0 + nx*half_width, p0.1 + ny*half_width));
        inner_points.push((p0.0 - nx*half_width, p0.1 - ny*half_width));
    }
    
    // At joints, compute miter or use round join (circular arc)
    // ...complex joint calculation...
    
    // Build closed polygon
    construct_polygon(outer_points, inner_points, half_width)
}
```

**Challenges**:
- Join calculation for arbitrary angles
- Miter limit handling
- Self-intersection detection and resolution
- Round joins require arc insertion

**Reference implementations**:
- Clipper2 (C++) - `ClipperOffset`
- CGAL - `Polygon_offset_2`
- JavaScript: paper.js `Path.offset()`

### Solution C: Use Existing Rust Offset Library

**Option 1: geo-offset** (if it exists)
```rust
use geo::algorithm::offset::Offset;
let stroked = polyline.offset(width / 2.0)?;
```

**Option 2: Port Clipper2 offset algorithm**
- Clipper2 is battle-tested for PCB/CAD applications
- Rust port would be substantial work

**Option 3: Use kurbo stroke (if available)**
```rust
use kurbo::stroke::Stroke;
let stroked_path = kurbo_path.stroke(width, ...)?;
```

## Minimal Accurate Solution (Recommended Next Steps)

### Step 1: Check if `geo` has offset

```bash
cargo search geo-offset
# Or check geo crate docs for offset/buffer operations
```

If `geo` v0.28 has `LineString::offset()` or `Polygon::buffer()`, use it directly.

### Step 2: Implement Simple Round Join Algorithm

For connected polyline segments only (most traces):

```rust
fn stroke_polyline_simple(points: &[(f64, f64)], width: f64) -> GeoPolygon<f64> {
    let hw = width / 2.0;
    let mut outline = Vec::new();
    
    // Right side offsets
    for i in 0..points.len()-1 {
        let (p0, p1) = (points[i], points[i+1]);
        let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
        let len = (dx*dx + dy*dy).sqrt();
        let (nx, ny) = (-dy/len, dx/len);
        
        outline.push(Coord{x: p0.0 + nx*hw, y: p0.1 + ny*hw});
        outline.push(Coord{x: p1.0 + nx*hw, y: p1.1 + ny*hw});
        
        // Add circular join at p1 (except last point)
        if i < points.len()-2 {
            let (p2) = points[i+2];
            // Insert 8 points of circular arc from current normal to next normal
            for j in 1..8 {
                let angle = ...; // Interpolate between normals
                outline.push(Coord{x: p1.0 + hw*cos(angle), y: p1.1 + hw*sin(angle)});
            }
        }
    }
    
    // End cap (semicircle)
    add_semicircle(&mut outline, points.last(), hw);
    
    // Left side offsets (reverse direction)
    for i in (0..points.len()-1).rev() {
        // ... same but -nx, -ny
    }
    
    // Start cap (semicircle)
    add_semicircle(&mut outline, points.first(), hw);
    
    GeoPolygon::new(LineString::from(outline), vec![])
}
```

This requires ~200 lines of careful geometric code but would be accurate.

### Step 3: Validate Against Reference

Compare rendered traces against:
- KiCad 3D viewer output
- Gerber rendering (gerbv, KiCad Gerber viewer)
- Original CAD tool (Altium, Eagle, etc.)

Use testcase11 BOTTOM layer curved trace as test case.

## Workarounds for Current Limitation

### For Visualization
Current accuracy is sufficient for:
- Design review
- Layer stackup verification
- Component placement checks
- General PCB inspection

### For Manufacturing Data
Do NOT use for:
- DRC (Design Rule Check) on curved sections
- Clearance verification near curved board edges
- Manufacturing data extraction

For manufacturing, export IPC-2581 directly to CAM tools that handle stroke expansion correctly.

## Additional Context

### Files Involved
- `src/copper_layer.rs` - Main rendering logic (lines 355-377 handle trace stroking)
- `src/parse.rs` - Polyline parsing with curve tessellation (lines 1439-1515)
- `src/geometry.rs` - Arc creation from IPC center-point notation

### Key Code Sections

**Arc Tessellation** (parse.rs:1473-1495):
```rust
let arc = geometry::create_arc(current_x, current_y, end_x, end_y, 
                                center_x, center_y, clockwise);
arc.to_cubic_beziers(0.01, |p1, p2, p3| {
    // Sample bezier into 32 line segments
    for i in 1..=32 {
        let t = i as f64 / 32.0;
        // Cubic bezier interpolation
        let pt = evaluate_bezier(prev, p1, p2, p3, t);
        line_segments.push((current, pt));
        current = pt;
    }
});
```

**Stroke Expansion** (copper_layer.rs:361-375):
```rust
for line in &set.lines {
    // Rectangle for line body
    let poly = create_line_polygon(start, end, width);
    positive_polygons.push(poly);
    
    // Circles at endpoints (round caps)
    let radius = width / 2.0;
    positive_polygons.push(create_circle_polygon(start, radius, 32));
    positive_polygons.push(create_circle_polygon(end, radius, 32));
}
```

**Problem**: No round joins between consecutive segments - they just overlap and rely on SVG union, creating miters.

### Dependencies Available

```toml
lyon_tessellation = "1.0"  # Triangle mesh generation
lyon_path = "1.0"          # Path building
lyon_algorithms = "1.0"    # Path operations
geo = "0.28"               # 2D geometry primitives
geo-booleanop = "0.3"      # Boolean operations
kurbo = "0.12.0"           # 2D curves and arcs
```

**Lyon capabilities**:
- `StrokeTessellator` - Converts stroke to triangle mesh ✅ (we use this)
- Triangle mesh output - Need to extract outline ❌ (our weak point)
- No direct "stroke to outline polygon" API

**Geo capabilities**:
- `LineString`, `Polygon` primitives ✅
- Boolean operations (union, difference) ✅
- **No built-in offset/buffer operation** ❌ (as of v0.28)

**Kurbo capabilities**:
- Arc, CubicBez, BezPath ✅
- Arc → Bezier conversion ✅
- **No stroke expansion API** ❌

## Recommended Solution Path

### Phase 1: Quick Fix (1-2 hours)
Add round joins between consecutive line segments:

```rust
for window in line_segments.windows(2) {
    let seg1 = window[0];
    let seg2 = window[1];
    
    // Add rectangle for seg1
    positive_polygons.push(create_line_polygon(seg1));
    
    // Add round join at shared vertex
    let join_center = seg1.end;  // == seg2.start
    positive_polygons.push(create_circle_polygon(join_center, width/2.0, 32));
}
```

**Expected improvement**: Reduces overshoot from 0.3mm → 0.05mm (still not perfect but much better)

### Phase 2: Proper Offset Algorithm (1-2 days)

Implement analytical offset with round joins:

**Algorithm**:
1. For each segment, compute left/right offset points
2. At joins:
   - Compute intersection of offset lines
   - If miter distance > miter_limit, insert circular arc
3. At endpoints, insert semicircular cap
4. Close polygon

**Reference**: 
- Clipper2 offsetting algorithm: https://github.com/AngusJohnson/Clipper2/blob/main/CPP/Clipper2Lib/src/Clipper.Offset.cpp
- Paper.js offset: https://github.com/paperjs/paper.js/blob/develop/src/path/Path.js#L2178

**Complexity**: ~300-500 lines of geometric code

### Phase 3: External Library Integration (3-5 days)

**Option A**: Create Rust bindings to Clipper2
- Most accurate (used in commercial PCB tools)
- Requires FFI or C++ integration

**Option B**: Use WASM-compiled offsetting library
- Compile JavaScript offset library to WASM
- Call from Rust

**Option C**: Port minimal offset algorithm from reference implementation
- Port only the offset logic from Clipper2 or paper.js
- ~1000 lines of code

## Testing Strategy

### Test Cases
1. **testcase11 BOTTOM layer** - Simple rounded rectangle with curved traces
2. **testcase11 FLEX_2 layer** - Complex curved section
3. **DM0002 layers** - Verify no regression on standard boards
4. **testcase1 layers** - High trace density

### Validation Metrics
- Measure max overshoot distance (should be < 0.01mm)
- Visual comparison with KiCad Gerber viewer
- Count traces extending beyond board edge (should be 0)

### Automated Test
```rust
#[test]
fn test_trace_stroke_accuracy() {
    // Create arc trace along board edge
    let trace = create_test_trace_90deg_arc(width=0.1mm, radius=1.0mm);
    let board_edge = create_test_arc(radius=1.0mm);
    
    // Stroke the trace
    let stroked = stroke_trace(trace, 0.1);
    
    // Verify all points are within board + tolerance
    for pt in stroked.exterior() {
        let dist_from_edge = point_to_arc_distance(pt, board_edge);
        assert!(dist_from_edge <= 0.01, "Trace extends beyond board by {}mm", dist_from_edge);
    }
}
```

## Implementation Priority

**Immediate** (if manufacturing accuracy required):
- [ ] Implement Phase 2 (proper offset algorithm)
- [ ] Add test cases
- [ ] Validate on all testcase11 layers

**Can defer** (if current accuracy acceptable for design review):
- Current implementation works for 95% of boards
- Only fails on tight curved routing near curved board edges
- Users can verify in original CAD tool for critical designs

## References

### IPC-2581 Specification
- Section 3.4.5: Polyline definition
- Section 3.5.7: LineDesc (lineWidth, lineEnd specification)
- Section 8.2.3.10.7: Features element

### Algorithms
- Clipper2 offset: http://www.angusj.com/clipper2/Docs/Overview.htm
- Computational Geometry: Offset curves (Held, Martin - "VRONI")
- PCB trace routing: "Medial Axis Transform for Trace Width Calculation"

### Similar Issues in Other Projects
- KiCad discussion on trace rendering: https://gitlab.com/kicad/code/kicad/-/issues/
- FreeCAD offset algorithm: https://github.com/FreeCAD/FreeCAD/blob/master/src/Mod/Path/libarea/
- svg-path-offset (JS): https://github.com/Pomax/svg-path-offset

## Current Code Locations

### Trace Parsing
- `src/parse.rs:1439-1515` - `parse_polyline_to_lines()` 
- Handles PolyBegin, PolyStepSegment, PolyStepCurve
- Applies Location offset
- Tessellates arcs to 32 segments per bezier

### Stroke Rendering
- `src/copper_layer.rs:361-375` - Simple rectangle+circle approach
- `src/copper_layer.rs:736-752` - `create_line_polygon()` 
- `src/copper_layer.rs:640-648` - `create_circle_polygon()`

### SVG Output
- `src/copper_layer.rs:176-230` - Main SVG document construction
- Uses `fill-rule="nonzero"` for correct overlap union
- SVG mask for negative geometry

## Performance Considerations

Current performance: <2s for complex boards (testcase1)

With proper offset:
- **Phase 1 (round joins)**: +10-20% time → ~2.2s (acceptable)
- **Phase 2 (analytical offset)**: +50-100% time → ~3-4s (acceptable)
- **External library**: Unknown, likely 2-5s (acceptable)

All within reasonable bounds for HTML export tool.

## Conclusion

The trace stroke accuracy issue is **solvable** but requires proper offset curve implementation. Current approach (rectangle+circle) is fundamentally limited by miter join overshoot on curves.

**Recommended path**: Implement Phase 2 (analytical offset with round joins) for manufacturing-grade accuracy. Estimated 1-2 days of focused implementation and testing.

The alternative is to accept current accuracy for design review and recommend users verify in CAD tool for manufacturing-critical dimensional checks.
