# Design Principles

This page captures the design philosophy used when evolving Zener and its
standard library. It is not part of the language specification.

## Language

Zener should bias toward one obvious way to write a design once the design
intent is known. That generally means a small set of general, composable
concepts rather than many narrow ones. Prefer powerful primitives over
opinionated features that have not yet proven their value, and if something can
live in the standard library or another library, it usually should not be part
of the language itself.

Zener should keep builtin language features to a minimum, stay close to
Starlark, and read as much like ordinary Python as possible. Its concepts
should also feel familiar to EEs coming from traditional ECAD tools. It should
minimize multiple sources of truth and other forms of repeated technical
description that can drift over time.

Types are useful, but they should be used conservatively. Use types for
semantic, stable, composable distinctions. Prefer imperative checks for
contextual rules, and be skeptical of modeling concepts as types when their
meaning depends on context or usage.

Zener is pre-1.0, so some churn is expected, but that churn should decrease
over time. The path to 1.0 should cut scope aggressively to stabilize a small,
durable core.

## Standard Library

Anything in the standard library becomes part of Zener's blessed vocabulary, so
the bar for inclusion should be high.

Standard library concepts should be broadly useful, mostly context-free, and
unambiguous enough to model consistently.

If the system cannot meaningfully use a distinction, it probably should not be
a stdlib type. The standard library should help designs converge on one
obvious representation, not introduce more competing ways to describe the same
thing.
