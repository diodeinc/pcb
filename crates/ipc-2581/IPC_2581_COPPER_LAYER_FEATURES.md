# IPC-2581 Copper Layer Features: Comprehensive Technical Reference

## Table of Contents

1. [Overview & Design Philosophy](#overview--design-philosophy)
2. [Core Concepts](#core-concepts)
3. [Hierarchical Structure](#hierarchical-structure)
4. [Trace Definition (Polyline)](#trace-definition-polyline)
5. [Pad Definition](#pad-definition)
6. [Filled Areas (Copper Pours)](#filled-areas-copper-pours)
7. [Standard Primitives](#standard-primitives)
8. [Hollow Shapes & Cutouts](#hollow-shapes--cutouts)
9. [Line Descriptors](#line-descriptors)
10. [Curves and Arcs](#curves-and-arcs)
11. [Dictionary System](#dictionary-system)
12. [Real-World Examples](#real-world-examples)
13. [Semantic Motivation](#semantic-motivation)
14. [Edge Cases & Complex Scenarios](#edge-cases--complex-scenarios)

---

## Overview & Design Philosophy

IPC-2581 is an XML-based, open standard for communicating electronics manufacturing information. The format is designed to replace legacy Gerber, ODB++, and proprietary formats with a modern, extensible data exchange standard.

### Key Design Goals

1. **Manufacturing Accuracy**: Represent PCB designs with sufficient fidelity for fabrication and assembly
2. **Vendor Neutrality**: No ties to specific CAD tools or manufacturers
3. **Data Efficiency**: Reuse common patterns through dictionary references
4. **Hierarchical Organization**: Group related features for efficient processing
5. **Extensibility**: Support complex designs (rigid-flex, HDI, embedded components)

### Why Copper Layers Matter

Copper layers define the electrical connectivity of the PCB. IPC-2581 represents copper features as:

- **Traces** (Polyline): Conductive paths connecting pads
- **Pads**: Landing areas for component pins or vias
- **Pours**: Large filled copper areas (planes, shields)
- **Shapes**: Geometric primitives (circles, rectangles, polygons)

The specification uses a **centerline representation** for traces (Polyline with lineWidth) rather than polygon outlines, which more accurately reflects how manufacturing tools (plotters, routers) create traces.

---

## Core Concepts

### Coordinate System

IPC-2581 uses a standard Cartesian coordinate system:

- **X-axis**: Positive values go left→right (west→east)
- **Y-axis**: Positive values go bottom→top (south→north)
- **Origin**: (0, 0) at the board datum point
- **Units**: Specified globally in `<CadHeader>` or per-dictionary (MILLIMETER, INCH, MILS, MICRON)
- **Precision**: IEEE double-precision floating point (typically 6+ decimal places)

```
Y (positive = north)
↑
|
|      +---------------+
|      |   PCB Board   |
|      |     TOP side  |
|      +---------------+
|
+-------------------------→ X (positive = east)
(0,0)
```

### Polarity

Layers have polarity that determines how features are interpreted:

- **POSITIVE**: Features add copper (normal traces, pads, pours)
- **NEGATIVE**: Features remove copper (rarely used for signal layers)

### Location Offsets

Every `<Features>` block contains a `<Location>` element that offsets all child geometry:

```xml
<Features>
  <Location x="10.0" y="5.0"/>  <!-- All coordinates offset by (10, 5) -->
  <Polyline>
    <PolyBegin x="1.0" y="2.0"/>  <!-- Actual position: (11.0, 7.0) -->
    ...
  </Polyline>
</Features>
```

In practice, most test files use `<Location x="0.0" y="0.0"/>` and provide absolute coordinates.

### Transformation (Xform)

The `<Xform>` element applies transformations in this order:

1. **Offset**: xOffset, yOffset (move origin)
2. **Rotation**: degrees, counter-clockwise (from TOP view)
3. **Mirror**: flip across Y-axis (x → -x)
4. **Scale**: multiply all dimensions

---

## Hierarchical Structure

Copper layer features follow a strict 5-level hierarchy:

```
Layer (CONDUCTOR)
  └── LayerFeature
        └── Set (grouped by net or type)
              └── Features (collection)
                    └── Geometry (Polyline, Pad, Outline, etc.)
```

### Level 1: Layer Definition

```xml
<Layer name="TOP" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
<Layer name="BOTTOM" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
<Layer name="GND" layerFunction="CONDUCTOR" side="INTERNAL" polarity="POSITIVE"/>
```

**Attributes:**
- `name`: Unique layer identifier
- `layerFunction="CONDUCTOR"`: Defines this as a copper layer
- `side`: TOP, BOTTOM, INTERNAL, or NONE
- `polarity`: POSITIVE (add copper) or NEGATIVE (remove copper)

### Level 2: LayerFeature

Links features to a specific layer:

```xml
<LayerFeature layerRef="TOP">
  <!-- Multiple Set elements -->
</LayerFeature>
```

### Level 3: Set

Groups related features, typically by net name:

```xml
<Set net="VCC">
  <!-- All VCC net features on this layer -->
</Set>

<Set net="GND" polarity="POSITIVE">
  <!-- All GND net features -->
</Set>

<Set>  <!-- No net = non-electrical features (silkscreen, etc.) -->
  <ColorRef id="COLOR_TOP"/>
</Set>
```

**Attributes:**
- `net`: Net name (electrical grouping)
- `polarity`: Override layer polarity for this set
- `geometryUsage`: THIEVING, THERMAL_RELIEF, TEXT, TEARDROP, GRAPHIC, NONE
- `testPoint`, `plate`, `padUsage`: For manufacturing specs

### Level 4: Features

Container for one or more geometric shapes:

```xml
<Set net="RESET">
  <Features>
    <Location x="0.0" y="0.0"/>
    <!-- Polyline or other geometry -->
  </Features>
  <Features>
    <Location x="5.0" y="10.0"/>
    <!-- Another Polyline -->
  </Features>
</Set>
```

### Level 5: Geometry

The actual shapes: `<Polyline>`, `<Pad>`, `<Outline>`, `<Line>`, `<Arc>`, `<Circle>`, etc.

---

## Trace Definition (Polyline)

Traces are represented as **centerline paths** with a specified width, not as polygon outlines.

### Basic Structure

```xml
<Polyline>
  <PolyBegin x="10.0" y="20.0"/>       <!-- Start point -->
  <PolyStepSegment x="15.0" y="20.0"/> <!-- Straight line to (15, 20) -->
  <PolyStepSegment x="15.0" y="25.0"/> <!-- Straight line to (15, 25) -->
  <LineDescRef id="ROUND_0.15"/>       <!-- Width: 0.15mm, round ends -->
</Polyline>
```

**Rendering Interpretation:**
- The Polyline defines the **centerline** of the trace
- `lineWidth` (from LineDesc) defines the total trace width
- The trace extends ±(lineWidth/2) perpendicular to the centerline
- `lineEnd="ROUND"` adds semicircular caps at endpoints

### Real Example: Simple Two-Point Trace

```xml
<Set net="UN30LED_QTLP680C203PRED10">
  <Features>
    <Location x="0.0" y="0.0"/>
    <Polyline>
      <PolyBegin x="-0.4125" y="1.1250"/>
      <PolyStepSegment x="-0.4175" y="1.1200"/>
      <PolyStepSegment x="-0.4175" y="1.0630"/>
      <LineDescRef id="ROUND_71"/>  <!-- 0.0071" = 0.18mm -->
    </Polyline>
  </Features>
</Set>
```

**Interpretation:**
- Net: UN30LED_QTLP680C203PRED10
- Path: (-0.4125, 1.1250) → (-0.4175, 1.1200) → (-0.4175, 1.0630)
- Width: 0.0071 inches (0.18mm)
- Total trace length: ~0.063 inches

### Multi-Segment Trace

```xml
<Polyline>
  <PolyBegin x="80.3700" y="91.4480"/>
  <PolyStepSegment x="80.3870" y="91.4480"/>
  <PolyStepSegment x="80.7720" y="91.0630"/>
  <PolyStepSegment x="80.7720" y="89.4080"/>
  <PolyStepSegment x="81.9750" y="88.2050"/>
  <PolyStepSegment x="81.9750" y="88.1780"/>
  <LineDescRef id="ROUND_1570"/>  <!-- 0.157mm -->
</Polyline>
```

This creates a 45° diagonal segment followed by vertical and diagonal paths.

### Why Centerline?

Manufacturing tools (plotters, routers) follow paths at specified widths. Representing traces as centerlines:

1. Matches CAM tool expectations
2. Simplifies width changes (just update LineDesc reference)
3. Reduces file size (fewer coordinates)
4. Accurately represents design intent

---

## Pad Definition

Pads are landing areas for component pins or vias. They reference predefined padstack definitions and shapes.

### Basic Pad Structure

```xml
<Pad padstackDefRef="LP033RND070_ICT">
  <Location x="-0.4125" y="1.1250"/>
  <StandardPrimitiveRef id="CIRCLE_1"/>
</Pad>
```

**Components:**
- `padstackDefRef`: References a `<PadStackDef>` that defines per-layer pad shapes
- `<Location>`: X, Y position of pad center
- `<StandardPrimitiveRef>`: Shape definition (often a Circle or RectCenter)

### Real Example: Via Pad

```xml
<Set net="UN30LED_QTLP680C203PRED10" testPoint="false" plate="true" padUsage="VIA">
  <NonstandardAttribute name="PADSTACK_USAGE" value="Through" type="STRING"/>
  <Pad padstackDefRef="LP033RND070_ICT">
    <Location x="-0.4125" y="1.1250"/>
    <StandardPrimitiveRef id="CIRCLE_1"/>
  </Pad>
</Set>
```

**Interpretation:**
- Net: UN30LED_QTLP680C203PRED10
- Type: Plated through-hole via
- Position: (-0.4125", 1.1250")
- Shape: CIRCLE_1 (defined in DictionaryStandard)

### Pad Shape Reference

Pad shapes are typically defined in `<DictionaryStandard>`:

```xml
<DictionaryStandard units="INCH">
  <EntryStandard id="CIRCLE_1">
    <Circle diameter="0.0276">
      <FillDescRef id="SOLID_FILL"/>
    </Circle>
  </EntryStandard>
</DictionaryStandard>
```

### SMD Pad (Rectangular)

```xml
<Pad>
  <Location x="5.0" y="10.0"/>
  <RectCenter width="1.5" height="2.0">
    <FillDescRef id="SOLID_FILL"/>
  </RectCenter>
</Pad>
```

This creates a 1.5mm × 2.0mm rectangular pad centered at (5.0, 10.0).

### Pad with Rotation

```xml
<Pad padstackDefRef="SMD_RECT">
  <Xform rotation="45.0"/>  <!-- Rotate 45° counter-clockwise -->
  <Location x="10.0" y="15.0"/>
  <StandardPrimitiveRef id="RECTCENTER_1"/>
</Pad>
```

---

## Filled Areas (Copper Pours)

Copper pours are large filled regions used for power/ground planes, shielding, or thermal management.

### Basic Structure

```xml
<Features>
  <Location x="0.0" y="0.0"/>
  <Outline>
    <Polygon>
      <PolyBegin x="0.0" y="0.0"/>
      <PolyStepSegment x="50.0" y="0.0"/>
      <PolyStepSegment x="50.0" y="40.0"/>
      <PolyStepSegment x="0.0" y="40.0"/>
      <PolyStepSegment x="0.0" y="0.0"/>  <!-- Close the loop -->
      <FillDescRef id="SOLID_FILL"/>
    </Polygon>
  </Outline>
</Features>
```

**Key Differences from Polyline:**
- `<Outline>` contains `<Polygon>` (not Polyline)
- Polygon MUST be closed (first point = last point)
- Uses `<FillDescRef>` instead of `<LineDescRef>`
- Renders as a solid filled area

### Real Example: Octagonal Copper Shape

```xml
<EntryStandard id="SHAPE_SH000002">
  <Contour>
    <Polygon>
      <PolyBegin x="-0.6500" y="-0.4000"/>
      <PolyStepSegment x="-0.4000" y="-0.6500"/>  <!-- Diagonal edge -->
      <PolyStepSegment x="0.4000" y="-0.6500"/>
      <PolyStepSegment x="0.6500" y="-0.4000"/>
      <PolyStepSegment x="0.6500" y="0.4000"/>
      <PolyStepSegment x="0.4000" y="0.6500"/>
      <PolyStepSegment x="-0.4000" y="0.6500"/>
      <PolyStepSegment x="-0.6500" y="0.4000"/>
      <PolyStepSegment x="-0.6500" y="-0.4000"/>  <!-- Closes to start -->
      <FillDescRef id="SOLID_FILL"/>
    </Polygon>
  </Contour>
</EntryStandard>
```

Creates an octagonal pad or copper pour centered at origin.

### Polygon with Curves

```xml
<Outline>
  <Polygon>
    <PolyBegin x="10.0" y="0.0"/>
    <PolyStepSegment x="40.0" y="0.0"/>
    <PolyStepCurve x="45.0" y="5.0"
                   centerX="40.0" centerY="5.0"
                   clockwise="false"/>  <!-- 90° arc -->
    <PolyStepSegment x="45.0" y="35.0"/>
    <PolyStepCurve x="40.0" y="40.0"
                   centerX="40.0" centerY="35.0"
                   clockwise="false"/>
    <PolyStepSegment x="10.0" y="40.0"/>
    <PolyStepCurve x="5.0" y="35.0"
                   centerX="10.0" centerY="35.0"
                   clockwise="false"/>
    <PolyStepSegment x="5.0" y="5.0"/>
    <PolyStepCurve x="10.0" y="0.0"
                   centerX="10.0" centerY="5.0"
                   clockwise="false"/>  <!-- Close with arc -->
    <FillDescRef id="SOLID_FILL"/>
  </Polygon>
</Outline>
```

Creates a rounded rectangle with 5mm corner radii.

---

## Standard Primitives

IPC-2581 defines 16 standard primitive shapes. All primitives can be:
- Defined inline
- Pre-defined in `<DictionaryStandard>` and referenced by ID
- Used for pads, fiducials, or copper pours

### Circle

**Definition:**
```xml
<Circle diameter="1.0">
  <FillDescRef id="SOLID_FILL"/>
</Circle>
```

**Attributes:**
- `diameter`: Outer diameter (nonNegativeDouble)
- Origin: Center of circle

**Example (0.6mm diameter pad):**
```xml
<EntryStandard id="CIRCLE_1">
  <Circle diameter="0.6000">
    <FillDescRef id="SOLID_FILL"/>
  </Circle>
</EntryStandard>
```

### RectCenter

**Definition:**
```xml
<RectCenter width="2.0" height="1.5">
  <FillDescRef id="SOLID_FILL"/>
</RectCenter>
```

**Attributes:**
- `width`: Dimension along X-axis
- `height`: Dimension along Y-axis
- Origin: Center of rectangle

**Example (1.1mm × 2.4mm pad):**
```xml
<EntryStandard id="RECTCENTER_3">
  <RectCenter width="1.1000" height="2.4000">
    <FillDescRef id="SOLID_FILL"/>
  </RectCenter>
</EntryStandard>
```

### Oval

**Definition:**
```xml
<Oval width="2.0" height="1.0">
  <FillDescRef id="SOLID_FILL"/>
</Oval>
```

**Geometry:** Rectangle with semicircular ends (full 180° arcs).

**Attributes:**
- `width`: Total width (including end radii)
- `height`: Total height
- End radius: Always = height/2

**Example:**
```xml
<EntryStandard id="OVAL_1">
  <Oval width="0.050000" height="0.025000">
    <FillDescRef id="SOLID_FILL"/>
  </Oval>
</EntryStandard>
```

### RectRound

**Definition:**
```xml
<RectRound width="2.0" height="1.5" radius="0.2"
           upperRight="true" upperLeft="true"
           lowerLeft="true" lowerRight="true">
  <FillDescRef id="SOLID_FILL"/>
</RectRound>
```

**Attributes:**
- `width`, `height`: Rectangle dimensions
- `radius`: Corner radius (must be ≤ min(width, height)/2)
- `upperRight`, `upperLeft`, `lowerLeft`, `lowerRight`: Boolean flags for which corners to round

**Example (rounded corners on all edges):**
```xml
<EntryStandard id="RECTROUND_1">
  <RectRound width="0.086614" height="0.196850" radius="0.007874"
             upperRight="true" upperLeft="true"
             lowerLeft="true" lowerRight="true">
    <FillDescRef id="SOLID_FILL"/>
  </RectRound>
</EntryStandard>
```

### RectCham

**Definition:**
```xml
<RectCham width="10.6" height="6.2" chamfer="2.0"
          upperLeft="true" lowerRight="true">
  <FillDescRef id="SOLID_FILL"/>
</RectCham>
```

**Attributes:**
- `width`, `height`: Rectangle dimensions
- `chamfer`: Chamfer distance from corner (45° cuts)
- Corner flags: Select which corners to chamfer

**Geometry:** Corners are cut at 45° angles.

### Contour

**Definition:**
```xml
<Contour>
  <Polygon>
    <PolyBegin x="0.0" y="0.0"/>
    <!-- Segments defining outer boundary -->
    <FillDescRef id="SOLID_FILL"/>
  </Polygon>
  <Cutout>
    <PolyBegin x="1.0" y="1.0"/>
    <!-- Segments defining inner cutout -->
    <FillDescRef id="VOID"/>
  </Cutout>
</Contour>
```

**Purpose:** Defines complex shapes with holes (e.g., thermal pads with voids).

**Real Example:**
```xml
<Contour>
  <Polygon>
    <PolyBegin x="0.00" y="0.00"/>
    <PolyStepSegment x="-14.00" y="0.00"/>
    <PolyStepCurve x="-3.00" y="3.00"
                   centerX="-14.00" centerY="3.00"
                   clockwise="true"/>
    <PolyStepSegment x="-17.00" y="7.00"/>
    <PolyStepSegment x="0.00" y="7.00"/>
    <PolyStepSegment x="0.00" y="0.00"/>
    <FillDescRef id="SOLID_FILL"/>
  </Polygon>
  <Cutout>
    <PolyBegin x="-10.00" y="5.00"/>
    <PolyStepSegment x="-13.00" y="5.00"/>
    <PolyStepSegment x="-13.00" y="3.00"/>
    <PolyStepCurve x="-10.00" y="3.00"
                   centerX="-11.50" centerY="3.00"
                   clockwise="false"/>
    <PolyStepSegment x="-10.00" y="5.00"/>
    <FillDescRef id="VOID"/>
  </Cutout>
</Contour>
```

### Donut

**Definition:**
```xml
<Donut shape="ROUND" outerDiameter="2.0" innerDiameter="1.0">
  <FillDescRef id="SOLID_FILL"/>
</Donut>
```

**Attributes:**
- `shape`: ROUND, SQUARE, HEXAGON, OCTAGON
- `outerDiameter`: Outer boundary
- `innerDiameter`: Inner void

**Purpose:** Annular rings (vias, thermal relief).

### Thermal

**Definition:**
```xml
<Thermal shape="ROUND" outerDiameter="3.0" innerDiameter="1.5"
         spokeCount="4" spokeWidth="0.5" spokeStartAngle="45.0">
  <FillDescRef id="SOLID_FILL"/>
</Thermal>
```

**Attributes:**
- `shape`: ROUND, SQUARE, HEXAGON, OCTAGON
- `outerDiameter`, `innerDiameter`: Ring dimensions
- `spokeCount`: Number of radial cutouts (0, 2, 3, 4)
- `spokeWidth`: Width of each spoke cutout
- `spokeStartAngle`: Angle of first spoke (degrees from +X axis)

**Purpose:** Thermal relief patterns to reduce heat sink from copper planes while maintaining electrical connection.

### Diamond, Hexagon, Octagon

All follow the same pattern:

```xml
<Diamond width="2.0" height="2.5">
  <FillDescRef id="SOLID_FILL"/>
</Diamond>

<Hexagon diameter="2.0">  <!-- Point-to-point, North-oriented -->
  <FillDescRef id="SOLID_FILL"/>
</Hexagon>

<Octagon diameter="3.0">  <!-- Point-to-point, North-oriented -->
  <FillDescRef id="SOLID_FILL"/>
</Octagon>
```

### Butterfly

**Definition:**
```xml
<Butterfly shape="SQUARE" side="1.8">
  <FillDescRef id="SOLID_FILL"/>
</Butterfly>
```

**Geometry:** Square or circle with two opposite quadrants removed (0-90° and 180-270°).

### Moire

**Definition:**
```xml
<Moire diameter="8.4" ringWidth="0.3" ringGap="0.6" ringNumber="5"
       lineWidth="0.3" lineLength="8.2" lineAngle="0">
  <FillDescRef id="SOLID_FILL"/>
</Moire>
```

**Purpose:** Registration marks for layer alignment.

---

## Hollow Shapes & Cutouts

IPC-2581 supports three levels of "negative" geometry:

### 1. fillProperty="HOLLOW"

Creates an **outlined shape** (no fill):

```xml
<DictionaryFillDesc units="MILLIMETER">
  <EntryFillDesc id="HOLLOW">
    <FillDesc fillProperty="HOLLOW"/>
  </EntryFillDesc>
</DictionaryFillDesc>

<Circle diameter="1.0">
  <FillDescRef id="HOLLOW"/>  <!-- Circle outline, not filled -->
</Circle>
```

**Use Cases:**
- Silkscreen markings
- Fiducial outlines
- Non-electrical annotations

### 2. fillProperty="VOID"

Creates a **cutout** in a previously filled area:

```xml
<Set>
  <Features>
    <Circle diameter="5.0">
      <FillDescRef id="SOLID_FILL"/>  <!-- Filled circle -->
    </Circle>
    <Circle diameter="2.0">
      <FillDescRef id="VOID"/>  <!-- Cuts a hole in the filled circle -->
    </Circle>
  </Features>
</Set>
```

**Rules:**
- VOID elements only affect elements that appear **before** them in the same Set
- VOID only clears **filled contours**, not hollow outlines
- VOID shapes must be fully contained within the parent filled shape

### 3. Cutout Element

Formal way to define holes in `<Contour>`:

```xml
<Contour>
  <Polygon>
    <!-- Outer boundary -->
    <FillDescRef id="SOLID_FILL"/>
  </Polygon>
  <Cutout>
    <!-- Inner hole -->
    <FillDescRef id="VOID"/>
  </Cutout>
  <Cutout>
    <!-- Another hole -->
    <FillDescRef id="VOID"/>
  </Cutout>
</Contour>
```

**Ordering:** Outermost polygon first, then cutouts from largest to smallest.

---

## Line Descriptors

Line descriptors define the **stroke properties** of traces and outlines.

### Basic Structure

```xml
<LineDesc lineEnd="ROUND" lineWidth="0.15"/>
```

**Attributes:**
- `lineEnd`: ROUND or SQUARE (cap style at endpoints)
- `lineWidth`: Total trace width (nonNegativeDouble)
- `lineProperty`: SOLID, DASHED (rarely used in copper layers)

### Dictionary Pattern

Line descriptors are typically pre-defined:

```xml
<DictionaryLineDesc units="MILLIMETER">
  <EntryLineDesc id="ROUND_0.15">
    <LineDesc lineEnd="ROUND" lineWidth="0.15"/>
  </EntryLineDesc>
  <EntryLineDesc id="ROUND_0.30">
    <LineDesc lineEnd="ROUND" lineWidth="0.30"/>
  </EntryLineDesc>
</DictionaryLineDesc>
```

Referenced in traces:

```xml
<Polyline>
  <PolyBegin x="10.0" y="20.0"/>
  <PolyStepSegment x="15.0" y="20.0"/>
  <LineDescRef id="ROUND_0.15"/>  <!-- 0.15mm wide trace -->
</Polyline>
```

### Real Example from Test Files

```xml
<DictionaryLineDesc units="MILLIMETER">
  <EntryLineDesc id="ROUND_1300">
    <LineDesc lineEnd="ROUND" lineWidth="0.1300"/>
  </EntryLineDesc>
  <EntryLineDesc id="ROUND_1570">
    <LineDesc lineEnd="ROUND" lineWidth="0.1570"/>
  </EntryLineDesc>
  <EntryLineDesc id="ROUND_3000">
    <LineDesc lineEnd="ROUND" lineWidth="0.3000"/>
  </EntryLineDesc>
</DictionaryLineDesc>
```

### lineEnd Types

#### ROUND (Observed in all test files)

Adds **semicircular caps** at trace endpoints:

```
     ___________
    (           )
    (___________)

    ← lineWidth →
```

Trace extends lineWidth/2 beyond the endpoint coordinates.

#### SQUARE (Specified but not observed)

Adds **square caps** (perpendicular ends):

```
     ___________
    |           |
    |___________|

    ← lineWidth →
```

Trace extends lineWidth/2 beyond endpoints with flat, perpendicular ends.

### Stroke Expansion Semantics

Given a Polyline:

```xml
<Polyline>
  <PolyBegin x="0.0" y="0.0"/>
  <PolyStepSegment x="10.0" y="0.0"/>
  <LineDesc lineEnd="ROUND" lineWidth="1.0"/>
</Polyline>
```

**Manufacturing Interpretation:**
1. **Centerline**: The path (0, 0) → (10, 0)
2. **Width**: ±0.5mm perpendicular to centerline
3. **Outline**: Rectangle from (-0.5, -0.5) to (10.5, 0.5)
4. **End caps**: Semicircles at (0, 0) and (10, 0) with radius 0.5mm

**Result:** A 1.0mm wide trace, 10mm long (centerline), with rounded ends, total length 11mm (including caps).

---

## Curves and Arcs

IPC-2581 supports circular arcs in two forms:

### 1. PolyStepCurve (in Polyline/Polygon)

**Definition:**
```xml
<PolyStepCurve x="15.0" y="25.0"
               centerX="10.0" centerY="20.0"
               clockwise="false"/>
```

**Attributes:**
- `x`, `y`: **Endpoint** of the arc
- `centerX`, `centerY`: **Center point** of the circular arc
- `clockwise`: Direction of arc from start to end

**Start Point:** Defined by previous `<PolyBegin>` or `<PolyStepSegment/Curve>`.

### Geometry

Given:
- Start: (x₁, y₁)
- End: (x₂, y₂)
- Center: (xc, yc)
- Direction: clockwise or counter-clockwise

The arc is the shorter of the two possible circular arcs between start and end, following the specified direction.

**Radius:** r = √((x₁ - xc)² + (y₁ - yc)²)

**Constraint:** The end point must also satisfy √((x₂ - xc)² + (y₂ - yc)²) = r (i.e., be on the circle).

### Real Example: Board Edge with Rounded Corner

```xml
<PolyStepCurve x="17.7946" y="-12.7000"
               centerX="17.794604" centerY="-13.335000"
               clockwise="false"/>
```

Creates a counter-clockwise arc from the previous point to (17.7946, -12.7) with center at (17.7946, -13.335), giving a 0.635mm radius arc.

### Complex Example: Multi-Arc Trace

```xml
<Polyline>
  <PolyBegin x="-1.072500" y="3.675000"/>
  <PolyStepSegment x="-1.114645" y="3.675000"/>  <!-- Straight -->
  <PolyStepCurve x="-1.132322" y="3.667678"
                 centerX="-1.11464466" centerY="3.65000000"
                 clockwise="false"/>  <!-- 90° arc -->
  <PolyStepSegment x="-1.140000" y="3.660000"/>  <!-- Straight -->
  <LineDescRef id="ROUND_15000"/>
</Polyline>
```

### 2. Arc Element (Standalone)

**Definition:**
```xml
<Arc startX="0.0" startY="0.0"
     endX="10.0" endY="10.0"
     centerX="0.0" centerY="10.0"
     clockwise="true">
  <LineDescRef id="ROUND_0.1"/>
</Arc>
```

**Use Case:** Standalone arc (not part of a polyline).

### Clockwise vs Counter-Clockwise

From the **TOP view** of the board:

- **clockwise="false"** (counter-clockwise): Arc curves to the left
- **clockwise="true"**: Arc curves to the right

Example: Moving from (0, 0) to (10, 0) with center at (5, 0):
- `clockwise="false"`: Arc goes upward (through (5, 5))
- `clockwise="true"`: Arc goes downward (through (5, -5))

### Tessellation Notes

CAM software typically tessellates arcs into line segments for rendering/manufacturing:

```
Arc → Cubic Bezier(s) → Line segments (e.g., 32 segments per bezier)
```

The IPC-2581 parser must convert:
1. Center-point arc notation → Kurbo Arc
2. Kurbo Arc → Cubic Beziers
3. Cubic Beziers → Line segments for stroke expansion

---

## Dictionary System

IPC-2581 uses four dictionaries to enable reuse and reduce file size:

### 1. DictionaryStandard

Stores **StandardPrimitive** shapes (Circle, RectCenter, Contour, etc.).

```xml
<DictionaryStandard units="MILLIMETER">
  <EntryStandard id="CIRCLE_0.6">
    <Circle diameter="0.6">
      <FillDescRef id="SOLID_FILL"/>
    </Circle>
  </EntryStandard>
  <EntryStandard id="RECT_1.5x2.0">
    <RectCenter width="1.5" height="2.0">
      <FillDescRef id="SOLID_FILL"/>
    </RectCenter>
  </EntryStandard>
</DictionaryStandard>
```

**Usage:**
```xml
<Pad>
  <Location x="10.0" y="15.0"/>
  <StandardPrimitiveRef id="CIRCLE_0.6"/>
</Pad>
```

### 2. DictionaryLineDesc

Stores **LineDesc** definitions (trace widths and end styles).

```xml
<DictionaryLineDesc units="INCH">
  <EntryLineDesc id="ROUND_71">
    <LineDesc lineEnd="ROUND" lineWidth="0.0071"/>
  </EntryLineDesc>
  <EntryLineDesc id="ROUND_276">
    <LineDesc lineEnd="ROUND" lineWidth="0.0276"/>
  </EntryLineDesc>
</DictionaryLineDesc>
```

**Usage:**
```xml
<Polyline>
  <PolyBegin x="1.0" y="2.0"/>
  <PolyStepSegment x="3.0" y="2.0"/>
  <LineDescRef id="ROUND_71"/>
</Polyline>
```

### 3. DictionaryFillDesc

Stores **FillDesc** definitions (fill patterns and properties).

```xml
<DictionaryFillDesc units="MILLIMETER">
  <EntryFillDesc id="SOLID_FILL">
    <FillDesc fillProperty="FILL"/>
  </EntryFillDesc>
  <EntryFillDesc id="HOLLOW">
    <FillDesc fillProperty="HOLLOW"/>
  </EntryFillDesc>
</DictionaryFillDesc>
```

**Usage:**
```xml
<Circle diameter="1.0">
  <FillDescRef id="SOLID_FILL"/>
</Circle>
```

### 4. DictionaryColor

Stores **Color** definitions for visualization (not manufacturing-critical).

```xml
<DictionaryColor>
  <EntryColor id="COLOR_TOP">
    <Color r="255" g="0" b="0"/>  <!-- Red -->
  </EntryColor>
  <EntryColor id="COLOR_BOTTOM">
    <Color r="0" g="0" b="255"/>  <!-- Blue -->
  </EntryColor>
</DictionaryColor>
```

**Usage:**
```xml
<Set net="VCC">
  <ColorRef id="COLOR_TOP"/>
</Set>
```

### Benefits of Dictionaries

1. **File Size Reduction**: A 0.15mm trace width defined once, referenced 10,000 times
2. **Consistency**: All instances guaranteed to use exact same parameters
3. **Maintainability**: Change one definition → updates all references
4. **Performance**: Parser can precompile dictionary entries

### Units Inheritance

Dictionary units apply to all definitions within:

```xml
<DictionaryLineDesc units="INCH">  <!-- All entries use inches -->
  <EntryLineDesc id="ROUND_71">
    <LineDesc lineEnd="ROUND" lineWidth="0.0071"/>  <!-- 0.0071 inches -->
  </EntryLineDesc>
</DictionaryLineDesc>
```

When referenced from `<Ecad>`, units must match the global `<CadHeader>` units.

---

## Real-World Examples

### Example 1: Simple Via Connection

**Scenario:** Plated through-hole via connecting TOP and BOTTOM layers.

```xml
<!-- TOP layer pad -->
<LayerFeature layerRef="TOP">
  <Set net="RESET" testPoint="false" plate="true" padUsage="VIA">
    <Pad padstackDefRef="VIA_0.3mm">
      <Location x="10.5" y="15.3"/>
      <Circle diameter="0.6">
        <FillDescRef id="SOLID_FILL"/>
      </Circle>
    </Pad>
  </Set>
</LayerFeature>

<!-- BOTTOM layer pad (same location) -->
<LayerFeature layerRef="BOTTOM">
  <Set net="RESET" testPoint="false" plate="true" padUsage="VIA">
    <Pad padstackDefRef="VIA_0.3mm">
      <Location x="10.5" y="15.3"/>
      <Circle diameter="0.6">
        <FillDescRef id="SOLID_FILL"/>
      </Circle>
    </Pad>
  </Set>
</LayerFeature>
```

**Manufacturing Interpretation:**
- Drill 0.3mm hole at (10.5, 15.3)
- Plate hole (copper inside barrel)
- 0.6mm diameter pad on TOP layer
- 0.6mm diameter pad on BOTTOM layer
- Net: RESET (electrically connected)

### Example 2: Curved Trace (Rigid-Flex)

**Scenario:** Trace following a curved board edge on a flex layer.

```xml
<LayerFeature layerRef="FLEX_2">
  <Set net="VCC">
    <Features>
      <Location x="0.0" y="0.0"/>
      <Polyline>
        <PolyBegin x="-0.997000" y="3.775000"/>
        <PolyStepSegment x="-0.997000" y="4.084145"/>  <!-- Straight -->
        <PolyStepCurve x="-1.004322" y="4.101822"
                       centerX="-1.02200000" centerY="4.08414466"
                       clockwise="false"/>  <!-- Small arc -->
        <PolyStepSegment x="-1.042678" y="4.140178"/>
        <PolyStepCurve x="-1.050000" y="4.157855"
                       centerX="-1.02500000" centerY="4.15785534"
                       clockwise="true"/>   <!-- Another arc -->
        <PolyStepSegment x="-1.050000" y="4.195000"/>
        <LineDescRef id="ROUND_15000"/>  <!-- 0.15mm -->
      </Polyline>
    </Features>
  </Set>
</LayerFeature>
```

**Interpretation:**
- Net: VCC
- Layer: FLEX_2 (flexible substrate)
- Path includes two curved segments and three straight segments
- Width: 0.15mm with round caps
- Curves follow board flex contour

### Example 3: SMD Pad Array (QFN Package)

**Scenario:** 4×4 array of rectangular SMD pads for a QFN component.

```xml
<LayerFeature layerRef="TOP">
  <Set net="GND">
    <!-- Thermal pad (center) -->
    <Pad>
      <Location x="0.0" y="0.0"/>
      <RectCenter width="5.0" height="5.0">
        <FillDescRef id="SOLID_FILL"/>
      </RectCenter>
    </Pad>
  </Set>

  <Set net="VCC">
    <!-- Row 1 pads -->
    <Pad>
      <Location x="-3.5" y="3.5"/>
      <RectCenter width="0.6" height="0.3">
        <FillDescRef id="SOLID_FILL"/>
      </RectCenter>
    </Pad>
    <Pad>
      <Location x="-2.5" y="3.5"/>
      <RectCenter width="0.6" height="0.3">
        <FillDescRef id="SOLID_FILL"/>
      </RectCenter>
    </Pad>
    <!-- ... more pads ... -->
  </Set>
</LayerFeature>
```

### Example 4: Copper Pour with Thermal Relief

**Scenario:** Ground plane with thermal relief around a via.

```xml
<LayerFeature layerRef="GND">
  <!-- Ground plane (large filled area) -->
  <Set net="GND">
    <Features>
      <Outline>
        <Polygon>
          <PolyBegin x="0.0" y="0.0"/>
          <PolyStepSegment x="100.0" y="0.0"/>
          <PolyStepSegment x="100.0" y="80.0"/>
          <PolyStepSegment x="0.0" y="80.0"/>
          <PolyStepSegment x="0.0" y="0.0"/>
          <FillDescRef id="SOLID_FILL"/>
        </Polygon>
      </Outline>
    </Features>
  </Set>

  <!-- Via with thermal relief -->
  <Set net="GND">
    <Pad>
      <Location x="50.0" y="40.0"/>
      <Thermal shape="ROUND" outerDiameter="2.0" innerDiameter="0.8"
               spokeCount="4" spokeWidth="0.4" spokeStartAngle="45.0">
        <FillDescRef id="SOLID_FILL"/>
      </Thermal>
    </Pad>
  </Set>
</LayerFeature>
```

**Result:**
- 100mm × 80mm ground plane
- Via at center (50, 40) with 0.8mm pad
- 2.0mm outer clearance with 4 thermal spokes at 45° angles
- Spokes are 0.4mm wide

### Example 5: Teardrop Trace Connection

**Scenario:** Trace with teardrop at via connection (improves reliability).

```xml
<LayerFeature layerRef="TOP">
  <Set net="SIGNAL_A" geometryUsage="TEARDROP">
    <Features>
      <Location x="0.0" y="0.0"/>
      <!-- Main trace -->
      <Polyline>
        <PolyBegin x="5.0" y="10.0"/>
        <PolyStepSegment x="10.0" y="10.0"/>
        <LineDescRef id="ROUND_0.15"/>
      </Polyline>

      <!-- Teardrop polygon -->
      <Outline>
        <Polygon>
          <PolyBegin x="10.0" y="10.075"/>  <!-- Trace edge -->
          <PolyStepCurve x="10.3" y="10.0"
                         centerX="10.0" centerY="10.0"
                         clockwise="false"/>  <!-- Curved transition -->
          <PolyStepCurve x="10.0" y="9.925"
                         centerX="10.0" centerY="10.0"
                         clockwise="false"/>
          <PolyStepSegment x="10.0" y="10.075"/>  <!-- Close -->
          <FillDescRef id="SOLID_FILL"/>
        </Polygon>
      </Outline>
    </Features>
  </Set>

  <Set net="SIGNAL_A" padUsage="VIA">
    <Pad>
      <Location x="10.0" y="10.0"/>
      <Circle diameter="0.6">
        <FillDescRef id="SOLID_FILL"/>
      </Circle>
    </Pad>
  </Set>
</LayerFeature>
```

---

## Semantic Motivation

### Why Centerline for Traces?

**Manufacturing Reality:**
- Photo plotters draw traces by moving a light source along a path at a specified width
- CNC routers follow paths with a cutting tool diameter
- Traces are **defined by their centerline**, not polygon outlines

**Benefits:**
1. **Direct mapping** to CAM tool commands
2. **Width changes** don't require recalculating coordinates
3. **File size**: 2 points + width vs. 4+ points for polygon outline
4. **Design intent**: "0.15mm trace from A to B" is clearer than "polygon with these 8 vertices"

### Why Separate LineDesc/FillDesc?

**Principle:** Separate **geometry** from **styling**.

**Analogy:** SVG/CSS separation:
- HTML: `<circle cx="10" cy="10" r="5"/>`
- CSS: `circle { fill: blue; stroke-width: 2px; }`

**IPC-2581:**
- Geometry: `<Circle diameter="1.0"/>`
- Styling: `<FillDescRef id="SOLID_FILL"/>`

**Benefits:**
1. **Reusability**: Same shape, different fills (solid, hollow, hatched)
2. **Consistency**: All 0.15mm traces guaranteed to use same LineDesc
3. **Maintainability**: Change line width globally by updating dictionary entry

### Why Hierarchical Set Organization?

**CAM Software Processing:**
1. Load layer
2. Group features by net
3. For each net:
   - Compute clearances
   - Check design rules
   - Generate toolpaths

**Benefits:**
- **Efficient querying**: "Give me all VCC features on TOP"
- **Design rule checking**: Clearance between nets
- **Selective export**: Export only power nets for plane analysis

### Why Dictionary Pattern?

**Real-World Data:**
- Typical PCB: 10,000+ traces
- Common trace widths: 5-10 unique values
- Without dictionary: 10,000 × 20 bytes = 200KB
- With dictionary: 10 × 20 bytes + 10,000 × 4 bytes (ID ref) = 40KB

**50% file size reduction** on typical designs.

---

## Edge Cases & Complex Scenarios

### 1. Traces Extending Beyond Board Edge

**Problem:** Curved traces with mitered joins can overshoot board edges by 0.1-0.3mm.

**Root Cause:**
- Polyline defines **centerline**
- Parser tessellates curves → line segments
- Stroke expansion uses rectangle + circle for each segment
- Mitered joins between segments overshoot on curves

**Solution:**
- Use lyon_algorithms for proper stroke expansion with round joins
- OR: Accept minor overshoot for visualization (use CAM tools for manufacturing accuracy)

**Example (testcase11 BOTTOM layer):**
```xml
<Polyline>
  <PolyBegin x="3.765" y="-0.915"/>
  <PolyStepSegment x="3.800857" y="-0.915"/>
  <PolyStepCurve x="3.807929" y="-0.917929"
                 centerX="3.800857" centerY="-0.925002"
                 clockwise="true"/>
  <LineDesc lineWidth="0.100"/>
</Polyline>
```

With 0.1mm trace width and tight curve, mitered joins can extend 0.2mm beyond ideal stroke boundary.

### 2. Multi-Segment Curves (Complex Board Outlines)

**Scenario:** Rigid-flex board with organic curves requires 20+ PolyStepCurve commands.

**Example:**
```xml
<Polygon>
  <PolyBegin x="-13.8800" y="154.3000"/>
  <PolyStepCurve x="-13.0800" y="155.5000"
                 centerX="-13.0800" centerY="154.3000"
                 clockwise="false"/>
  <PolyStepSegment x="-13.0800" y="157.5000"/>
  <PolyStepCurve x="-14.2800" y="158.7000"
                 centerX="-13.0800" centerY="158.7000"
                 clockwise="false"/>
  <!-- 15 more PolyStepCurve commands -->
  <FillDescRef id="SOLID_FILL"/>
</Polygon>
```

**Challenges:**
- Tessellation quality (how many line segments per curve?)
- Numerical precision (accumulated error)
- Performance (complex curves → many segments)

### 3. Hollow Circle (Registration Mark)

**Use Case:** Fiducial or alignment mark with outline only.

```xml
<GlobalFiducial>
  <Location x="5.0" y="5.0"/>
  <Circle diameter="3.0">
    <LineDesc lineEnd="ROUND" lineWidth="0.1"/>  <!-- Outline stroke -->
    <FillDescRef id="HOLLOW"/>  <!-- No fill -->
  </Circle>
</GlobalFiducial>
```

**Rendering:**
- 3.0mm diameter circle centered at (5, 5)
- 0.1mm outline stroke (width of the circle line itself)
- No copper fill inside

### 4. Polygon with Multiple Cutouts (Thermal Pad)

**Scenario:** Large thermal pad with multiple vias cut out.

```xml
<Contour>
  <Polygon>
    <!-- 5mm × 5mm square -->
    <PolyBegin x="-2.5" y="-2.5"/>
    <PolyStepSegment x="2.5" y="-2.5"/>
    <PolyStepSegment x="2.5" y="2.5"/>
    <PolyStepSegment x="-2.5" y="2.5"/>
    <PolyStepSegment x="-2.5" y="-2.5"/>
    <FillDescRef id="SOLID_FILL"/>
  </Polygon>

  <!-- Via cutout 1 -->
  <Cutout>
    <PolyBegin x="-1.0" y="-1.0"/>
    <PolyStepSegment x="-0.5" y="-1.0"/>
    <PolyStepSegment x="-0.5" y="-0.5"/>
    <PolyStepSegment x="-1.0" y="-0.5"/>
    <PolyStepSegment x="-1.0" y="-1.0"/>
    <FillDescRef id="VOID"/>
  </Cutout>

  <!-- Via cutout 2 -->
  <Cutout>
    <PolyBegin x="0.5" y="0.5"/>
    <PolyStepSegment x="1.0" y="0.5"/>
    <PolyStepSegment x="1.0" y="1.0"/>
    <PolyStepSegment x="0.5" y="1.0"/>
    <PolyStepSegment x="0.5" y="0.5"/>
    <FillDescRef id="VOID"/>
  </Cutout>
</Contour>
```

**Result:** 5mm square pad with two 0.5mm square holes.

### 5. Trace with Varying Width (Impedance Control)

**Scenario:** Differential pair with width taper near connector.

```xml
<Set net="USB_DP">
  <!-- Wide section (0.3mm) -->
  <Features>
    <Location x="0.0" y="0.0"/>
    <Polyline>
      <PolyBegin x="0.0" y="0.0"/>
      <PolyStepSegment x="10.0" y="0.0"/>
      <LineDescRef id="ROUND_0.30"/>
    </Polyline>
  </Features>

  <!-- Narrow section (0.15mm) -->
  <Features>
    <Location x="0.0" y="0.0"/>
    <Polyline>
      <PolyBegin x="10.0" y="0.0"/>
      <PolyStepSegment x="20.0" y="0.0"/>
      <LineDescRef id="ROUND_0.15"/>
    </Polyline>
  </Features>
</Set>
```

**Note:** Each `<Features>` block can have different LineDesc, allowing width changes.

### 6. Curved Trace Following Board Edge (Rigid-Flex)

**Scenario:** High-speed trace following a curved board edge to minimize length.

```xml
<Polyline>
  <PolyBegin x="10.0" y="20.0"/>
  <PolyStepSegment x="12.0" y="22.0"/>  <!-- Diagonal approach -->
  <PolyStepCurve x="15.0" y="24.0"
                 centerX="12.0" centerY="24.0"
                 clockwise="false"/>  <!-- 90° curve -->
  <PolyStepSegment x="20.0" y="24.0"/>  <!-- Straight run -->
  <PolyStepCurve x="22.0" y="22.0"
                 centerX="22.0" centerY="24.0"
                 clockwise="false"/>  <!-- Another 90° curve -->
  <PolyStepSegment x="25.0" y="20.0"/>  <!-- Exit -->
  <LineDescRef id="ROUND_0.15"/>
</Polyline>
```

**Challenge:** Accurate stroke expansion on curved sections to avoid board edge violations.

---

## Summary

IPC-2581 copper layer features provide a comprehensive, manufacturing-oriented representation of PCB copper artwork:

### Key Takeaways

1. **Hierarchical Structure**: Layer → LayerFeature → Set → Features → Geometry
2. **Centerline Representation**: Traces defined by path + width, not polygon outlines
3. **Dictionary Pattern**: Reusable definitions reduce file size and ensure consistency
4. **Standard Primitives**: 16 built-in shapes cover 95% of pad/via requirements
5. **Curve Support**: PolyStepCurve enables organic board shapes and curved traces
6. **Boolean Semantics**: FILL, HOLLOW, VOID provide additive and subtractive geometry
7. **Manufacturing Focus**: Data structure designed for CAM software and fabrication

### Implementation Considerations

**For Parser Authors:**
- Tessellate curves carefully (balance accuracy vs. performance)
- Implement proper stroke expansion (round joins, not miters)
- Handle Location offsets correctly
- Support dictionary precompilation for performance

**For CAD Tool Developers:**
- Export centerline + width for traces (not polygon outlines)
- Use dictionaries aggressively for common shapes/widths
- Validate arc geometry (endpoint must lie on circle)
- Test with rigid-flex designs (complex curves)

**For Manufacturers:**
- IPC-2581 is a lossless format (no Gerber "dark/clear" ambiguity)
- Curves are exact (not approximated with many short lines)
- Net information preserved (design rules, impedance control)
- Supports advanced features (HDI, rigid-flex, embedded components)

---

## References

- **IPC-2581 Revision C Specification**: Full standard with detailed schema
- **Test Case Suite**: 54 official test cases (testcase1-12, DM0002, SN0002)
- **XML Schema**: `http://webstds.ipc.org/2581`
- **W3C XML Schema Primer**: Background on XML Schema concepts
- **IEEE 754**: Floating-point precision standard used for coordinates

---

*Document Version 1.0*
*Generated from comprehensive analysis of IPC-2581 Rev C specification and 54 official test cases*
*For questions or corrections, refer to the IPC-2581 standards committee*

---

## Holes, Vias, and Through-Hole Technology

### Critical Concept: The Hole-Pad-Padstack Triangle

In IPC-2581, **holes are NOT part of copper layers**. They exist in a separate dimensional realm:

```
Conceptual Model:
┌─────────────────────────────────────┐
│  COPPER DOMAIN (2D)                 │
│  - Pads (per-layer shapes)          │
│  - Traces (Polylines)               │
│  - Pours (filled Polygons)          │
└─────────────────────────────────────┘
            ↕ Referenced by
┌─────────────────────────────────────┐
│  PADSTACK DOMAIN (Multi-layer)      │
│  - PadStackDef (defines pad shapes  │
│    on each layer + hole)            │
└─────────────────────────────────────┘
            ↕ Instantiated as
┌─────────────────────────────────────┐
│  Z-AXIS DOMAIN (3D)                 │
│  - Hole (drill operation)           │
│  - SlotCavity (routed slot)         │
└─────────────────────────────────────┘
```

### The Three-Level System

1. **PadStackDef** (Template): Defines what a via/PTH looks like across all layers
2. **Pad** (Instance on copper layer): Copper annular ring on a specific layer
3. **Hole** (Instance on drill layer): Physical drilling operation through Z-axis

---

## PadStackDef: The Multi-Layer Template

### Purpose and Motivation

**Why PadStackDef exists:**
- A through-hole via touches **multiple copper layers**
- Each layer may have **different pad geometry** (different sizes, thermal reliefs)
- The hole specification is **shared across all layers**
- **Antipad**: Clearance cutout in copper planes
- **Thermal**: Spoke pattern connecting pad to plane

**Without PadStackDef**, you'd need to manually replicate pad definitions across 12+ layers. **With PadStackDef**, you define once, reference everywhere.

### Basic Structure

```xml
<PadStackDef name="VIA_0.5mm_THRU">
  <!-- The hole specification (Z-axis) -->
  <PadstackHoleDef name="PH5000" 
                   diameter="0.5000" 
                   platingStatus="PLATED" 
                   plusTol="0.0" 
                   minusTol="0.0" 
                   x="0.0" 
                   y="0.0"/>
  
  <!-- Pad on TOP layer -->
  <PadstackPadDef layerRef="TOP" padUse="REGULAR">
    <Location x="0.0" y="0.0"/>
    <StandardPrimitiveRef id="CIRCLE_1"/>  <!-- 1.0mm pad -->
  </PadstackPadDef>
  
  <!-- Antipad (clearance) on TOP layer power plane -->
  <PadstackPadDef layerRef="TOP" padUse="ANTIPAD">
    <Location x="0.0" y="0.0"/>
    <StandardPrimitiveRef id="CIRCLE_15"/>  <!-- 1.5mm clearance -->
  </PadstackPadDef>
  
  <!-- Thermal relief on TOP layer -->
  <PadstackPadDef layerRef="TOP" padUse="THERMAL">
    <Location x="0.0" y="0.0"/>
    <StandardPrimitiveRef id="THERMAL_4_SPOKE"/>
  </PadstackPadDef>
  
  <!-- Repeat for LAYER_2, LAYER_3, ..., BOTTOM -->
</PadStackDef>
```

### Real Example from testcase9-RevC

```xml
<PadStackDef name="68C53P-MIL">
  <PadstackHoleDef name="PH13462" 
                   diameter="1.3462" 
                   platingStatus="PLATED" 
                   plusTol="0.0" 
                   minusTol="0.0" 
                   x="0.0" 
                   y="0.0"/>
  
  <!-- TOP layer: Regular pad -->
  <PadstackPadDef layerRef="TOP" padUse="REGULAR">
    <Location x="0.0" y="0.0"/>
    <StandardPrimitiveRef id="CIRCLE_10"/>
  </PadstackPadDef>
  
  <!-- TOP layer: Antipad (clearance in plane) -->
  <PadstackPadDef layerRef="TOP" padUse="ANTIPAD">
    <Location x="0.0" y="0.0"/>
    <StandardPrimitiveRef id="CIRCLE_13"/>
  </PadstackPadDef>
  
  <!-- TOP layer: Thermal relief -->
  <PadstackPadDef layerRef="TOP" padUse="THERMAL">
    <Location x="0.0" y="0.0"/>
    <StandardPrimitiveRef id="CIRCLE_10"/>
  </PadstackPadDef>
  
  <!-- LAYER_2: Regular pad -->
  <PadstackPadDef layerRef="LAYER_2" padUse="REGULAR">
    <Location x="0.0" y="0.0"/>
    <StandardPrimitiveRef id="CIRCLE_10"/>
  </PadstackPadDef>
  
  <!-- LAYER_2: Antipad -->
  <PadstackPadDef layerRef="LAYER_2" padUse="ANTIPAD">
    <Location x="0.0" y="0.0"/>
    <StandardPrimitiveRef id="CIRCLE_13"/>
  </PadstackPadDef>
  
  <!-- LAYER_2: Thermal -->
  <PadstackPadDef layerRef="LAYER_2" padUse="THERMAL">
    <Location x="0.0" y="0.0"/>
    <StandardPrimitiveRef id="CIRCLE_10"/>
  </PadstackPadDef>
  
  <!-- Repeat for LAYER_3, LAYER_4, BOTTOM -->
</PadStackDef>
```

**Analysis:**
- Hole: 1.3462mm diameter, **PLATED** (copper-lined barrel)
- Regular pad (CIRCLE_10): Actual copper annular ring
- Antipad (CIRCLE_13): Larger clearance cutout in planes
- Thermal (CIRCLE_10): Same size as regular, but with thermal relief pattern

### PadUse Attribute Values

```xml
padUse="REGULAR"  <!-- Normal copper pad -->
padUse="ANTIPAD"  <!-- Clearance hole in copper plane -->
padUse="THERMAL"  <!-- Thermal relief (spokes connecting to plane) -->
```

**Semantic Meaning:**

1. **REGULAR**: The standard copper pad
   - Used on signal layers
   - Used on power/ground planes when via connects to that net
   - Solid copper connection

2. **ANTIPAD**: Clearance cutout
   - Used on power/ground planes when via does **NOT** connect to that plane
   - Creates isolation gap
   - Prevents short circuit

3. **THERMAL**: Thermal relief connection
   - Used on power/ground planes when via **DOES** connect to that plane
   - Uses spoke pattern instead of solid connection
   - Reduces heat sink during soldering
   - Maintains electrical connection while allowing heat flow

### Example: Via Through GND Plane

```xml
<!-- Via connecting signal trace through GND plane -->
<PadStackDef name="VIA_SIGNAL">
  <PadstackHoleDef diameter="0.3" platingStatus="PLATED"/>
  
  <!-- TOP layer (signal): Regular pad -->
  <PadstackPadDef layerRef="TOP" padUse="REGULAR">
    <StandardPrimitiveRef id="CIRCLE_0.6"/>
  </PadstackPadDef>
  
  <!-- GND plane: Antipad (clearance, NOT connected) -->
  <PadstackPadDef layerRef="GND_PLANE" padUse="ANTIPAD">
    <StandardPrimitiveRef id="CIRCLE_1.0"/>  <!-- Larger clearance -->
  </PadstackPadDef>
  
  <!-- BOTTOM layer (signal): Regular pad -->
  <PadstackPadDef layerRef="BOTTOM" padUse="REGULAR">
    <StandardPrimitiveRef id="CIRCLE_0.6"/>
  </PadstackPadDef>
</PadStackDef>
```

**Result:**
- Via passes through GND plane without electrical connection
- 1.0mm clearance gap prevents short
- TOP and BOTTOM pads are 0.6mm solid copper

### Example: Via Connecting to GND Plane

```xml
<!-- Via connecting to GND plane -->
<PadStackDef name="VIA_GND">
  <PadstackHoleDef diameter="0.3" platingStatus="PLATED"/>
  
  <!-- TOP layer: Regular pad -->
  <PadstackPadDef layerRef="TOP" padUse="REGULAR">
    <StandardPrimitiveRef id="CIRCLE_0.6"/>
  </PadstackPadDef>
  
  <!-- GND plane: Thermal relief (connected with spokes) -->
  <PadstackPadDef layerRef="GND_PLANE" padUse="THERMAL">
    <Thermal shape="ROUND" 
             outerDiameter="1.2" 
             innerDiameter="0.6" 
             spokeCount="4" 
             spokeWidth="0.2"/>
  </PadstackPadDef>
  
  <!-- BOTTOM layer: Regular pad -->
  <PadstackPadDef layerRef="BOTTOM" padUse="REGULAR">
    <StandardPrimitiveRef id="CIRCLE_0.6"/>
  </PadstackPadDef>
</PadStackDef>
```

**Result:**
- Via electrically connected to GND plane
- 4 spokes (0.2mm wide) connect inner pad to plane
- Reduced thermal mass for soldering

---

## Holes: Physical Drilling Operations

### Hole Element Structure

```xml
<Hole name="H1" 
      diameter="0.5000" 
      platingStatus="VIA" 
      plusTol="0.001" 
      minusTol="0.001" 
      x="10.5" 
      y="15.3"/>
```

**Attributes:**
- `name`: Unique identifier (e.g., "H1", "H2", ...)
- `diameter`: Finished hole size (after plating if applicable)
- `platingStatus`: VIA | PLATED | NONPLATED | VIA_CAPPED
- `plusTol`, `minusTol`: Tolerance on hole diameter
- `x`, `y`: Absolute position on board

### platingStatus Values

```
VIA:          Plated through-hole used for electrical connection (via)
PLATED:       Plated through-hole for component lead (PTH)
NONPLATED:    Non-plated hole (NPTH) - mechanical only
VIA_CAPPED:   Via with top/bottom copper caps (plugged via)
```

**Manufacturing Interpretation:**

| platingStatus | Drill | Plate | Use Case |
|---|---|---|---|
| VIA | Yes | Yes | Signal via, power via |
| PLATED | Yes | Yes | Through-hole component lead |
| NONPLATED | Yes | No | Mounting hole, mechanical alignment |
| VIA_CAPPED | Yes | Yes + Cap | Buried via, thermal via with cap |

### Drill Layers

Holes appear in special **drill layers**, not copper layers:

```xml
<Layer name="DRILL_1-12" 
       layerFunction="DRILL" 
       side="NONE" 
       polarity="POSITIVE"/>

<LayerFeature layerRef="DRILL_1-12">
  <Set net="RESET" geometry="VIA_0.3mm">
    <Hole name="H1" diameter="0.3" platingStatus="VIA" x="10.5" y="15.3"/>
  </Set>
  
  <Set net="GND" geometry="VIA_0.3mm">
    <Hole name="H2" diameter="0.3" platingStatus="VIA" x="12.0" y="15.3"/>
  </Set>
</LayerFeature>
```

**Key Points:**
- `layerFunction="DRILL"`: Special layer type for drilling
- `side="NONE"`: Holes have no side (they go through)
- `geometry` attribute: References PadStackDef name
- Holes are grouped by `net` in Sets

### Real Example: Via Array from testcase1-RevC

```xml
<LayerFeature layerRef="DRILL_1-12">
  <Set net="TEST_TDO" geometry="LP033RND070_ICT">
    <ColorRef id="COLOR_DRILL_1-12"/>
    <Hole name="H1" diameter="0.0157" platingStatus="VIA" 
          plusTol="0.0" minusTol="0.0" x="-0.0875" y="5.8375"/>
  </Set>
  
  <Set net="UN30LED_QTLP680C203PRED10" geometry="LP033RND070_ICT">
    <Hole name="H2" diameter="0.0157" platingStatus="VIA" 
          plusTol="0.0" minusTol="0.0" x="-0.4125" y="1.1250"/>
  </Set>
  
  <Set net="UN30LED_QTLP680C203PGRN10" geometry="LP033RND070_ICT">
    <Hole name="H3" diameter="0.0157" platingStatus="VIA" 
          plusTol="0.0" minusTol="0.0" x="-0.4875" y="1.1250"/>
  </Set>
  
  <!-- ... hundreds more vias ... -->
</LayerFeature>
```

**Analysis:**
- All holes use geometry `LP033RND070_ICT` (refers to a PadStackDef)
- Diameter: 0.0157" (0.4mm) - typical via size
- All marked as `platingStatus="VIA"`
- Each via connected to a different net

### Relationship to Copper Pads

**The Connection:**

```
PadStackDef "LP033RND070_ICT"
├── PadstackHoleDef: 0.4mm diameter, PLATED
├── PadstackPadDef (TOP, REGULAR): 0.7mm circle
├── PadstackPadDef (GND2, ANTIPAD): 1.0mm circle
├── PadstackPadDef (L10, REGULAR): 0.7mm circle
└── ... (for all 12 layers)

When instantiated as Hole H1 at (x, y):
├── Drill operation at (x, y) with 0.4mm bit
├── Plate barrel with copper
├── Add 0.7mm pad on TOP layer at (x, y)
├── Add 1.0mm clearance on GND2 layer at (x, y)
└── Add 0.7mm pad on L10 layer at (x, y)
```

### Copper Layer Manifestation

The hole itself does NOT appear on copper layers. Instead, the **pads defined in PadStackDef** appear:

```xml
<!-- TOP copper layer -->
<LayerFeature layerRef="TOP">
  <Set net="TEST_TDO" testPoint="false" plate="true" padUsage="VIA">
    <Pad padstackDefRef="LP033RND070_ICT">
      <Location x="-0.0875" y="5.8375"/>  <!-- Same x, y as Hole H1 -->
      <StandardPrimitiveRef id="CIRCLE_1"/>
    </Pad>
  </Set>
</LayerFeature>

<!-- GND2 copper layer (plane) -->
<LayerFeature layerRef="GND2">
  <Set net="GND2">
    <!-- Large copper pour with clearances for vias -->
    <Features>
      <Outline>
        <Polygon>
          <!-- Board-sized polygon -->
        </Polygon>
      </Outline>
    </Features>
  </Set>
  
  <!-- Antipad cutout at via location (from PadStackDef) -->
  <!-- This is typically generated by CAM software from PadStackDef -->
</LayerFeature>
```

---

## SlotCavity: Routed Slots and Cavities

### Purpose

**SlotCavity** represents features created by **routing** (not drilling):

- **Slots**: Elongated holes (e.g., card edge connectors)
- **Cavities**: Partial-depth cutouts (e.g., component pockets)

Unlike circular holes, slots can have:
- Rectangular shapes
- Rounded ends
- Arbitrary polygon outlines
- Partial depth (not through-board)

### Basic Structure

```xml
<SlotCavity name="S155" 
            platingStatus="PLATED" 
            plusTol="0.0" 
            minusTol="0.0">
  <Outline>
    <Polygon>
      <PolyBegin x="15.2650" y="29.8950"/>
      <PolyStepSegment x="16.3650" y="29.8950"/>
      <PolyStepSegment x="16.3650" y="28.0950"/>
      <PolyStepSegment x="15.2650" y="28.0950"/>
      <PolyStepSegment x="15.2650" y="29.8950"/>
    </Polygon>
    <LineDesc lineEnd="ROUND" lineWidth="0.0"/>
  </Outline>
</SlotCavity>
```

**Components:**
- `platingStatus`: PLATED | NONPLATED
- `<Outline>`: Defines the slot shape (Polygon)
- `<LineDesc lineWidth="0.0">`: No stroke (hairline outline)

### Real Example from testcase9-RevC

```xml
<LayerFeature layerRef="DRILL_1-12">
  <Set geometry="2_25X1_50SL1_8X1_1P_MM" componentRef="R19">
    <SlotCavity name="S155" platingStatus="PLATED" plusTol="0.0" minusTol="0.0">
      <Outline>
        <Polygon>
          <PolyBegin x="15.2650" y="29.8950"/>
          <PolyStepSegment x="16.3650" y="29.8950"/>
          <PolyStepSegment x="16.3650" y="28.0950"/>
          <PolyStepSegment x="15.2650" y="28.0950"/>
          <PolyStepSegment x="15.2650" y="29.8950"/>
        </Polygon>
        <LineDesc lineEnd="ROUND" lineWidth="0.0"/>
      </Outline>
    </SlotCavity>
  </Set>
  
  <Set geometry="2_25X1_50SL1_8X1_1P_MM" componentRef="R19">
    <SlotCavity name="S156" platingStatus="PLATED" plusTol="0.0" minusTol="0.0">
      <Outline>
        <Polygon>
          <PolyBegin x="15.2650" y="18.8950"/>
          <PolyStepSegment x="16.3650" y="18.8950"/>
          <PolyStepSegment x="16.3650" y="17.0950"/>
          <PolyStepSegment x="15.2650" y="17.0950"/>
          <PolyStepSegment x="15.2650" y="18.8950"/>
        </Polygon>
        <LineDesc lineEnd="ROUND" lineWidth="0.0"/>
      </Outline>
    </SlotCavity>
  </Set>
</LayerFeature>
```

**Analysis:**
- Two rectangular slots for component R19
- Geometry references PadStackDef "2_25X1_50SL1_8X1_1P_MM"
- Size: ~1.1mm × 1.8mm rectangles
- `platingStatus="PLATED"`: Copper-plated walls

### Manufacturing Process

**Plated Slot:**
1. Laminate board layers
2. Route slot outline with CNC mill
3. **Plate** walls with copper (same as PTH process)
4. Add copper pads on each layer (from PadStackDef)

**Non-Plated Slot:**
1. Route slot after all copper/plating complete
2. No copper on walls (mechanical only)

### Slot with Rounded Ends

```xml
<SlotCavity name="S1" platingStatus="PLATED">
  <Outline>
    <Polygon>
      <PolyBegin x="10.0" y="5.0"/>
      <PolyStepSegment x="20.0" y="5.0"/>
      <PolyStepCurve x="20.0" y="3.0" 
                     centerX="20.0" centerY="4.0" 
                     clockwise="false"/>  <!-- Semicircle end -->
      <PolyStepSegment x="10.0" y="3.0"/>
      <PolyStepCurve x="10.0" y="5.0" 
                     centerX="10.0" centerY="4.0" 
                     clockwise="false"/>  <!-- Semicircle end -->
    </Polygon>
    <LineDesc lineEnd="ROUND" lineWidth="0.0"/>
  </Outline>
</SlotCavity>
```

Creates "stadium" shaped slot: 10mm long, 2mm wide, with semicircular ends.

### Cavity (Partial Depth)

From IPC-2581C spec section 8.2.3.10.6:

```xml
<SlotCavity name="SC1" platingStatus="PLATED">
  <Location x="345.200" y="45.832"/>
  <Oval width="2.3" height="0.5"/>
  <MaterialCut depth="0.6" 
               startCutLayer="TOP" 
               plusTol="0.06" 
               minusTol="0.06"/>
</SlotCavity>
```

**Attributes:**
- `<MaterialCut depth>`: Cut 0.6mm deep from TOP
- Does NOT go through entire board
- Used for component pockets, counterbores

---

## Comprehensive Via Example

### Scenario: Signal via from TOP to BOTTOM through 4-layer board

**Stackup:**
```
TOP (signal layer)
  GND (plane)
    SIG (internal signal)
  PWR (plane)
BOTTOM (signal layer)
```

**Step 1: Define PadStackDef**

```xml
<PadStackDef name="VIA_SIGNAL_0.3mm">
  <!-- Hole: 0.3mm drill, plated -->
  <PadstackHoleDef name="PH3000" 
                   diameter="0.3000" 
                   platingStatus="PLATED" 
                   plusTol="0.05" 
                   minusTol="0.05" 
                   x="0.0" 
                   y="0.0"/>
  
  <!-- TOP layer: 0.6mm pad -->
  <PadstackPadDef layerRef="TOP" padUse="REGULAR">
    <Location x="0.0" y="0.0"/>
    <Circle diameter="0.6">
      <FillDescRef id="SOLID_FILL"/>
    </Circle>
  </PadstackPadDef>
  
  <!-- GND plane: 1.2mm antipad (clearance, no connection) -->
  <PadstackPadDef layerRef="GND" padUse="ANTIPAD">
    <Location x="0.0" y="0.0"/>
    <Circle diameter="1.2">
      <FillDescRef id="HOLLOW"/>
    </Circle>
  </PadstackPadDef>
  
  <!-- SIG layer: 0.6mm pad -->
  <PadstackPadDef layerRef="SIG" padUse="REGULAR">
    <Location x="0.0" y="0.0"/>
    <Circle diameter="0.6">
      <FillDescRef id="SOLID_FILL"/>
    </Circle>
  </PadstackPadDef>
  
  <!-- PWR plane: 1.2mm antipad (clearance, no connection) -->
  <PadstackPadDef layerRef="PWR" padUse="ANTIPAD">
    <Location x="0.0" y="0.0"/>
    <Circle diameter="1.2">
      <FillDescRef id="HOLLOW"/>
    </Circle>
  </PadstackPadDef>
  
  <!-- BOTTOM layer: 0.6mm pad -->
  <PadstackPadDef layerRef="BOTTOM" padUse="REGULAR">
    <Location x="0.0" y="0.0"/>
    <Circle diameter="0.6">
      <FillDescRef id="SOLID_FILL"/>
    </Circle>
  </PadstackPadDef>
</PadStackDef>
```

**Step 2: Instantiate Hole on Drill Layer**

```xml
<LayerFeature layerRef="DRILL_1-4">
  <Set net="RESET" geometry="VIA_SIGNAL_0.3mm">
    <Hole name="H42" 
          diameter="0.3000" 
          platingStatus="VIA" 
          plusTol="0.05" 
          minusTol="0.05" 
          x="25.4" 
          y="38.1"/>
  </Set>
</LayerFeature>
```

**Step 3: Pads Appear on Copper Layers**

```xml
<!-- TOP layer: Signal trace connects to via -->
<LayerFeature layerRef="TOP">
  <Set net="RESET">
    <!-- Trace leading to via -->
    <Features>
      <Location x="0.0" y="0.0"/>
      <Polyline>
        <PolyBegin x="20.0" y="38.1"/>
        <PolyStepSegment x="24.8" y="38.1"/>  <!-- Stops 0.6mm from via center -->
        <LineDescRef id="ROUND_0.15"/>
      </Polyline>
    </Features>
    
    <!-- Via pad at (25.4, 38.1) -->
    <Pad padstackDefRef="VIA_SIGNAL_0.3mm">
      <Location x="25.4" y="38.1"/>
      <Circle diameter="0.6">
        <FillDescRef id="SOLID_FILL"/>
      </Circle>
    </Pad>
  </Set>
</LayerFeature>

<!-- GND layer: Plane with clearance -->
<LayerFeature layerRef="GND">
  <Set net="GND">
    <!-- Large filled plane -->
    <Features>
      <Outline>
        <Polygon>
          <PolyBegin x="0.0" y="0.0"/>
          <PolyStepSegment x="100.0" y="0.0"/>
          <PolyStepSegment x="100.0" y="80.0"/>
          <PolyStepSegment x="0.0" y="80.0"/>
          <PolyStepSegment x="0.0" y="0.0"/>
          <FillDescRef id="SOLID_FILL"/>
        </Polygon>
      </Outline>
    </Features>
    
    <!-- Antipad at (25.4, 38.1) creates clearance hole -->
    <!-- Typically handled by CAM software using PadStackDef ANTIPAD -->
  </Set>
</LayerFeature>

<!-- BOTTOM layer: Signal trace connects to via -->
<LayerFeature layerRef="BOTTOM">
  <Set net="RESET">
    <!-- Via pad -->
    <Pad padstackDefRef="VIA_SIGNAL_0.3mm">
      <Location x="25.4" y="38.1"/>
      <Circle diameter="0.6">
        <FillDescRef id="SOLID_FILL"/>
      </Circle>
    </Pad>
    
    <!-- Trace from via -->
    <Features>
      <Location x="0.0" y="0.0"/>
      <Polyline>
        <PolyBegin x="26.0" y="38.1"/>
        <PolyStepSegment x="30.0" y="38.1"/>
        <LineDescRef id="ROUND_0.15"/>
      </Polyline>
    </Features>
  </Set>
</LayerFeature>
```

**Manufacturing Result:**
1. Drill 0.3mm ±0.05mm hole at (25.4, 38.1)
2. Plate barrel with copper
3. Add 0.6mm pads on TOP, SIG, BOTTOM layers
4. Leave 1.2mm clearance on GND, PWR planes
5. Traces connect seamlessly to via pads

---

## Buried and Blind Vias

### Concept

**Through Via**: Goes from TOP to BOTTOM (all layers)
**Blind Via**: Goes from outer layer to internal layer (e.g., TOP to L2)
**Buried Via**: Goes between internal layers only (e.g., L2 to L4)

### Implementation

IPC-2581 handles this through **layer span** in PadStackDef:

```xml
<!-- Blind via: TOP to LAYER_2 only -->
<PadStackDef name="BLIND_VIA_TOP_L2">
  <PadstackHoleDef diameter="0.2" platingStatus="PLATED"/>
  
  <!-- Only define pads for layers in span -->
  <PadstackPadDef layerRef="TOP" padUse="REGULAR">
    <StandardPrimitiveRef id="CIRCLE_0.4"/>
  </PadstackPadDef>
  
  <PadstackPadDef layerRef="LAYER_2" padUse="REGULAR">
    <StandardPrimitiveRef id="CIRCLE_0.4"/>
  </PadstackPadDef>
  
  <!-- No definitions for LAYER_3, LAYER_4, BOTTOM -->
</PadStackDef>

<!-- Drill layer specifies span -->
<Layer name="DRILL_TOP_L2" layerFunction="DRILL">
  <Span fromLayer="TOP" toLayer="LAYER_2"/>
</Layer>

<LayerFeature layerRef="DRILL_TOP_L2">
  <Set net="CLK" geometry="BLIND_VIA_TOP_L2">
    <Hole name="H_BLIND_1" diameter="0.2" platingStatus="VIA" x="10.0" y="10.0"/>
  </Set>
</LayerFeature>
```

**Key Points:**
- `<Span fromLayer="TOP" toLayer="LAYER_2"/>`: Defines depth range
- PadStackDef only includes layers in span
- Manufacturing: Drill before laminating all layers

---

## Non-Plated Through Holes (NPTH)

### Purpose

**NPTH** = holes without copper plating:
- Mounting holes
- Mechanical alignment pins
- Standoff locations
- Press-fit pins (some designs)

### Example

```xml
<PadStackDef name="MOUNTING_HOLE_3.2mm">
  <!-- Non-plated hole -->
  <PadstackHoleDef name="NH31750" 
                   diameter="3.175" 
                   platingStatus="NONPLATED" 
                   plusTol="0.1" 
                   minusTol="0.1" 
                   x="0.0" 
                   y="0.0"/>
  
  <!-- Clearance on all layers (no copper) -->
  <PadstackPadDef layerRef="TOP" padUse="ANTIPAD">
    <StandardPrimitiveRef id="CIRCLE_3.5"/>
  </PadstackPadDef>
  
  <PadstackPadDef layerRef="LAYER_2" padUse="ANTIPAD">
    <StandardPrimitiveRef id="CIRCLE_3.5"/>
  </PadstackPadDef>
  
  <!-- ... antipad on all copper layers ... -->
</PadStackDef>

<LayerFeature layerRef="DRILL_NPTH">
  <Set geometry="MOUNTING_HOLE_3.2mm">
    <Hole name="MOUNT_1" 
          diameter="3.175" 
          platingStatus="NONPLATED" 
          x="5.0" 
          y="5.0"/>
  </Set>
  
  <Set geometry="MOUNTING_HOLE_3.2mm">
    <Hole name="MOUNT_2" 
          diameter="3.175" 
          platingStatus="NONPLATED" 
          x="95.0" 
          y="75.0"/>
  </Set>
</LayerFeature>
```

**Manufacturing:**
1. Drill 3.175mm hole (NO plating)
2. Ensure 3.5mm clearance on all copper layers
3. Use for M3 screw mounting

---

## Via Capping / Plugging

### VIA_CAPPED

**Scenario:** Via needs to be sealed (plugged) for:
- Solder dam (prevent solder wicking)
- Component mounting on top of via
- Moisture protection
- Thermal management

```xml
<Hole name="H_CAPPED" 
      diameter="0.3" 
      platingStatus="VIA_CAPPED" 
      x="20.0" 
      y="30.0"/>
```

**Manufacturing Process:**
1. Drill and plate via normally
2. Fill via with epoxy or solder mask
3. Add copper cap on TOP and/or BOTTOM
4. Allows SMD pad to be placed directly over via

---

## Advanced Topics

### Thermal Vias

**Purpose:** Heat dissipation from IC to plane or heatsink

```xml
<PadStackDef name="THERMAL_VIA_0.3mm">
  <PadstackHoleDef diameter="0.3" platingStatus="PLATED"/>
  
  <!-- TOP layer: Small pad under IC thermal pad -->
  <PadstackPadDef layerRef="TOP" padUse="REGULAR">
    <Circle diameter="0.5">
      <FillDescRef id="SOLID_FILL"/>
    </Circle>
  </PadstackPadDef>
  
  <!-- GND plane: Direct connection (no thermal relief) -->
  <PadstackPadDef layerRef="GND" padUse="REGULAR">
    <Circle diameter="0.5">
      <FillDescRef id="SOLID_FILL"/>
    </Circle>
  </PadstackPadDef>
</PadStackDef>

<!-- Array of thermal vias under QFN package -->
<LayerFeature layerRef="DRILL_1-4">
  <Set net="GND" geometry="THERMAL_VIA_0.3mm">
    <Hole name="TH1" diameter="0.3" platingStatus="VIA" x="50.0" y="50.0"/>
    <Hole name="TH2" diameter="0.3" platingStatus="VIA" x="50.6" y="50.0"/>
    <Hole name="TH3" diameter="0.3" platingStatus="VIA" x="51.2" y="50.0"/>
    <!-- ... grid of thermal vias ... -->
  </Set>
</LayerFeature>
```

**Note:** No thermal relief spokes (full solid connection for maximum heat transfer).

### Castellated Edges

**Concept:** Half-drilled holes at board edge for module mounting

```xml
<!-- Define padstack with offset hole -->
<PadStackDef name="CASTELLATED_1.0mm">
  <PadstackHoleDef diameter="1.0" 
                   platingStatus="PLATED" 
                   x="0.0"  <!-- Hole center at board edge -->
                   y="0.0"/>
  
  <PadstackPadDef layerRef="TOP" padUse="REGULAR">
    <Oval width="1.0" height="1.5">  <!-- Extends inward from edge -->
      <FillDescRef id="SOLID_FILL"/>
    </Oval>
  </PadstackPadDef>
</PadStackDef>

<!-- Place holes at board edge -->
<LayerFeature layerRef="DRILL_EDGE">
  <Set geometry="CASTELLATED_1.0mm">
    <Hole name="CAST_1" diameter="1.0" platingStatus="PLATED" x="0.0" y="10.0"/>
    <Hole name="CAST_2" diameter="1.0" platingStatus="PLATED" x="0.0" y="12.54"/>
    <!-- ... along edge ... -->
  </Set>
</LayerFeature>
```

**Manufacturing:** Drill holes, then route board edge through hole centers.

---

## Summary: The Complete Via System

### Data Flow

```
DESIGN PHASE:
1. Engineer defines PadStackDef (template)
   - Hole size, plating
   - Pad shapes per layer
   - Antipads, thermals

INSTANTIATION PHASE:
2. Place Hole on drill layer
   - References PadStackDef via geometry attribute
   - Specifies x, y location

EXPANSION PHASE (CAM Software):
3. For each Hole instance:
   - Read referenced PadStackDef
   - Generate Pad on each layer (per PadstackPadDef)
   - Apply REGULAR, ANTIPAD, THERMAL as specified
   - Generate clearances in copper pours

MANUFACTURING PHASE:
4. Drill holes (from drill layer data)
5. Plate holes (if platingStatus != NONPLATED)
6. Image copper (pads from layer data)
7. Etch copper (remove unwanted copper)
```

### File Organization Pattern

Typical IPC-2581 file structure:

```xml
<IPC-2581>
  <Content>
    <!-- Dictionaries -->
    <DictionaryStandard>
      <!-- Pad shapes: CIRCLE_0.6, CIRCLE_1.0, THERMAL_4_SPOKE, etc. -->
    </DictionaryStandard>
    
    <!-- Stackup -->
    <Stackup>
      <StackupLayer layerRef="TOP" thickness="0.035"/>
      <StackupLayer layerRef="GND" thickness="0.5"/>
      <StackupLayer layerRef="SIG" thickness="0.035"/>
      <StackupLayer layerRef="PWR" thickness="0.5"/>
      <StackupLayer layerRef="BOTTOM" thickness="0.035"/>
    </Stackup>
    
    <!-- Step (design) -->
    <Step name="MyBoard">
      <!-- PadStackDefs -->
      <PadStackDef name="VIA_0.3mm">...</PadStackDef>
      <PadStackDef name="PTH_1.0mm">...</PadStackDef>
      <PadStackDef name="NPTH_3.2mm">...</PadStackDef>
      
      <!-- Copper Layers -->
      <LayerFeature layerRef="TOP">
        <Set net="RESET">
          <Pad padstackDefRef="VIA_0.3mm">
            <Location x="25.4" y="38.1"/>
          </Pad>
          <Features>
            <Polyline>...</Polyline>
          </Features>
        </Set>
      </LayerFeature>
      
      <LayerFeature layerRef="GND">
        <!-- Copper plane with anti pads -->
      </LayerFeature>
      
      <LayerFeature layerRef="BOTTOM">
        <!-- More traces and pads -->
      </LayerFeature>
      
      <!-- Drill Layers -->
      <LayerFeature layerRef="DRILL_1-4">
        <Set net="RESET" geometry="VIA_0.3mm">
          <Hole name="H1" diameter="0.3" platingStatus="VIA" x="25.4" y="38.1"/>
        </Set>
      </LayerFeature>
      
      <LayerFeature layerRef="DRILL_NPTH">
        <Set geometry="NPTH_3.2mm">
          <Hole name="MOUNT_1" diameter="3.175" platingStatus="NONPLATED" x="5.0" y="5.0"/>
        </Set>
      </LayerFeature>
    </Step>
  </Content>
</IPC-2581>
```

### Critical Insights

1. **Holes are NOT copper features** - they're Z-axis drilling operations
2. **PadStackDef is the bridge** - connects 3D holes to 2D copper layers
3. **Each layer gets its own pad definition** - allows layer-specific geometry
4. **ANTIPAD creates clearances** - essential for plane isolation
5. **THERMAL manages heat** - spokes balance electrical and thermal needs
6. **platingStatus determines process** - VIA vs PLATED vs NONPLATED
7. **Slots extend the system** - non-circular holes with routing

---

## Specification References

### From IPC-2581C Specification

**Section 8.2.3.2 - PadStackDef:**
> The PadStackDef element consists of multiple padstacks types or descriptions taken from the CAD system and is intended to preserve the data from the layout system. The information noted pertain to the CadProperty of which the padstack is a part. The relationship is identified by the CadProperty unique name and is the original design file from the CAD system. The data becomes although redundant when the individual layered features are defined provides a reference for the padstack usage.

**Section 8.2.3.10.5 - Hole:**
> The Hole element describes the characteristics of a particular hole, including naming the hole description with a unique name that may be reused. The main purpose of including hole in the Set means that specific information can be described as all the particular holes in one set of data. In this instance, the layerRef of LayerFeature is to the Layer/Stackup element which describes the overallThickness for those holes that go entirely through the board. For those holes that are buried or blind vias, the appropriate Stackup reference shall be used as a part of the layerRef of the LayerFeature descriptions of holes.

**Section 8.2.3.10.6 - SlotCavity:**
> The SlotCavity element describes a feature created by a machining operation that removes material from a bare board within a given shape. The shape is defined by the substitution group Feature, which can be either a user defined shape or a standard primitive shape. The feature can be plated or nonplated. The SlotCavity element can occur multiple times within the LayerFeature Set of a layer.

---

## Skia-Based Copper SVG Export Plan

### Goals & Scope

- Generate a single SVG document containing vertically stacked copper layers with per-bucket coloring (pads, vias, traces, fills, cutouts).
- Achieve micron-level precision suitable for manufacturing review.
- Leverage existing IPC-2581 parsing; focus on geometry resolution, flattening, and SVG emission.
- Provide a debuggable, staged pipeline that supports quick iteration.

### Stage Map Overview

| Stage | Purpose | Key Inputs | Key Outputs |
|-------|---------|------------|-------------|
| S0 | Load & units | Parsed IPC structures | `BoardContext` with normalized units, dictionaries |
| S1 | Hierarchy/Xform resolution | `BoardContext`, `LayerFeature` | `ResolvedFeature` list per layer |
| S2 | Padstack expansion | `ResolvedFeature`, padstack dict | Cached `SkPath` per pad instantiation |
| S3 | Primitive conversion | Resolved copper primitives | Classified `SkPath` + polarity |
| S4 | Boolean flattening | Bucketed paths | Final geometry per bucket (positive minus negative) |
| S5 | Composite & color | Bucket geometry | Render-ready layer model |
| S6 | SVG emission | Layer model | SVG document written to disk |

Each stage should expose timing metrics and optional debug dumps for inspection.

### Stage Details

**Stage 0 – Input Readiness**
- Reuse existing parser (`parse.rs`). Ensure units convert to a fixed nanometer grid (int64) while retaining original doubles until boolean ops.
- Build `BoardContext` struct: references to dictionary entries, padstack defs, line descriptors, stackup metadata.
- Add validation log (counts of layers, features, padstack usages).
- Output cached to avoid rework unless XML timestamps change.

**Stage 1 – Hierarchy & Transformation Resolution**
- Iterate `LayerFeature → Set → Features`.
- Apply `<Location>` offsets then `<Xform>` operations (offset, rotation, mirror, scale) using 64-bit floats.
- Produce `ResolvedFeature { feature_kind, polarity, net, padstack_ref, geometry_spec }`.
- Record per-layer bounding boxes for centering the later SVG stack.
- Add optional debug flag `--dump-stage1 layer_name` writing JSON (feature counts, sample coords).

**Stage 2 – Padstack Expansion**
- For each `ResolvedFeature` that references a padstack, look up the pad geometry for the active layer and pad use (REGULAR, ANTIPAD, THERMAL).
- Build `PadCacheKey { padstack_symbol, layer_symbol, pad_use }`; store `Arc<SkPath>`.
- Apply final translation to target coordinates with `SkPath::transform`.
- For holes/slots: only instantiate copper-bearing pad shapes; drilling data stays out of SVG export.
- Collect expanded pads into classification buckets (pads, vias, pth, etc.) but defer boolean ops.

**Stage 3 – Primitive Conversion**
- Convert polylines/traces: create `SkPath` centerline, configure `PathStroker` with line width, join, cap from `LineDesc`.
- Convert polygons/pours directly via path construction; arcs modeled with Skia's arc primitives to preserve curvature.
- Simplify optional: apply `path.simplify()` to remove redundant segments, reducing boolean cost.
- Append negative features marked explicitly (no subtraction yet).

**Stage 4 – Boolean Flattening**
- For each physical layer, maintain buckets:
  - `CopperBucket::Pads`
  - `CopperBucket::Vias`
  - `CopperBucket::Smd`
  - `CopperBucket::Traces`
  - `CopperBucket::Fills`
  - `CopperBucket::Cutouts`
- Within each bucket:
  1. Union all positive polarity paths via `pathops::op_union`.
  2. Union all negative polarity paths separately.
  3. Subtract negatives from positives (`op_difference`).
- Prior to ops, snap coordinates to nanometer grid via `SkMatrix::scale` or manual rounding to avoid sliver artifacts.
- Record statistics (node counts, area) for QA logs.

**Stage 5 – Composite & Styling**
- Map each bucket to an RGBA color and opacity (e.g., pads `#FFA500`, vias `#1E90FF`, traces `#FF4500`, fills `#32CD32`, cutouts `#000000` with alpha).
- Build `LayerRenderModel { layer_name, bbox, buckets: Vec<BucketRender> }`.
- Optionally generate legend metadata (bucket name, color).
- Provide debug flag `--dump-stage5 layer=bucket` to export per-bucket SVG for inspection.

**Stage 6 – SVG Emission**
- Determine vertical stacking offset: use max layer bounding width/height, add configurable gutter (e.g., 2 mm) between layers.
- Create SVG root sized to accommodate all layers; set `viewBox` in micrometers for deterministic scaling.
- For each layer:
  - Create `<g id="layer-TOP" transform="translate(0, y_offset)">`.
  - Insert `<title>` and `<desc>` metadata describing layer function and net count.
  - Emit sub-`<g>` per bucket with `class="bucket-pads"` etc., apply fill/stroke colors, set `pointer-events="none"` for easier viewing.
- Add `<style>` section mapping bucket classes to colors, plus hover effects for debugging.
- Write final SVG to `export/<board_name>_copper.svg`.

### Tooling & Iteration Workflow

- New CLI entry: `cargo run -- export-svg <file.ipc> --layers TOP,BOTTOM --timings --dump-stage5 TOP:traces`.
- Timing output after each stage (ms) for profiling.
- Optional `--skip-stage` flags for rapid iteration (e.g., rebuild from Stage 4 after tweaking boolean ops).
- Unit tests in `tests/svg_export.rs` verifying:
  - Transform resolution for combined rotation/mirror.
  - Padstack expansion matches expected bounding boxes/area.
  - Boolean difference removes antipads correctly.
- Real board regression: supply golden stats JSON (layer → bucket → area µm²). Compare within tolerance before writing SVG (`--verify-only` mode).

### Implementation Sequence

1. Wire `export_svg` command scaffolding, stage definitions, timing hooks.
2. Implement Stage 1 resolver leveraging existing geometry helpers; add targeted tests.
3. Add padstack cache + Stage 2 expansion; confirm via debug dumps on small fixture.
4. Build Stage 3 primitive translation; integrate Skia `PathStroker`, ensure micron precision.
5. Implement Stage 4 boolean pipeline with snap-to-grid helper and stats logging.
6. Assemble Stage 5/6 rendering and SVG writer with vertical stacking.
7. Add CLI flags for debug dumps, update documentation, and validate on representative boards.

### Debugging Guidelines

- Use `--dump-stage3 layer=bucket` to inspect pre-boolean paths when subtraction issues appear.
- Check area deltas between Stage 3 and Stage 4 to detect unexpected losses/gains.
- For tricky pads, render only the pad bucket by temporarily disabling others via CLI flag `--only-buckets pads,traces`.
- Keep Skia CPU backend in software mode; avoid GPU dependencies for deterministic CI runs.

With this plan, another engineer can implement the Skia-backed SVG export iteratively, ensuring correctness first while keeping the pipeline structured for future optimizations.

---

*End of Holes, Vias, and Through-Hole Technology section*
