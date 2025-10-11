# TypeInstanceId Mismatch Issue

## Problem

Child modules are evaluated **twice** during module instantiation, causing enum type mismatches.

## Reproduction

### Failing Case

**child.zen**:
```python
Package = enum("A", "B", "C")
package = config("PACKAGE", Package, default=Package("A"))

def accept_package(package: Package):
    print(package)

accept_package(package)
```

**parent.zen**:
```python
load("child.zen", "Package")
Child = Module("child.zen")

Child(name="CHILD", PACKAGE=Package("A"))
```

**Error**:
```
Value `Package("A")` of type `enum` does not match the type annotation `Package` for argument `package`
```

### What Happens

1. **First Eval** (when `Module("child.zen")` is called):
   - Evaluates child.zen to populate `frozen_module` for type introspection
   - Creates `Package = enum(...)` with **TypeInstanceId=X**
   - Parent accesses this via `load()` or `Child.Package`

2. **Parent Uses Type**:
   - `Package("A")` creates value with **TypeInstanceId=X**

3. **Second Eval** (when `Child(...)` is called):
   - Re-evaluates child.zen with actual inputs
   - Creates **NEW** `Package = enum(...)` with **TypeInstanceId=Y**
   - Input value has TypeInstanceId=X, but child expects TypeInstanceId=Y
   - Starlark's runtime type checker rejects the mismatch

## Why This Happens

Each `enum()` call generates a fresh `TypeInstanceId`. Evaluating the same file twice creates two different enum types, even with identical definitions.

## Working Case

When the enum is defined in a separate file and both parent and child `load()` it:

**type.zen**: `Package = enum("A", "B", "C")`  
**parent.zen**: `load("type.zen", "Package")`  
**child.zen**: `load("type.zen", "Package")`

Both get the **same frozen enum** from type.zen → same TypeInstanceId → works!

## PhysicalValue Doesn't Have This Issue

PhysicalValue caches `TypeInstanceId` globally by unit:

```rust
static CACHE: OnceLock<Mutex<HashMap<PhysicalUnitDims, TypeInstanceId>>> = OnceLock::new();

fn type_instance_id(&self) -> TypeInstanceId {
    *CACHE.get_or_init(|| Mutex::new(HashMap::new()))
        .lock().unwrap()
        .entry(self.unit)
        .or_insert_with(TypeInstanceId::r#gen)
}
```

When `Voltage()` is called multiple times across different evaluations, it returns the **same TypeInstanceId**.

## Root Cause

Starlark's `enum()` generates a new TypeInstanceId on each call. We evaluate child modules twice, creating duplicate enum definitions with different identities.

## Test Cases

### 1. Failing Case - Enum Defined in Child Module

```bash
cargo run -- build repro/failing/parent.zen
```

**Result**: ❌ Type mismatch error

**What it shows**: When parent loads `Package` from child.zen and child also defines `Package`, the two evaluations of child.zen create different TypeInstanceIds.

**Files**:
- `failing/child.zen` - Defines `Package` enum and uses it in type annotation
- `failing/parent.zen` - Loads `Package` from child, instantiates Child module

### 2. Working Case - Enum Defined in Shared File

```bash
cargo run -- build repro/working/parent.zen
```

**Result**: ✅ Works correctly

**What it shows**: When both parent and child `load()` from the same type.zen file, they get the **same frozen enum** with the **same TypeInstanceId**.

**Files**:
- `working/type.zen` - Defines `Package` enum (single source of truth)
- `working/child.zen` - Loads `Package` from type.zen
- `working/parent.zen` - Loads `Package` from type.zen, instantiates Child

### 3. PhysicalValue Test - Cached TypeInstanceId

```bash
cargo run -- build repro/test_physical/parent.zen
```

**Result**: ✅ Works correctly

**What it shows**: PhysicalValue uses a global cache for TypeInstanceId (keyed by unit). Even though parent and child both call `builtin.Voltage()` in separate evaluations, they get the **same TypeInstanceId** from the cache.

**Files**:
- `test_physical/child.zen` - Defines `Voltage = builtin.Voltage()` and uses it
- `test_physical/parent.zen` - Defines `Voltage = builtin.Voltage()`, instantiates Child

**Key Difference**: PhysicalValue implements custom `type_instance_id()` method that returns cached ID based on unit definition, not creating new ID each call.

## Summary

| Test Case | Enum Source | Result | Why |
|-----------|-------------|--------|-----|
| failing/ | Defined in child.zen (evaluated twice) | ❌ Fails | Two evals → two TypeInstanceIds |
| working/ | Defined in type.zen (loaded once) | ✅ Works | One frozen enum → one TypeInstanceId |
| test_physical/ | PhysicalValue with cached ID | ✅ Works | Global cache → same TypeInstanceId |
