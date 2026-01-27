"""
Layout operation log for debugging and testing.

Records every action taken during apply_changeset for deterministic snapshot testing.
Each operation is a structured OpEvent serialized as one human-readable line.

Note: Renames (moved() paths) are handled in Rust preprocessing before
the Python sync runs. This module does not log rename operations.
"""

from dataclasses import dataclass, field
from typing import Any, Dict, List, Literal, Optional

from .changeset import format_line, parse_line


OpKind = Literal[
    "NET_ADD",
    "NET_REMOVE",
    "GR_REMOVE",
    "FP_REMOVE",
    "FP_ADD",
    "GR_ADD",
    "FP_REPLACE",
    "GR_MEMBER",
    "FRAG_TRACK",
    "FRAG_VIA",
    "FRAG_ZONE",
    "FRAG_GRAPHIC",
    "PLACE_FP",
    "PLACE_GR",
    "PLACE_FP_INHERIT",
]


@dataclass(frozen=True)
class OpEvent:
    """A single structured operation event."""

    kind: OpKind
    fields: Dict[str, Any] = field(default_factory=dict)

    def to_line(self) -> str:
        """Serialize to a single human-readable line."""
        return format_line(self.kind, self.fields)

    @classmethod
    def from_line(cls, line: str) -> "OpEvent":
        """Parse a single line back to OpEvent."""
        kind, fields = parse_line(line)
        return cls(kind=kind, fields=fields)


@dataclass
class OpLog:
    """Accumulates layout operations for debugging and testing."""

    events: List[OpEvent] = field(default_factory=list)

    def emit(self, event: OpEvent) -> None:
        """Append an event to the log."""
        self.events.append(event)

    # =========================================================================
    # Phase 1: Net operations
    # =========================================================================

    def net_add(self, name: str) -> None:
        self.emit(OpEvent(kind="NET_ADD", fields={"name": name}))

    def net_remove(self, name: str) -> None:
        self.emit(OpEvent(kind="NET_REMOVE", fields={"name": name}))

    # =========================================================================
    # Phase 2: Deletions
    # =========================================================================

    def gr_remove(self, path: str, items_deleted: int) -> None:
        self.emit(OpEvent(
            kind="GR_REMOVE",
            fields={"path": path, "items": items_deleted},
        ))

    def fp_remove(self, path: str) -> None:
        self.emit(OpEvent(kind="FP_REMOVE", fields={"path": path}))

    # =========================================================================
    # Phase 3: Additions
    # =========================================================================

    def fp_add(
        self,
        path: str,
        reference: str,
        fpid: str,
        value: str,
        x: int,
        y: int,
        layer: str = "",
        pad_count: int = 0,
    ) -> None:
        fields: Dict[str, Any] = {
            "path": path,
            "ref": reference,
            "fpid": fpid,
            "value": value,
            "x": x,
            "y": y,
        }
        if layer:
            fields["layer"] = layer
        if pad_count:
            fields["pads"] = pad_count
        self.emit(OpEvent(kind="FP_ADD", fields=fields))

    def gr_add(self, path: str) -> None:
        self.emit(OpEvent(kind="GR_ADD", fields={"path": path}))

    # =========================================================================
    # Phase 4: FPID Changes
    # =========================================================================

    def fp_replace(
        self,
        path: str,
        old_fpid: str,
        new_fpid: str,
        x: int,
        y: int,
        layer: str = "",
        pad_count: int = 0,
    ) -> None:
        fields: Dict[str, Any] = {
            "path": path,
            "old": old_fpid,
            "new": new_fpid,
            "x": x,
            "y": y,
        }
        if layer:
            fields["layer"] = layer
        if pad_count:
            fields["pads"] = pad_count
        self.emit(OpEvent(kind="FP_REPLACE", fields=fields))

    # =========================================================================
    # =========================================================================
    # Phase 4: Group Membership
    # =========================================================================

    def gr_member(self, group_path: str, member_paths: List[str]) -> None:
        self.emit(OpEvent(
            kind="GR_MEMBER",
            fields={"path": group_path, "members": sorted(member_paths)},
        ))

    # =========================================================================
    # Phase 6b: Fragment Routing
    # =========================================================================

    def frag_track(
        self,
        group_path: str,
        net_name: str,
        layer: str,
        start_x: int,
        start_y: int,
        end_x: int,
        end_y: int,
        width: int = 0,
    ) -> None:
        import math
        dx = end_x - start_x
        dy = end_y - start_y
        length = int(math.sqrt(dx * dx + dy * dy))
        fields: Dict[str, Any] = {
            "group": group_path,
            "net": net_name,
            "layer": layer,
            "x1": start_x,
            "y1": start_y,
            "x2": end_x,
            "y2": end_y,
            "len": length,
        }
        if width:
            fields["w"] = width
        self.emit(OpEvent(kind="FRAG_TRACK", fields=fields))

    def frag_via(
        self,
        group_path: str,
        net_name: str,
        x: int,
        y: int,
        drill: int = 0,
    ) -> None:
        fields: Dict[str, Any] = {
            "group": group_path,
            "net": net_name,
            "x": x,
            "y": y,
        }
        if drill:
            fields["drill"] = drill
        self.emit(OpEvent(kind="FRAG_VIA", fields=fields))

    def frag_zone(
        self, group_path: str, net_name: str, layer: str, name: str
    ) -> None:
        fields: Dict[str, Any] = {
            "group": group_path,
            "net": net_name,
            "layer": layer,
        }
        if name:
            fields["name"] = name
        self.emit(OpEvent(kind="FRAG_ZONE", fields=fields))

    def frag_graphic(
        self, group_path: str, graphic_type: str, layer: str
    ) -> None:
        self.emit(OpEvent(
            kind="FRAG_GRAPHIC",
            fields={"group": group_path, "type": graphic_type, "layer": layer},
        ))

    # =========================================================================
    # Phase 8: HierPlace
    # =========================================================================

    def place_fp(self, path: str, x: int, y: int, w: int = 0, h: int = 0) -> None:
        fields: Dict[str, Any] = {"path": path, "x": x, "y": y}
        if w and h:
            fields["w"] = w
            fields["h"] = h
        self.emit(OpEvent(kind="PLACE_FP", fields=fields))

    def place_gr(self, path: str, x: int, y: int, w: int = 0, h: int = 0) -> None:
        fields: Dict[str, Any] = {"path": path, "x": x, "y": y}
        if w and h:
            fields["w"] = w
            fields["h"] = h
        self.emit(OpEvent(kind="PLACE_GR", fields=fields))

    def place_fp_inherit(
        self,
        path: str,
        x: int,
        y: int,
        old_fpid: str,
        new_fpid: str,
    ) -> None:
        """Log position inheritance for FPID change."""
        self.emit(OpEvent(
            kind="PLACE_FP_INHERIT",
            fields={
                "path": path,
                "x": x,
                "y": y,
                "old_fpid": old_fpid,
                "new_fpid": new_fpid,
            },
        ))

    # =========================================================================
    # Serialization
    # =========================================================================

    def to_plaintext(self) -> str:
        """Serialize to plaintext - one line per event."""
        if not self.events:
            return ""
        lines = [event.to_line() for event in self.events]
        return "\n".join(lines) + "\n"

    def log_to(self, logger) -> None:
        """Log all events as INFO-level messages."""
        for event in self.events:
            logger.info(f"OPLOG {event.to_line()}")

    @classmethod
    def from_plaintext(cls, text: str) -> "OpLog":
        """Parse plaintext back to OpLog."""
        events: List[OpEvent] = []
        for line in text.strip().split("\n"):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            events.append(OpEvent.from_line(line))
        return cls(events=events)
