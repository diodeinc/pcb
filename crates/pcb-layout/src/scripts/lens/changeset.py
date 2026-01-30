"""
Changeset, serialization, and snapshot utilities for lens sync.

This module contains:
1. Serialization utilities (format_line, parse_line, etc.)
2. LensSnapshot for debugging before/after comparison
3. SyncChangeset - the interface between lens computation and application

Note: Renames (moved() paths) are handled in Rust preprocessing before
the Python sync runs. This module no longer tracks renames.
"""

from dataclasses import dataclass, field
from typing import Any, Dict, List, Literal, Optional, Set

from .types import (
    EntityId,
    EntityPath,
    Position,
    BoardView,
    BoardComplement,
    FootprintView,
    FootprintComplement,
    GroupView,
    GroupComplement,
    default_footprint_complement,
)


# =============================================================================
# Serialization Utilities
# =============================================================================


def format_value(value: Any) -> str:
    """Format a value for serialization."""
    if isinstance(value, str):
        if " " in value or "=" in value or '"' in value or "," in value or not value:
            escaped = value.replace("\\", "\\\\").replace('"', '\\"')
            return f'"{escaped}"'
        return value
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, list):
        formatted = ", ".join(format_value(v) for v in value)
        return f"[{formatted}]"
    return str(value)


def parse_value(s: str) -> Any:
    """Parse a serialized value."""
    s = s.strip()

    if s.startswith('"') and s.endswith('"'):
        inner = s[1:-1]
        return inner.replace('\\"', '"').replace("\\\\", "\\")

    if s.startswith("[") and s.endswith("]"):
        inner = s[1:-1].strip()
        if not inner:
            return []
        items = []
        for item in _split_list(inner):
            items.append(parse_value(item.strip()))
        return items

    if s == "true":
        return True
    if s == "false":
        return False

    try:
        if "." in s:
            return float(s)
        return int(s)
    except ValueError:
        return s


def _split_list(s: str) -> List[str]:
    """Split a comma-separated list, respecting quotes and brackets."""
    items = []
    current = ""
    in_quotes = False
    in_brackets = 0
    escape = False

    for c in s:
        if escape:
            current += c
            escape = False
        elif c == "\\":
            current += c
            escape = True
        elif c == '"':
            current += c
            in_quotes = not in_quotes
        elif c == "[" and not in_quotes:
            current += c
            in_brackets += 1
        elif c == "]" and not in_quotes:
            current += c
            in_brackets -= 1
        elif c == "," and not in_quotes and in_brackets == 0:
            items.append(current.strip())
            current = ""
        else:
            current += c

    if current.strip():
        items.append(current.strip())
    return items


def _tokenize(line: str) -> List[str]:
    """Tokenize a line into command and key=value pairs."""
    tokens = []
    current = ""
    in_quotes = False
    in_brackets = 0
    escape = False

    for c in line:
        if escape:
            current += c
            escape = False
        elif c == "\\":
            current += c
            escape = True
        elif c == '"':
            current += c
            in_quotes = not in_quotes
        elif c == "[" and not in_quotes:
            current += c
            in_brackets += 1
        elif c == "]" and not in_quotes:
            current += c
            in_brackets -= 1
        elif c == " " and not in_quotes and in_brackets == 0:
            if current:
                tokens.append(current)
                current = ""
        else:
            current += c

    if current:
        tokens.append(current)
    return tokens


def format_line(kind: str, fields: Dict[str, Any]) -> str:
    """Format a single line: KIND key=value key=value ..."""
    parts = [kind]
    for key, value in fields.items():
        parts.append(f"{key}={format_value(value)}")
    return " ".join(parts)


def parse_line(line: str) -> tuple:
    """Parse a single line into (kind, fields) tuple."""
    line = line.strip()
    if not line or line.startswith("#"):
        raise ValueError(f"Cannot parse empty or comment line: {line!r}")

    tokens = _tokenize(line)
    if not tokens:
        raise ValueError(f"No tokens in line: {line!r}")

    kind = tokens[0]
    fields: Dict[str, Any] = {}

    for token in tokens[1:]:
        if "=" not in token:
            raise ValueError(f"Invalid field (no '='): {token!r}")
        key, value_str = token.split("=", 1)
        fields[key] = parse_value(value_str)

    return kind, fields


# =============================================================================
# Snapshot Serialization
# =============================================================================


def _normalize_layout_path(path: str) -> str:
    """Normalize layout path for deterministic snapshots."""
    if not path:
        return path
    import os.path

    return os.path.basename(path.rstrip("/"))


def _serialize_footprint_view(fp: FootprintView) -> str:
    """Serialize a FootprintView to a single line."""
    fields: Dict[str, Any] = {
        "path": str(fp.path),
        "ref": fp.reference,
        "value": fp.value,
        "fpid": fp.fpid,
    }
    if fp.dnp:
        fields["dnp"] = True
    if fp.exclude_from_bom:
        fields["bom"] = False
    if fp.exclude_from_pos:
        fields["pos"] = False
    if fp.fields:
        sorted_fields = sorted(
            (k, v)
            for k, v in fp.fields.items()
            if k not in ("Path", "Datasheet", "Description") and v
        )
        if sorted_fields:
            fields["fields"] = [f"{k}={v}" for k, v in sorted_fields]
    return format_line("FPV", fields)


def _serialize_footprint_complement(
    entity_id: EntityId, fc: FootprintComplement
) -> str:
    """Serialize a FootprintComplement to a single line."""
    fields: Dict[str, Any] = {
        "path": str(entity_id.path),
        "x": fc.position.x,
        "y": fc.position.y,
        "orient": fc.orientation,
        "layer": fc.layer,
    }
    if fc.locked:
        fields["locked"] = True
    if fc.reference_position is not None:
        fields["ref_x"] = fc.reference_position.x
        fields["ref_y"] = fc.reference_position.y
    if not fc.reference_visible:
        fields["ref_vis"] = False
    if fc.value_position is not None:
        fields["val_x"] = fc.value_position.x
        fields["val_y"] = fc.value_position.y
    if fc.value_visible:
        fields["val_vis"] = True
    return format_line("FPC", fields)


def _serialize_group_view(gv: GroupView) -> str:
    """Serialize a GroupView to a single line."""
    members = sorted(str(m.path) for m in gv.member_ids)
    fields: Dict[str, Any] = {
        "path": str(gv.path),
        "members": members,
    }
    if gv.layout_path:
        fields["layout"] = _normalize_layout_path(gv.layout_path)
    return format_line("GRV", fields)


def _serialize_group_complement(entity_id: EntityId, gc: GroupComplement) -> List[str]:
    """Serialize a GroupComplement to multiple lines (one per routing item)."""
    lines: List[str] = []
    path = str(entity_id.path)

    for track in sorted(gc.tracks, key=lambda t: (t.layer, t.net_name, t.uuid)):
        fields: Dict[str, Any] = {
            "group": path,
            "net": track.net_name or "",
            "layer": track.layer,
            "x1": track.start.x,
            "y1": track.start.y,
            "x2": track.end.x,
            "y2": track.end.y,
            "w": track.width,
        }
        lines.append(format_line("GTR", fields))

    for via in sorted(gc.vias, key=lambda v: (v.net_name, v.uuid)):
        fields = {
            "group": path,
            "net": via.net_name or "",
            "x": via.position.x,
            "y": via.position.y,
            "drill": via.drill,
        }
        lines.append(format_line("GVIA", fields))

    for zone in sorted(gc.zones, key=lambda z: (z.layer, z.net_name, z.uuid)):
        fields = {
            "group": path,
            "net": zone.net_name or "",
            "layer": zone.layer,
            "name": zone.name,
        }
        lines.append(format_line("GZONE", fields))

    return lines


def serialize_view(view: BoardView) -> List[str]:
    """Serialize a BoardView to lines (footprints and groups only, not nets)."""
    lines: List[str] = []

    for entity_id in sorted(view.footprints.keys(), key=lambda e: str(e.path)):
        lines.append(_serialize_footprint_view(view.footprints[entity_id]))

    for entity_id in sorted(view.groups.keys(), key=lambda e: str(e.path)):
        lines.append(_serialize_group_view(view.groups[entity_id]))

    return lines


def serialize_complement(complement: BoardComplement) -> List[str]:
    """Serialize a BoardComplement to lines."""
    lines: List[str] = []

    for entity_id in sorted(complement.footprints.keys(), key=lambda e: str(e.path)):
        lines.append(
            _serialize_footprint_complement(entity_id, complement.footprints[entity_id])
        )

    for entity_id in sorted(complement.groups.keys(), key=lambda e: str(e.path)):
        gc = complement.groups[entity_id]
        if not gc.is_empty:
            lines.extend(_serialize_group_complement(entity_id, gc))

    return lines


# =============================================================================
# SyncChangeset
# =============================================================================


ChangeKind = Literal["add", "remove"]


@dataclass(frozen=True)
class FootprintChange:
    """A single footprint change for reporting/iteration."""

    kind: ChangeKind
    entity_id: EntityId


@dataclass(frozen=True)
class GroupChange:
    """A single group change for reporting/iteration."""

    kind: ChangeKind
    entity_id: EntityId


@dataclass
class SyncChangeset:
    """The complete sync plan - pure, serializable."""

    view: BoardView
    complement: BoardComplement

    added_footprints: Set[EntityId] = field(default_factory=set)
    removed_footprints: Dict[EntityId, FootprintComplement] = field(
        default_factory=dict
    )

    added_groups: Set[EntityId] = field(default_factory=set)
    removed_groups: Set[EntityId] = field(default_factory=set)

    @property
    def is_empty(self) -> bool:
        return (
            len(self.added_footprints) == 0
            and len(self.removed_footprints) == 0
            and len(self.added_groups) == 0
            and len(self.removed_groups) == 0
        )

    @property
    def footprint_changes(self) -> List[FootprintChange]:
        changes: List[FootprintChange] = []
        for eid in sorted(self.added_footprints, key=lambda e: str(e.path)):
            changes.append(FootprintChange(kind="add", entity_id=eid))
        for eid in sorted(self.removed_footprints.keys(), key=lambda e: str(e.path)):
            changes.append(FootprintChange(kind="remove", entity_id=eid))
        return changes

    @property
    def group_changes(self) -> List[GroupChange]:
        changes: List[GroupChange] = []
        for eid in sorted(self.added_groups, key=lambda e: str(e.path)):
            changes.append(GroupChange(kind="add", entity_id=eid))
        for eid in sorted(self.removed_groups, key=lambda e: str(e.path)):
            changes.append(GroupChange(kind="remove", entity_id=eid))
        return changes

    def to_plaintext(self) -> str:
        """Serialize to plaintext - one line per change, parseable."""
        lines: List[str] = []

        for change in self.footprint_changes:
            if change.kind == "add":
                fp = self.view.footprints[change.entity_id]
                comp = self.complement.footprints.get(change.entity_id)
                fields = {
                    "path": str(fp.path),
                    "ref": fp.reference,
                    "fpid": fp.fpid,
                    "value": fp.value,
                }
                if comp:
                    fields["x"] = comp.position.x
                    fields["y"] = comp.position.y
                    if comp.layer:
                        fields["layer"] = comp.layer
                lines.append(format_line("FP_ADD", fields))
            elif change.kind == "remove":
                comp = self.removed_footprints[change.entity_id]
                fields = {
                    "path": str(change.entity_id.path),
                    "fpid": change.entity_id.fpid,
                    "x": comp.position.x,
                    "y": comp.position.y,
                    "orient": comp.orientation,
                    "layer": comp.layer,
                }
                if comp.locked:
                    fields["locked"] = True
                lines.append(format_line("FP_REMOVE", fields))

        for change in self.group_changes:
            if change.kind == "add":
                group = self.view.groups[change.entity_id]
                fields = {
                    "path": str(group.path),
                    "members": len(group.member_ids),
                }
                if group.layout_path:
                    fields["fragment"] = True
                lines.append(format_line("GR_ADD", fields))
            elif change.kind == "remove":
                lines.append(
                    format_line("GR_REMOVE", {"path": str(change.entity_id.path)})
                )

        if not lines:
            return ""
        return "\n".join(lines) + "\n"

    @classmethod
    def from_plaintext(
        cls, text: str, view: BoardView, complement: BoardComplement
    ) -> "SyncChangeset":
        """Parse plaintext back to SyncChangeset."""
        added_footprints: Set[EntityId] = set()
        removed_footprints: Dict[EntityId, FootprintComplement] = {}
        added_groups: Set[EntityId] = set()
        removed_groups: Set[EntityId] = set()

        for line in text.strip().split("\n"):
            line = line.strip()
            if not line or line.startswith("#"):
                continue

            cmd, fields = parse_line(line)

            if cmd == "FP_ADD":
                eid = EntityId(
                    path=EntityPath.from_string(fields["path"]),
                    fpid=fields.get("fpid", ""),
                )
                added_footprints.add(eid)
            elif cmd == "FP_REMOVE":
                eid = EntityId(
                    path=EntityPath.from_string(fields["path"]),
                    fpid=fields.get("fpid", ""),
                )
                removed_footprints[eid] = FootprintComplement(
                    position=Position(x=int(fields["x"]), y=int(fields["y"])),
                    orientation=float(fields.get("orient", 0)),
                    layer=fields.get("layer", "F.Cu"),
                    locked=fields.get("locked", False),
                )
            elif cmd == "GR_ADD":
                eid = EntityId(path=EntityPath.from_string(fields["path"]))
                added_groups.add(eid)
            elif cmd == "GR_REMOVE":
                eid = EntityId(path=EntityPath.from_string(fields["path"]))
                removed_groups.add(eid)

        return cls(
            view=view,
            complement=complement,
            added_footprints=added_footprints,
            removed_footprints=removed_footprints,
            added_groups=added_groups,
            removed_groups=removed_groups,
        )

    def to_diagnostics(self) -> List[Dict[str, Any]]:
        """Convert to user-facing diagnostics."""
        diagnostics: List[Dict[str, Any]] = []

        for change in self.footprint_changes:
            path = str(change.entity_id.path)
            fpid = change.entity_id.fpid

            if change.kind == "add":
                fp_view = self.view.footprints.get(change.entity_id)
                ref = fp_view.reference if fp_view else change.entity_id.path.name
                diagnostics.append(
                    {
                        "kind": "layout.sync.missing_footprint",
                        "severity": "info",
                        "body": f"Footprint {ref} ({path}:{fpid}) will be added",
                        "path": path,
                        "reference": ref,
                    }
                )
            elif change.kind == "remove":
                # For removed footprints, we don't have the old view, so use path name
                ref = change.entity_id.path.name
                diagnostics.append(
                    {
                        "kind": "layout.sync.extra_footprint",
                        "severity": "warning",
                        "body": f"Footprint {ref} ({path}:{fpid}) will be removed",
                        "path": path,
                        "reference": ref,
                    }
                )

        return diagnostics


def build_sync_changeset(
    new_view: BoardView,
    new_complement: BoardComplement,
    old_complement: Optional[BoardComplement] = None,
) -> SyncChangeset:
    """Build a SyncChangeset by diffing new and old complements.

    Derives what was added/removed by comparing the keys of the complements.
    """
    old_fps = old_complement.footprints if old_complement else {}
    old_groups = old_complement.groups if old_complement else {}

    # Compute tracking by diffing complement keys
    new_fp_ids = set(new_complement.footprints.keys())
    old_fp_ids = set(old_fps.keys())
    added_footprints = new_fp_ids - old_fp_ids
    removed_fp_ids = old_fp_ids - new_fp_ids

    new_group_ids = set(new_complement.groups.keys())
    old_group_ids = set(old_groups.keys())
    added_groups = new_group_ids - old_group_ids
    removed_groups = old_group_ids - new_group_ids

    removed_footprints = {
        eid: old_fps.get(eid, default_footprint_complement()) for eid in removed_fp_ids
    }

    return SyncChangeset(
        view=new_view,
        complement=new_complement,
        added_footprints=added_footprints,
        removed_footprints=removed_footprints,
        added_groups=added_groups,
        removed_groups=removed_groups,
    )


def log_lens_state(
    prefix: str,
    view: BoardView,
    complement: BoardComplement,
    logger: Any,
) -> None:
    """Log view and complement state at INFO level.

    This replaces the old LensSnapshot serialization with inline logging.
    Each line is prefixed with OLD or NEW for diffing.
    """
    for line in serialize_view(view):
        logger.info(f"{prefix} {line}")
    for line in serialize_complement(complement):
        logger.info(f"{prefix} {line}")


def log_changeset(changeset: "SyncChangeset", logger: Any) -> None:
    """Log changeset as INFO-level messages."""
    text = changeset.to_plaintext()
    if text.strip():
        for line in text.strip().split("\n"):
            logger.info(f"CHANGESET {line}")
