"""
Core data types for the lens-based layout synchronization system.
"""

from dataclasses import dataclass, field
from typing import Any, Dict, FrozenSet, Optional, Tuple
import uuid as uuid_module


@dataclass(frozen=True)
class EntityPath:
    """Hierarchical path identifying an entity. Immutable and hashable."""

    segments: Tuple[str, ...]

    @classmethod
    def from_string(cls, path: str) -> "EntityPath":
        if not path:
            return cls(segments=())
        return cls(segments=tuple(path.split(".")))

    def __str__(self) -> str:
        return ".".join(self.segments)

    def __bool__(self) -> bool:
        return len(self.segments) > 0

    def parent(self) -> Optional["EntityPath"]:
        if len(self.segments) <= 1:
            return None
        return EntityPath(segments=self.segments[:-1])

    def is_ancestor_of(self, other: "EntityPath") -> bool:
        return (
            len(other.segments) > len(self.segments)
            and other.segments[: len(self.segments)] == self.segments
        )

    def relative_to(self, ancestor: "EntityPath") -> Optional["EntityPath"]:
        if not ancestor.is_ancestor_of(self) and ancestor != self:
            return None
        suffix = self.segments[len(ancestor.segments) :]
        return EntityPath(segments=suffix)

    @property
    def name(self) -> str:
        return self.segments[-1] if self.segments else ""

    @property
    def depth(self) -> int:
        return len(self.segments)


@dataclass(frozen=True)
class EntityId:
    """Unique identifier for an entity, derived from path and fpid.

    For footprints, identity includes the FPID - changing FPID means
    the old entity is removed and a new one is added.

    For groups, fpid is empty string.
    """

    path: EntityPath
    fpid: str = ""
    uuid: str = field(default="")

    def __post_init__(self):
        if not self.uuid:
            # Include fpid in uuid computation for footprints
            key = f"{self.path}\0{self.fpid}"
            object.__setattr__(
                self,
                "uuid",
                str(uuid_module.uuid5(uuid_module.NAMESPACE_URL, key)),
            )

    def __str__(self) -> str:
        return str(self.path)

    def __hash__(self) -> int:
        return hash((self.path, self.fpid))

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, EntityId):
            return False
        return self.path == other.path and self.fpid == other.fpid

    @classmethod
    def from_string(cls, path: str, fpid: str = "") -> "EntityId":
        return cls(path=EntityPath.from_string(path), fpid=fpid)

    @property
    def kiid_uuid(self) -> str:
        """UUID for KIID_PATH matching (path only, no fpid).

        This is the UUID used in KiCad's KIID_PATH field, which is
        computed only from the hierarchical path.
        """
        return str(uuid_module.uuid5(uuid_module.NAMESPACE_URL, str(self.path)))


@dataclass(frozen=True)
class FootprintView:
    """View portion of a footprint - derived from SOURCE netlist."""

    entity_id: EntityId
    reference: str
    value: str
    fpid: str
    dnp: bool = False
    exclude_from_bom: bool = False
    exclude_from_pos: bool = False
    fields: Dict[str, str] = field(default_factory=dict)

    @property
    def path(self) -> EntityPath:
        return self.entity_id.path


@dataclass(frozen=True)
class GroupView:
    """View portion of a group - derived from SOURCE netlist."""

    entity_id: EntityId
    member_ids: Tuple[EntityId, ...]
    layout_path: Optional[str] = None

    @property
    def path(self) -> EntityPath:
        return self.entity_id.path


@dataclass(frozen=True)
class NetView:
    """View portion of a net - derived from SOURCE netlist."""

    name: str
    connections: Tuple[Tuple[EntityId, str], ...]
    kind: str = "Net"  # Net type kind (e.g., "Net", "Power", "Ground", "NotConnected")

    def has_connection_to(self, entity_id: EntityId) -> bool:
        return any(fp_id == entity_id for fp_id, _ in self.connections)


@dataclass(frozen=True)
class BoardView:
    """Complete View derived from SOURCE netlist."""

    footprints: Dict[EntityId, FootprintView] = field(default_factory=dict)
    groups: Dict[EntityId, GroupView] = field(default_factory=dict)
    nets: Dict[str, NetView] = field(default_factory=dict)
    # Pads that should be marked as no_connect in KiCad (from NotConnected nets)
    # Each pad gets a unique unconnected-(...) net name generated at apply time
    not_connected_pads: FrozenSet[Tuple[EntityId, str]] = field(
        default_factory=frozenset
    )


@dataclass(frozen=True)
class Position:
    """2D position in KiCad internal units (nanometers)."""

    x: int
    y: int

    def offset_by(self, dx: int, dy: int) -> "Position":
        return Position(x=self.x + dx, y=self.y + dy)

    def __add__(self, other: "Position") -> "Position":
        return Position(x=self.x + other.x, y=self.y + other.y)

    def __sub__(self, other: "Position") -> "Position":
        return Position(x=self.x - other.x, y=self.y - other.y)


@dataclass(frozen=True)
class FootprintComplement:
    """Complement portion of a footprint - user-authored placement."""

    position: Position
    orientation: float
    layer: str
    locked: bool = False
    reference_position: Optional[Position] = None
    reference_visible: bool = True
    value_position: Optional[Position] = None
    value_visible: bool = False

    def with_position(self, position: Position) -> "FootprintComplement":
        return FootprintComplement(
            position=position,
            orientation=self.orientation,
            layer=self.layer,
            locked=self.locked,
            reference_position=self.reference_position,
            reference_visible=self.reference_visible,
            value_position=self.value_position,
            value_visible=self.value_visible,
        )

    def with_locked(self, locked: bool) -> "FootprintComplement":
        return FootprintComplement(
            position=self.position,
            orientation=self.orientation,
            layer=self.layer,
            locked=locked,
            reference_position=self.reference_position,
            reference_visible=self.reference_visible,
            value_position=self.value_position,
            value_visible=self.value_visible,
        )


@dataclass(frozen=True)
class TrackComplement:
    """Complement for a track segment."""

    uuid: str
    start: Position
    end: Position
    width: int
    layer: str
    net_name: str = ""


@dataclass(frozen=True)
class ViaComplement:
    """Complement for a via."""

    uuid: str
    position: Position
    diameter: int
    drill: int
    via_type: str = "through"
    net_name: str = ""


@dataclass(frozen=True)
class ZoneComplement:
    """Complement for a copper zone."""

    uuid: str
    name: str
    outline: Tuple[Position, ...]
    layer: str
    priority: int = 0
    net_name: str = ""
    fill_settings: Dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class GraphicComplement:
    """Complement for a graphic element."""

    uuid: str
    graphic_type: str
    layer: str
    geometry: Dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class GroupComplement:
    """Complement for a group - routing and graphics within the group."""

    tracks: Tuple[TrackComplement, ...] = ()
    vias: Tuple[ViaComplement, ...] = ()
    zones: Tuple[ZoneComplement, ...] = ()
    graphics: Tuple[GraphicComplement, ...] = ()

    @property
    def is_empty(self) -> bool:
        return (
            len(self.tracks) == 0
            and len(self.vias) == 0
            and len(self.zones) == 0
            and len(self.graphics) == 0
        )


@dataclass(frozen=True)
class BoardComplement:
    """Complete Complement - all user-authored data from DEST."""

    footprints: Dict[EntityId, FootprintComplement] = field(default_factory=dict)
    groups: Dict[EntityId, GroupComplement] = field(default_factory=dict)

    def get_footprint_complement(
        self, entity_id: EntityId
    ) -> Optional[FootprintComplement]:
        return self.footprints.get(entity_id)

    def get_group_complement(self, entity_id: EntityId) -> Optional[GroupComplement]:
        return self.groups.get(entity_id)


def default_footprint_complement() -> FootprintComplement:
    """Default placement for a new footprint."""
    return FootprintComplement(
        position=Position(x=0, y=0),
        orientation=0.0,
        layer="F.Cu",
        locked=False,
    )


def default_group_complement() -> GroupComplement:
    """Default complement for a new group (empty routing)."""
    return GroupComplement(tracks=(), vias=(), zones=(), graphics=())
