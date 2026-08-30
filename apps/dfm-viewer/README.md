# PCB DFM viewer

A standalone Vite / React / TypeScript app for inspecting DFM diagnostic JSON.
It runs entirely in the browser; opening a file does not upload it. React and
React DOM are the only runtime dependencies. No account, backend, external
fonts, or telemetry is required.

From the repository root:

```sh
cargo run -p pcbc -- ipc dfm check board.xml --pdk standard \
  --include-geometry --output board.dfm.json

npm --prefix apps/dfm-viewer ci
npm --prefix apps/dfm-viewer run dev
```

Open the local URL printed by Vite, then drop the JSON file into the app or
choose **Open JSON**. Multiple files can be opened together and selected in the
header. Failing DFM checks still write their reports before exiting nonzero.
`pcb dfm board.zen --pdk standard --include-geometry -o board.dfm.json` exports the same format.

Ordinary diagnostic JSON remains supported, with a clear notice when full
board geometry is absent. Preparation errors use `verdict: "incomplete"`;
they never appear as passing checks. Scope, skipped checks, waivers, and
measurement uncertainty remain available in the inspector.

## Navigation

- Select a diagnostic family, a grouped cause, and a site or repeated physical
  occurrence. Layer and occurrence filters include the applicable descendants.
- **Fit finding** (`F`) returns to the measured region. **Fit layout** (`A` or
  `0`) shows the entire checked board, array, or fabrication panel.
- Drag to pan without a region clamp. `+` / `−`, double-click, or pinch zoom
  around the pointer. Shift-double-click zooms out.
- **Trackpad** mode: two-finger scroll pans; pinch or Ctrl+scroll zooms.
  **Mouse** mode: wheel zooms, left/middle drag pans.
- The minimap shares the actual vector geometry. Fine full-span crosshairs
  locate the red finding box without covering it; a blue dashed box shows the
  live main viewport, and gray dots mark other related sites. Click or drag to recenter. It expands to
  retain both the layout and viewport when you pan or zoom beyond the layout.
- Both scale bars use the current CSS pixels per millimeter, including after
  resizing. `←` / `→` move between sites; `J` / `K` move between findings.

## Geometry and labels

`--include-geometry` adds a versioned `scene` to the diagnostic report. Each
semantic layer or feature pass is exported once for the full checked layout.
Native PCB IR SVG retains analytic curves, aperture reuse, affine transforms,
polarity, and cutouts. The main view and minimap reference the same active
vector definitions; neither uses raster thumbnails or geometry cropped to
the diagnostic region.

Checks also supply optional native display geometry for evidence: routed-slot
arcs, round clearance bands, overlapping drill circles, and required copper
minus the actual copper layer. The browser renders these as native SVG paths,
strokes, and masks, so zooming does not expose a fixed polygon approximation.
Layer material definitions are reused during navigation; evidence filter and
mask surfaces are bounded to the visible region. No denser whole-board mesh
or browser geometry library is needed.

The original evidence remains authoritative for its measured quantity and
declared uncertainty. Native display geometry does not alter measurements,
verdicts, or finding/site/waiver identities. Genuine source polygons, measured
boundary spans, and candidate regions remain polygons; the viewer never fits
invented curves through them. Width-disk diameters are not replaced with
chords between their boundary witnesses. Old reports retain their original
evidence; regenerate JSON to obtain the new display geometry.

SVG input is parsed into an allowlisted vector tree, not injected as HTML.
Only finite geometry and local fragment references are accepted; IDs are
namespaced per layer. Scripts, active elements, styles, entities, external
resources, and unknown coordinate frames are rejected.

Selected dimensions have fixed-size text on translucent backplates with
leaders; required dimensions and shortfalls stay together in the inspector.
The compact inspector also shows measurement meaning and uncertainty, the rule
and method, applicable layers, region coordinates, and source provenance.
The label approach follows [ArcGIS callout guidance](https://pro.arcgis.com/en/pro-app/3.3/help/mapping/layer-properties/text-symbols.htm)
and [MapLibre's ordered anchor placement](https://maplibre.org/maplibre-style-spec/layers/#text-variable-anchor).
The camera overview follows the familiar [minimap viewport pattern](https://reactflow.dev/api-reference/components/minimap).

## Development and distribution

```sh
npm --prefix apps/dfm-viewer test
npm --prefix apps/dfm-viewer run build
npm --prefix apps/dfm-viewer run preview
```

`dist/` is an ordinary static application. Serve it with any static HTTP server
and open local JSON files in the browser. A same-origin `?report=/path/run.json`
URL can load a report directly; cross-origin report URLs are intentionally
rejected. There is no build-time or runtime dependency between Cargo and Node.

For local feedback, generated corpus JSON can be placed in `public/local/` and
opened with `?report=/local/example.dfm.json`. That directory is gitignored and
excluded from production builds, so private boards cannot accidentally become
part of the viewer distribution.
