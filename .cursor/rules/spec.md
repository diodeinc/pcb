# Specification Documentation Rule

When making changes to the Zener language that affect:

- Language syntax
- Built-in functions
- Core types (Net, Component, Symbol, Interface, Module)
- Load resolution mechanisms
- Module system behavior
- Type system features
- Default behaviors or aliases

You MUST update `docs/spec.md` to reflect these changes. The specification should always be in sync with the implementation.

## Guidelines:

1. Add new features to the appropriate section
2. Update existing documentation if behavior changes
3. Include clear examples in Starlark syntax
4. Document both the feature and its parameters/options
5. If adding defaults (like package aliases), document both the defaults and how to override them
