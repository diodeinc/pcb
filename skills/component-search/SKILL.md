---
name: component-search
description: Search for and add electronic components, modules, and reference designs to a Zener PCB project. Use when designing a board, module, or subsystem and you need to find parts, reusable subcircuits, or reference designs — whether by functional description or specific MPN. Covers `pcb search` (registry:modules, registry:components, web:components) and `pcb new component` for importing web components into a workspace.
---

# Component Search

Find and add components, modules, and reference designs to a Zener project. Use this workflow any time you need a part or subcircuit that isn't already in the workspace or covered by stdlib generics.

## Hard Stop Before Manual Creation

If `pcb search` and `pcb new component` do not produce a usable part, do not manually create the component, symbol, or footprint by default. Briefly say what failed, say you can try making it manually, and get explicit user confirmation first. In interactive mode, prefer `ask`.

## Search Priority

Always search in this order. Move down only when the higher tier doesn't have what fits.

1. **`pcb search -m registry:modules <query> -f json`** — Pre-designed, validated subcircuits (modules and reference designs). Best option: the design work is already done, with passives, layout, and validation included.
2. **`pcb search -m registry:components <query> -f json`** — Pre-packaged component definitions in the registry. Good: symbol, footprint, and `.zen` file already exist and are ready to use.
3. **`pcb search -m web:components <MPN> -f json`** — Diode's web component database (CSE, LCSC sources). Fallback: returns a `component_id` that must be imported with `pcb new component`.

If the user asks for a specific MPN, still try registry first before falling back to web.

## Search Tips

- **Registry search** is richly indexed — it supports MPN search, manufacturer name search, semantic/functional queries, and lexical keyword matching. Use descriptive queries freely: `"buck converter 3.3V"`, `"Texas Instruments LDO"`, `"USB-C connector"`.
- **Web components search is strict MPN-only.** Only use exact or partial manufacturer part numbers: `"TPS54331"`, `"STM32F103C8T6"`. Do NOT include descriptions, keywords, or functional terms in the query — they will cause the search to fail or return irrelevant results. Strip everything except the MPN.
- All commands output JSON with `-f json`. Parse results programmatically to evaluate options.
- Registry results include `dependencies` (what they use) and `dependents` (who uses them) for context.
- Web results include `model_availability` showing whether ECAD and STEP models are available. The same MPN may appear from multiple sources (DigiKey, CSE, LCSC) with different model availability; check all returned results before concluding models are unavailable.
- **Try multiple queries.** Parts go by different names — full MPN, base family, orderable variant, manufacturer alias. If the first search doesn't find what you need, try alternative names before giving up.
- Use `pcb doc --package <url>@<version>` to inspect a registry module's io/config interface before using it.

## Choosing Between Results

Pick when there's a clear winner. Present tradeoffs and ask only when genuinely ambiguous.

Selection heuristics in priority order:

1. **Functional fit** — does it meet the electrical requirements?
2. **ECAD + STEP availability** — strongly prefer results with both models available.
3. **Package** — prefer leadless packages (QFN, DFN, LGA, WLCSP) over leaded alternatives (SOIC, TSSOP, QFP) when multiple package options exist.
4. **Sourcing** — prefer in-stock parts. Check `availability` fields for stock counts and pricing.
5. **Source quality** — for web:components, prefer CSE source over LCSC.
6. **Registry adoption** — more `dependents` in registry results means more battle-tested.

## Using Registry Results

Registry modules and components (Flows 1 and 2) are used directly via `Module()` with the registry URL. Auto-dep handles `pcb.toml` updates automatically — just use the URL and build.

```python
# Reference design from registry:modules search
LDO = Module("github.com/diodeinc/registry/reference/AP2112Kx/AP2112Kx.zen")

LDO(
    name="LDO_3V3",
    VIN=vbus_5v0,
    VOUT=vdd_3v3,
    GND=gnd,
)
```

```python
# Component from registry:components search
TPS54331 = Module("github.com/diodeinc/registry/components/TPS54331D/TPS54331D.zen")
```

Use `pcb doc --package <url>@<version>` to check available io/config before wiring into a design.

## Importing Web Components

Web component results (Flow 3) require an import step before use.

1. Search: `pcb search -m web:components <MPN> -f json`
2. Pick a result and extract its `component_id`, `part_number`, and `manufacturer`.
3. Import:

```bash
pcb new component --component-id <ID> --part-number <MPN> --manufacturer <MFR>
```

This downloads the symbol, footprint, and STEP model, scans the datasheet, and generates a `.zen` file into `components/<manufacturer>/<mpn>/`. If the component already exists in the workspace, it skips and reports the existing path.

4. Use the imported component via `Module()` with the local workspace path:

```python
ESP32 = Module("./components/Espressif_Systems/ESP32-S3-WROOM-1-N16R8/ESP32-S3-WROOM-1-N16R8.zen")
```

## Command Reference

### Search

```bash
# Modules and reference designs (fast, local index)
pcb search -m registry:modules <query> -f json

# Pre-packaged components (fast, local index)
pcb search -m registry:components <query> -f json

# Web component database (network, slower, MPN-ONLY queries)
pcb search -m web:components <MPN> -f json
```

### Import

```bash
# Import a web component into the workspace
pcb new component --component-id <ID> [--part-number <MPN>] [--manufacturer <MFR>]
```

### Inspect

```bash
# Read a registry package's io/config interface
pcb doc --package <url>@<version>
```

## Verifying Sourcing with `pcb bom`

After adding components to a design, use `pcb bom` to check sourcing and availability:

```bash
pcb bom boards/MyBoard/MyBoard.zen -f json
```

The JSON output is a list of BOM entries, each with:
- `designator`, `mpn`, `manufacturer`, `package`, `value`, `description`
- `availability` — per-entry sourcing data:
  - `us` / `global` — regional summary with `price`, `stock`, `alt_stock`
  - `offers` — individual distributor offers with `region`, `distributor`, `stock`, `price`

### Fixing BOM issues

- **"No house cap/resistor found"** warnings during build mean no pre-qualified generic part matches the spec. Adjust the value, package, or voltage rating, or specify an explicit `part=Part(mpn=..., manufacturer=...)` where appropriate.
- **Low stock or no offers** — search for alternative parts using the component search flows above, then update the design.
- **Checking availability** — look at `stock` counts across regions. Parts with zero stock and only `alt_stock` may have long lead times.
