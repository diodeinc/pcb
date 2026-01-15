#!/usr/bin/env python3
"""
Compare layout snapshot JSON files to identify structural vs positional diffs.

This script compares two layout snapshot files (or git refs) and categorizes
differences into:
- POSITION: Only x/y coordinate changes
- STRUCTURAL: Changes to footprints, groups, nets, references, etc.

Usage:
    # Compare current file against main branch
    python tools/compare_layout_snapshots.py main HEAD path/to/snapshot.snap

    # Compare two specific files
    python tools/compare_layout_snapshots.py --file old.json new.json

    # Compare all layout snapshots between branches
    python tools/compare_layout_snapshots.py main HEAD --all
"""

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Set, Tuple


@dataclass
class DiffResult:
    """Result of comparing two snapshots."""
    snapshot_name: str
    position_changes: List[str]
    structural_changes: List[str]
    
    @property
    def has_position_changes(self) -> bool:
        return len(self.position_changes) > 0
    
    @property
    def has_structural_changes(self) -> bool:
        return len(self.structural_changes) > 0
    
    @property
    def is_identical(self) -> bool:
        return not self.has_position_changes and not self.has_structural_changes


def get_file_from_git(ref: str, path: str) -> Optional[str]:
    """Get file contents from a git ref or working directory.
    
    If ref is "WORKDIR", reads from the actual filesystem.
    """
    if ref == "WORKDIR":
        try:
            return Path(path).read_text()
        except FileNotFoundError:
            return None
    
    try:
        result = subprocess.run(
            ["git", "show", f"{ref}:{path}"],
            capture_output=True,
            text=True,
            check=True,
        )
        return result.stdout
    except subprocess.CalledProcessError:
        return None


def parse_snapshot(content: str) -> Dict[str, Any]:
    """Parse a snapshot file (handles insta header)."""
    lines = content.split("\n")
    
    # Skip insta header (lines starting with ---, source:, expression:, assertion_line:)
    json_start = 0
    for i, line in enumerate(lines):
        if line.strip().startswith("{"):
            json_start = i
            break
    
    json_content = "\n".join(lines[json_start:])
    return json.loads(json_content)


def get_sort_key(item: Any) -> str:
    """Get a stable sort key for an item (footprint, group, track, etc.).
    
    For items with regenerated UUIDs (tracks, vias), use geometry + properties.
    For items with stable identities (footprints, groups), use name/reference.
    """
    if isinstance(item, dict):
        # For tracks/vias, use geometry + layer + net (NOT uuid since it's regenerated)
        if "start" in item and "end" in item:
            start = item.get("start") or {}
            end = item.get("end") or {}
            layer = item.get("layer", "")
            net_name = item.get("net_name", "")
            width = item.get("width", 0)
            if isinstance(start, dict) and isinstance(end, dict):
                # Create a geometry key that's position-independent by using differences
                return f"track_{layer}_{net_name}_{width}_{start.get('x', 0)}_{start.get('y', 0)}_{end.get('x', 0)}_{end.get('y', 0)}"
        
        # For zones, use layer + net_name + priority (NOT uuid)
        if "layer" in item and "priority" in item:
            layer = item.get("layer", "")
            net_name = item.get("net_name", "")
            priority = item.get("priority", 0)
            name = item.get("name", "")
            return f"zone_{layer}_{net_name}_{priority}_{name}"
        
        # For graphics/drawings, use type + layer + shape + geometry
        # Include start/end to distinguish drawings (even though positions change)
        if "type" in item and "layer" in item and "shape" in item:
            start = item.get("start") or {}
            end = item.get("end") or {}
            start_x = start.get("x", 0) if isinstance(start, dict) else 0
            start_y = start.get("y", 0) if isinstance(start, dict) else 0
            end_x = end.get("x", 0) if isinstance(end, dict) else 0
            end_y = end.get("y", 0) if isinstance(end, dict) else 0
            return f"drawing_{item.get('layer')}_{item.get('type')}_{item.get('shape')}_{start_x}_{start_y}_{end_x}_{end_y}"
        
        # For footprints and groups, prefer name/reference (stable identity)
        if "name" in item:
            return f"named_{item['name']}"
        if "reference" in item:
            return f"ref_{item['reference']}"
        
        # Fall back to uuid if nothing else matches
        if "uuid" in item:
            return f"uuid_{item['uuid']}"
            
    return str(item)


def normalize_for_comparison(obj: Any, strip_positions: bool = False, sort_arrays: bool = True) -> Any:
    """Normalize an object for comparison.
    
    If strip_positions is True, replace all position/coordinate values with None.
    If sort_arrays is True, sort arrays by a stable key (uuid, name, etc.) to ignore ordering.
    """
    if isinstance(obj, dict):
        result = {}
        for k, v in obj.items():
            if strip_positions and k in ("x", "y", "position", "left", "right", "top", "bottom"):
                if k == "position":
                    result[k] = normalize_for_comparison(v, strip_positions=True, sort_arrays=sort_arrays)
                else:
                    result[k] = None  # Placeholder for position values
            else:
                result[k] = normalize_for_comparison(v, strip_positions, sort_arrays)
        return result
    elif isinstance(obj, list):
        normalized = [normalize_for_comparison(item, strip_positions, sort_arrays) for item in obj]
        # Sort arrays of dicts by a stable key to ignore ordering differences
        if sort_arrays and normalized and isinstance(normalized[0], dict):
            try:
                normalized = sorted(normalized, key=get_sort_key)
            except TypeError:
                pass  # If sorting fails, keep original order
        return normalized
    else:
        return obj


def _get_drawing_signature(drawing: Dict[str, Any]) -> str:
    """Get a position-independent signature for a drawing."""
    return f"{drawing.get('type')}_{drawing.get('layer')}_{drawing.get('shape')}_{drawing.get('width')}"


def _compare_drawings_as_multiset(
    old: List[Dict], 
    new: List[Dict], 
    path: str
) -> Tuple[List[str], List[str]]:
    """Compare drawings as multisets by structural properties (ignoring positions)."""
    from collections import Counter
    
    old_sigs = Counter(_get_drawing_signature(d) for d in old)
    new_sigs = Counter(_get_drawing_signature(d) for d in new)
    
    structural_changes = []
    
    # Check for missing/added signatures
    for sig in old_sigs - new_sigs:
        count = (old_sigs - new_sigs)[sig]
        structural_changes.append(f"{path}: {count} drawing(s) with signature '{sig}' removed")
    
    for sig in new_sigs - old_sigs:
        count = (new_sigs - old_sigs)[sig]
        structural_changes.append(f"{path}: {count} drawing(s) with signature '{sig}' added")
    
    # All position changes within drawings are just position changes
    position_changes = [f"{path}: {len(old)} drawings have position changes"]
    
    return position_changes if not structural_changes else [], structural_changes


def find_differences(
    old: Any, 
    new: Any, 
    path: str = "",
    position_keys: Set[str] = None,
) -> Tuple[List[str], List[str]]:
    """Find differences between two objects.
    
    Returns (position_changes, structural_changes).
    """
    if position_keys is None:
        position_keys = {"x", "y", "left", "right", "top", "bottom"}
    
    position_changes = []
    structural_changes = []
    
    if type(old) != type(new):
        structural_changes.append(f"{path}: type changed from {type(old).__name__} to {type(new).__name__}")
        return position_changes, structural_changes
    
    if isinstance(old, dict):
        all_keys = set(old.keys()) | set(new.keys())
        for key in sorted(all_keys):
            new_path = f"{path}.{key}" if path else key
            
            if key not in old:
                structural_changes.append(f"{new_path}: added")
            elif key not in new:
                structural_changes.append(f"{new_path}: removed")
            else:
                pos, struct = find_differences(old[key], new[key], new_path, position_keys)
                position_changes.extend(pos)
                structural_changes.extend(struct)
                
    elif isinstance(old, list):
        # Special handling for drawings arrays - compare as multisets
        if path.endswith(".drawings") and old and isinstance(old[0], dict) and "type" in old[0]:
            pos, struct = _compare_drawings_as_multiset(old, new, path)
            position_changes.extend(pos)
            structural_changes.extend(struct)
        elif len(old) != len(new):
            structural_changes.append(f"{path}: list length changed from {len(old)} to {len(new)}")
        else:
            for i, (o, n) in enumerate(zip(old, new)):
                pos, struct = find_differences(o, n, f"{path}[{i}]", position_keys)
                position_changes.extend(pos)
                structural_changes.extend(struct)
                
    else:
        if old != new:
            # Check if this is a position-related key
            key_name = path.split(".")[-1] if "." in path else path
            # Also check parent for "position" context
            is_position = key_name in position_keys or ".position." in path or path.endswith(".position")
            
            if is_position:
                position_changes.append(f"{path}: {old} -> {new}")
            else:
                structural_changes.append(f"{path}: {old} -> {new}")
    
    return position_changes, structural_changes


def compare_snapshots(old_content: str, new_content: str, name: str) -> DiffResult:
    """Compare two snapshot contents."""
    try:
        old_data = parse_snapshot(old_content)
        new_data = parse_snapshot(new_content)
        
        # Normalize both snapshots - sort arrays by stable keys (uuid, name, etc.)
        # This ensures we compare semantically equivalent data regardless of array ordering
        old_data = normalize_for_comparison(old_data, strip_positions=False, sort_arrays=True)
        new_data = normalize_for_comparison(new_data, strip_positions=False, sort_arrays=True)
    except json.JSONDecodeError as e:
        return DiffResult(
            snapshot_name=name,
            position_changes=[],
            structural_changes=[f"JSON parse error: {e}"],
        )
    
    position_changes, structural_changes = find_differences(old_data, new_data)
    
    return DiffResult(
        snapshot_name=name,
        position_changes=position_changes,
        structural_changes=structural_changes,
    )


def find_layout_snapshots() -> List[str]:
    """Find all layout snapshot files."""
    result = subprocess.run(
        ["git", "ls-files", "*.layout.json.snap"],
        capture_output=True,
        text=True,
    )
    return [f for f in result.stdout.strip().split("\n") if f]


def main():
    parser = argparse.ArgumentParser(
        description="Compare layout snapshots to identify structural vs positional diffs"
    )
    parser.add_argument("old_ref", nargs="?", default="main", help="Old git ref (default: main)")
    parser.add_argument("new_ref", nargs="?", default="HEAD", help="New git ref (default: HEAD)")
    parser.add_argument("path", nargs="?", help="Specific snapshot path to compare")
    parser.add_argument("--all", action="store_true", help="Compare all layout snapshots")
    parser.add_argument("--file", action="store_true", help="Treat refs as file paths instead of git refs")
    parser.add_argument("--verbose", "-v", action="store_true", help="Show all position changes")
    
    args = parser.parse_args()
    
    if args.file:
        # Compare two files directly
        if not args.path:
            print("Error: Need two file paths with --file", file=sys.stderr)
            return 1
        
        old_content = Path(args.old_ref).read_text()
        new_content = Path(args.path).read_text()
        results = [compare_snapshots(old_content, new_content, args.path)]
        
    elif args.all:
        # Compare all layout snapshots
        snapshots = find_layout_snapshots()
        results = []
        
        for snap in snapshots:
            old_content = get_file_from_git(args.old_ref, snap)
            new_content = get_file_from_git(args.new_ref, snap)
            
            if old_content is None and new_content is None:
                continue
            elif old_content is None:
                results.append(DiffResult(snap, [], [f"NEW FILE"]))
            elif new_content is None:
                results.append(DiffResult(snap, [], [f"DELETED"]))
            else:
                results.append(compare_snapshots(old_content, new_content, snap))
                
    elif args.path:
        # Compare specific snapshot
        old_content = get_file_from_git(args.old_ref, args.path)
        new_content = get_file_from_git(args.new_ref, args.path)
        
        if old_content is None:
            print(f"Error: Could not find {args.path} in {args.old_ref}", file=sys.stderr)
            return 1
        if new_content is None:
            print(f"Error: Could not find {args.path} in {args.new_ref}", file=sys.stderr)
            return 1
            
        results = [compare_snapshots(old_content, new_content, args.path)]
    else:
        parser.print_help()
        return 1
    
    # Print results
    has_structural = False
    
    for result in results:
        if result.is_identical:
            continue
            
        print(f"\n{'='*60}")
        print(f"📁 {result.snapshot_name}")
        print(f"{'='*60}")
        
        if result.has_structural_changes:
            has_structural = True
            print(f"\n⚠️  STRUCTURAL CHANGES ({len(result.structural_changes)}):")
            for change in result.structural_changes[:20]:  # Limit output
                print(f"    {change}")
            if len(result.structural_changes) > 20:
                print(f"    ... and {len(result.structural_changes) - 20} more")
        
        if result.has_position_changes:
            print(f"\n📍 Position changes: {len(result.position_changes)}")
            if args.verbose:
                for change in result.position_changes[:50]:
                    print(f"    {change}")
                if len(result.position_changes) > 50:
                    print(f"    ... and {len(result.position_changes) - 50} more")
    
    # Summary
    print(f"\n{'='*60}")
    print("SUMMARY")
    print(f"{'='*60}")
    
    total_position = sum(len(r.position_changes) for r in results)
    total_structural = sum(len(r.structural_changes) for r in results)
    files_with_changes = sum(1 for r in results if not r.is_identical)
    files_with_structural = sum(1 for r in results if r.has_structural_changes)
    
    print(f"Files compared: {len(results)}")
    print(f"Files with changes: {files_with_changes}")
    print(f"Files with STRUCTURAL changes: {files_with_structural}")
    print(f"Total position changes: {total_position}")
    print(f"Total structural changes: {total_structural}")
    
    if has_structural:
        print("\n❌ STRUCTURAL CHANGES DETECTED - Review carefully!")
        return 1
    elif total_position > 0:
        print("\n✅ Only position changes detected (expected with placement algorithm changes)")
        return 0
    else:
        print("\n✅ No changes detected")
        return 0


if __name__ == "__main__":
    sys.exit(main())
