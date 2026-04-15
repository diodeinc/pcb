# ENG-249: `io()` Template-First Design

## Summary

Add support for passing a net or interface template as the first positional
argument to `io()`.

Examples:

```zen
VDD = io(Power(voltage=Voltage("3.3V")))
BUS = io(MyInterface(clk=Net("CLK")))
```

This form is not just shorthand for `default=...`. A template acts as all of:

- the source of the placeholder type
- the source of the default/template metadata
- the source of implicit checks that constrain passed-in values

The long-term goal is to deprecate `default=` for `io()`. For implementation
simplicity, template-derived enforcement should also apply to
`io(type, default=template)` so both forms share the same code path.

## Goals

- Support `io(template)` for net templates and interface templates.
- Infer the placeholder type from the template.
- Preserve existing missing-input behavior.
- Reuse template/default metadata plumbing already used by `default=...`.
- Introduce `implicit_checks` as the abstraction for constraints implied by a
  template.
- In MVP, enforce voltage compatibility for typed net templates that carry a
  meaningful `voltage` property.
- Keep `input(...)` and `output(...)` as pure syntax sugar over `io(...)`.

## Non-Goals For MVP

- A generic property-compatibility engine for all net fields.
- Recursive implicit-check enforcement through interface templates.
- Rich template-origin metadata in module signature output.
- Special handling for partially provided interface values beyond the simplest
  working behavior.

## User-Facing Semantics

### Accepted Forms

`io()` accepts:

- a net type or interface factory
- a net template value or interface template value

Examples:

```zen
SIG = io(Net)
VDD = io(Power(voltage=Voltage("3.3V")))
BUS = io(I2c)
BUS = io(I2c(scl=Net("SCL"), sda=Net("SDA")))
```

### Template Meaning

When the first positional argument is a template:

- `typ` is inferred from the template value
- `default_value` metadata comes from the template
- `implicit_checks` are derived from the template

The template therefore behaves as a placeholder contract, not merely a fallback
value.

### `default=` Semantics

To reduce code paths, `io(type, default=template)` should use the same internal
representation and enforcement behavior as `io(template)`.

Examples:

```zen
VDD = io(Power(voltage=Voltage("3.3V")))
VDD = io(Power, default=Power(voltage=Voltage("3.3V")))
```

These should be treated equivalently by the runtime.

This implies a behavior change for legacy `default=` call sites, but it keeps
the implementation simpler and aligns old and new forms.

### Ambiguous Calls

These forms should be rejected early with a clear error suggesting removal of
`default=`:

```zen
io(Power(voltage=Voltage("3.3V")), default=Power("ALT"))
io("VDD", Power(voltage=Voltage("3.3V")), default=Power("ALT"))
```

### Checks Ordering

When an input value is provided, validation order is:

1. Type validation and implicit type conversion
2. Template-derived `implicit_checks`
3. Explicit `checks`

Implicit-check failures should look like ordinary explicit-check failures.

### Missing Input Behavior

Behavior remains unchanged:

- Required + non-strict + template present: instantiate from the template.
- Required + strict + template present: still error as missing input.
- Optional + template present: instantiate from the template, not `None`.

## MVP Implicit Checks

## Voltage Compatibility

For MVP, the only required implicit check is voltage compatibility on typed net
templates with a meaningful `voltage` property.

Rule:

- if the template net has a non-`None` voltage
- and the resolved input value is a compatible typed net
- then the passed-in net's voltage must be contained within the template
  voltage

This is semantically equivalent to:

```zen
voltage_within(template.voltage)
```

Examples:

```zen
VDD = io(Power(voltage=Voltage("1.71V - 3.6V")))
```

This implies that a provided `Power("VIN", voltage=Voltage("3.3V"))` is valid,
while `Power("VIN", voltage=Voltage("5V"))` is invalid.

If the template voltage is nominal-only, like `Voltage("3.3V")`, it should be
normalized the same way `voltage_within(...)` does and still participate in the
containment check.

If the template voltage is `None`, no implicit voltage check is generated.

If the passed-in net has `voltage=None` while the template requires a voltage
constraint, validation fails.

If the passed-in value fails type validation first, that error wins and
implicit checks do not run.

## Future Direction

MVP should include a code comment near the implicit-check derivation path noting
that this should eventually expand to:

```text
all overlapping fields must be compatible
```

## Interface Templates

`io(interface_template)` is part of the feature and should infer the placeholder
type from the interface template while preserving default/template metadata.

However, recursive implicit-check derivation through interface leaf nets is
follow-up work, not required for MVP.

The eventual design is:

- derive implicit checks recursively from constrained leaf nets
- only constrained leaves enforce anything
- preserve leaf paths in diagnostics where practical

But MVP may stop at:

- accepting interface templates
- inferring the interface type
- using the template for fallback/default metadata

## Naming Semantics

Template naming follows normal construction rules:

- if the template carries an explicit name, that explicit name wins for
  generated fallback values
- otherwise assignment inference applies as usual

If a provided external net is passed into `io(template)`, the template must not
rename the provided value.

The template only affects generated fallback values and validation.

## Signature And Metadata

In module signature metadata:

- `type_value` should reflect the inferred type, not the template instance
- `default_value` should continue to show the template instance

No new explicit marker is required in signature metadata to distinguish
template-first declarations from legacy `default=` declarations.

## Diagnostics

Implicit-check failures should be ordinary evaluation errors.

Example shape:

```text
Input 'VDD' voltage 5V is not within template voltage 1.71V - 3.6V
```

Diagnostics should primarily blame the call site that supplied the invalid
input. If practical, declaration context from the `io(...)` site may be attached
as related information, but this is secondary.

## Proposed Runtime Model

Internally, normalize `io()` declarations into:

- `placeholder_type`
- `template_value`
- `implicit_checks`
- `explicit_checks`
- `optional`
- `help`
- `direction`

The key design point is that template-derived behavior should be normalized
before the existing resolution path runs, rather than adding a separate
execution path for template-first `io()`.

## Proposed Resolution Flow

1. Parse arguments.
2. Detect whether the first positional arg is a type/factory or a template
   value.
3. Normalize to:
   - inferred placeholder type
   - template/default value, if any
   - derived `implicit_checks`, if any
4. Reject ambiguous combinations such as both a template positional argument and
   `default=...`.
5. Resolve provided input if present.
6. Run:
   - type validation and implicit conversion
   - implicit checks
   - explicit checks
7. If input is omitted, preserve current strict/non-strict/optional behavior and
   use the template/default path for generated fallback values.
8. Record parameter metadata using the inferred type and normalized default.

## Implementation Notes

### Reuse Opportunities

The implementation should borrow from existing template behavior:

- interface template cloning and naming in
  `crates/pcb-zen-core/src/lang/interface.rs`
- net instantiation from a base net in
  `crates/pcb-zen-core/src/lang/net.rs`
- existing parameter metadata recording in
  `crates/pcb-zen-core/src/lang/module.rs`

### Important Constraint

`InterfaceValue` already retains its factory, which makes interface-template
normalization straightforward.

`NetValue` does not retain its original `NetType` object, only its runtime type
name and properties. MVP may therefore either:

- reconstruct the effective placeholder type from the net value's type name, or
- extend net runtime metadata if needed

Prefer the smallest correct change.

### Voltage Check Implementation

Do not depend on calling the stdlib `voltage_within(...)` symbol directly from
Rust resolution code.

Instead, implement the MVP voltage check natively in the `implicit_checks`
derivation/execution path, while matching the semantics of
`stdlib/checks.zen:voltage_within`.

## MVP Implementation Plan

This section is intended to be kept up to date as implementation progresses.

## Status

- Phase 1: MVP functionality only - implemented
- Sign-off gate after Phase 1 - complete
- Phase 2: Manual smoke tests - complete
- Phase 3: Light automated coverage - pending
- Phase 4: Docs and example cleanup - pending

## Phase 1: MVP Functionality

This phase is the only implementation work to do first.

Do not start manual smoke tests, automated test coverage, or docs updates until
the MVP functionality is implemented and explicitly signed off on.

Concrete work:

- Add internal normalization for `io()` declarations so both `io(template)` and
  `io(type, default=template)` produce the same normalized representation.
- Introduce `implicit_checks` as a first-class internal abstraction alongside
  explicit `checks`.
- Extend `io()` argument handling to accept:
  - net template values
  - interface template values
- Infer placeholder type from template values.
- Reject ambiguous combinations where a template positional argument is used
  together with `default=...`, and suggest removing `default=`.
- Preserve current missing-input behavior while routing template-backed
  placeholders through the same generated-default path.
- Ensure metadata recording uses:
  - inferred type as `type_value`
  - normalized template/default value as `default_value`
- Implement MVP implicit-check derivation for typed net templates with a
  meaningful `voltage` property.
- Implement MVP implicit-check execution order:
  1. type validation and implicit conversion
  2. implicit checks
  3. explicit checks
- Make `io(type, default=template)` enforce the same implicit voltage checks as
  `io(template)`.
- Support interface templates for type inference and default/template metadata,
  but leave recursive interface implicit checks for follow-up.
- Add a code comment near the implicit-check derivation path noting the intended
  future rule: all overlapping fields must be compatible.

Phase 1 is complete when all of the following are true:

- `io(template)` works for net and interface templates
- placeholder type is inferred from the template
- template/default metadata is preserved
- implicit checks exist as a first-class internal abstraction
- typed net templates with a meaningful voltage property enforce voltage
  containment on provided inputs
- `io(type, default=template)` shares the same enforcement behavior
- recursive interface implicit checks are explicitly left for follow-up

## Sign-Off Gate

After Phase 1 is implemented, stop.

Do not move on to manual smoke tests, automated tests, examples, or docs until
the MVP functionality is reviewed and explicitly signed off on.

## Phase 2: Manual Smoke Tests

Only start this phase after sign-off on Phase 1.

Keep this light and focused on validating the new runtime behavior manually.

Suggested smoke tests:

- a net template form succeeds:
  - `VDD = io(Power(voltage=Voltage("3.3V")))`
- a template-backed provided input with compatible voltage succeeds
- a template-backed provided input with incompatible voltage fails
- `io(type, default=template)` behaves the same as `io(template)`
- an interface template is accepted and its type/default metadata path works

## Phase 3: Light Automated Coverage

Only start this phase after sign-off on Phase 1 and completion of the manual
smoke tests.

Keep automated coverage intentionally light.

Add exactly 2 new high-level snapshot tests:

- one high-level snapshot covering successful template-backed `io()` behavior
- one high-level snapshot covering failing template-enforced voltage
  compatibility

Avoid building a large matrix of unit tests for MVP. We expect broader coverage
to come from updating existing tests, examples, and docs to use templates where
appropriate later.

## Phase 4: Docs And Example Cleanup

Only start this phase after sign-off on Phase 1, completion of manual smoke
tests, and landing the two high-level snapshot tests.

This phase should include:

- updating user-facing docs where appropriate
- updating examples where template-first `io()` is the better form
- updating existing tests and fixtures where template-first `io()` is more
  appropriate

This phase is intentionally last. MVP functionality comes first.
