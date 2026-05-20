use super::*;

#[derive(Debug, Clone)]
pub(super) struct ImportSchematicPositionComment {
    pub(super) at: ImportSchematicAt,
    pub(super) unit: Option<i64>,
    pub(super) mirror: Option<String>,
    pub(super) lib_name: Option<String>,
    pub(super) lib_id: Option<KiCadLibId>,
    pub(super) target_kind: ImportSchematicTargetKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImportSchematicTargetKind {
    GenericResistor,
    GenericCapacitor,
    Other,
}

impl ImportSchematicTargetKind {
    pub(super) fn from_module_path(module_path: &str) -> Self {
        match module_path {
            "@stdlib/generics/Resistor.zen" => Self::GenericResistor,
            "@stdlib/generics/Capacitor.zen" => Self::GenericCapacitor,
            _ => Self::Other,
        }
    }

    pub(super) fn promoted_target_pin_axis_deg(self) -> Option<f64> {
        match self {
            Self::GenericResistor | Self::GenericCapacitor => Some(90.0),
            Self::Other => None,
        }
    }

    pub(super) fn promoted_target_pin_1_to_2_deg(self) -> Option<f64> {
        // In the promoted stdlib passive symbols, pin 1 sits at positive local Y and
        // pin 2 at negative local Y, so pin1->pin2 points toward -Y (270 degrees).
        match self {
            Self::GenericResistor | Self::GenericCapacitor => Some(270.0),
            Self::Other => None,
        }
    }

    pub(super) fn promoted_target_lib_id(self) -> Option<KiCadLibId> {
        match self {
            Self::GenericResistor => Some(KiCadLibId::from("Device:R".to_string())),
            Self::GenericCapacitor => Some(KiCadLibId::from("Device:C".to_string())),
            Self::Other => None,
        }
    }
}
