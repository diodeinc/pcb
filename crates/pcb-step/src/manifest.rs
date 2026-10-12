//! The scene's identity and metadata as JSON (`pcb-scene/1`, scene mm).

use serde_json::{Value, json};

use crate::Options;
use crate::board::{Board, Mount};
use crate::scene::{Scene, frame};

pub const SCHEMA: &str = "pcb-scene/1";

/// `instance` gives where the writer drew each coverage entry.
pub fn manifest(
    board: &Board,
    options: &Options,
    scene: &Scene,
    instance: &dyn Fn(usize) -> Option<Value>,
) -> Value {
    let frame = frame(board, options);
    let mut proxy = vec![false; board.footprints.len()];
    for p in &scene.proxies {
        proxy[p.footprint] = true;
    }
    let mut footprints: Vec<Value> = board
        .footprints
        .iter()
        .enumerate()
        .map(|(index, fp)| {
            let at = frame.point(fp.at);
            json!({
                "reference": fp.reference,
                "uuid": fp.uuid,
                "libId": fp.lib_id,
                "value": fp.value,
                "side": if fp.back { "back" } else { "front" },
                "at": [at.x, at.y],
                "rotation": fp.rotation,
                "dnp": fp.dnp,
                "mount": match fp.mount {
                    Mount::Smd => "smd",
                    Mount::ThroughHole => "through_hole",
                    Mount::Unspecified => "unspecified",
                },
                "excludeFromBom": fp.exclude_from_bom,
                "excludeFromPosFiles": fp.exclude_from_pos,
                "boardOnly": fp.board_only,
                "nets": fp.nets,
                "proxy": proxy[index],
                "models": [],
            })
        })
        .collect();
    for (index, entry) in scene.coverage.models.iter().enumerate() {
        let mut model = json!({
            "model": entry.model,
            "status": entry.status,
            "detail": entry.detail,
        });
        if let Some(instance) = instance(index) {
            model["instance"] = instance;
        }
        footprints[entry.footprint]["models"]
            .as_array_mut()
            .unwrap()
            .push(model);
    }
    let mut nets: Vec<(&str, u32)> = board.nets.iter().map(|(n, i)| (*n, *i)).collect();
    nets.sort_by_key(|(_, id)| *id);
    json!({
        "schema": SCHEMA,
        "board": {
            "thickness": board.thickness,
            "copperLayers": board.copper_layers,
            "copperZ": board.physical().copper_z,
            "nets": nets.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "syntheticOutline": scene.synthetic_outline,
        },
        "complete": scene.coverage.is_complete(),
        "footprints": footprints,
    })
}
