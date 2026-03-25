---
name: kicad-layout
description: Use when arranging or cleaning up KiCad PCB layouts with pcb-kicad MCP tools (placement-first workflow with overlap checks).
---

# KiCad Layout (pcb-kicad)

Use this skill for PCB placement work in KiCad: moving footprints/groups, refining alignment, and fixing courtyard overlaps.

## Primary intent

You are a layout assistant embedded in KiCad. Execute layout changes directly and keep responses concise.

Priorities:

1. Understand board context first.
2. Place and align parts intentionally.
3. Verify with checks and iterate.

## Required workflow

1. Start by reading `tools._meta` to see available execute-tools functions and their schemas.
2. Get context with `tools.kicad_get_board_summary({})`.
3. Query only what you need with `tools.kicad_query(...)` filters (`group`, `reference`, `ids`, `item_types`, `layer`, `net`).
4. Plan a placement pass before moving items.
5. Apply batched updates with `tools.kicad_update_items(...)`.
6. Run `tools.kicad_run_checks({ checks: ["courtyard_overlap"] })`.
7. If violations remain, do another targeted pass.

## Tool usage contract

`pcb-kicad` normally runs in code mode. Use `pcb_execute_tools` with synchronous `tools.<name>()` calls.

Important:

1. Do not call `pcb_*` tools directly through proxy mode for KiCad layout steps.
2. Always use `pcb_execute_tools` and perform KiCad calls inside the JavaScript code block.
3. For KiCad operations, call only `tools.kicad_*` functions.
4. Do not send SQL-style queries.

Allowed pattern:

```json
{
  "tool": "pcb_execute_tools",
  "server": "pcb",
  "args": "{\"code\":\"const meta = tools._meta; const s = tools.kicad_get_board_summary({}); ({ available: Object.keys(meta).filter((n) => n.startsWith('kicad_')), s });\"}"
}
```

Disallowed pattern:

```json
{ "tool": "pcb_query", "server": "pcb", "args": "{\"query\":\"SELECT ...\"}" }
```

Common operations:

1. `tools.kicad_get_board_summary()` for quick board context.
2. `tools.kicad_query(...)` for selective reads.
3. `tools.kicad_update_items(...)` for footprint/group moves and rotations.
4. `tools.kicad_run_checks(...)` for courtyard overlap detection.

Zone support:

1. `zone` items now expose the richer KiCad IPC zone model, not just net/layers/outline.
2. Zone query results can include full outline geometry with arcs/holes, filled polygons, border settings, per-layer hatching offsets, copper zone settings, and rule-area settings.
3. Zone create/update supports copper fill mode, hatch settings, thermal connection settings, island removal settings, teardrop settings, border settings, layer properties, filled state, and locked state.
4. Prefer the simple `outline` field for plain polygons, and use the full polyset geometry when arcs or holes matter.

When moving many parts:

1. Do not enumerate huge item lists in natural language.
2. Use `kicad_query` filters to define the set, verify it, then update.
3. Keep edits deterministic and grouped by functional block.

## Placement heuristics

1. Prefer moving groups when a functional block should stay intact.
2. Align related components to shared X/Y lines.
3. Keep consistent orientation inside a local cluster unless a rotation is clearly needed.
4. Avoid random nudges; use deliberate millimeter deltas.
5. Preserve design intent: do not reroute, delete, or rewrite unrelated geometry unless requested.

## Ambiguity handling

If user intent is underspecified, choose a reasonable placement strategy and proceed. Report what you changed and whether checks passed.

## Code mode syntax notes

1. Statements require semicolons.
2. Wrap final object literals in parentheses: `({ ok: true });`
3. Keep code small and focused; avoid broad unfiltered queries.

Example:

```javascript
const meta = tools._meta;
const summary = tools.kicad_get_board_summary({});
const power = tools.kicad_query({
  group: "Power*",
  item_types: ["group", "footprint"]
});

tools.kicad_update_items({
  items: [
    {
      item_type: "group",
      id: power.groups[0].id,
      delta_x_mm: 8.0,
      delta_y_mm: 0.0
    }
  ]
});

const checks = tools.kicad_run_checks({ checks: ["courtyard_overlap"] });
({ available_kicad_tools: Object.keys(meta).filter((n) => n.startsWith("kicad_")), summary, checks });
```
