use std::io::{self, Write};

use comfy_table::{Cell, Color, Table};

use crate::Bom;

impl Bom {
    /// Write BOM as a formatted table to the given writer
    ///
    /// # Arguments
    /// * `writer` - Output destination
    pub fn write_table<W: Write>(&self, mut writer: W) -> io::Result<()> {
        // Print legend in a compact table with 2 columns
        writeln!(writer, "Legend:")?;
        let mut legend_table = Table::new();
        legend_table.load_preset(comfy_table::presets::NOTHING);
        legend_table.set_content_arrangement(comfy_table::ContentArrangement::Disabled);

        legend_table.add_row(vec![
            Cell::new("■").fg(Color::Green),
            Cell::new("Plenty available / easy to source"),
            Cell::new("  "),
            Cell::new("■").fg(Color::Blue),
            Cell::new("House component"),
        ]);
        legend_table.add_row(vec![
            Cell::new("■").fg(Color::Yellow),
            Cell::new("Limited inventory / harder to source"),
            Cell::new("  "),
            Cell::new("■").fg(Color::DarkGrey),
            Cell::new("DNP (Do Not Populate)"),
        ]);
        legend_table.add_row(vec![
            Cell::new("■").fg(Color::Red),
            Cell::new("No inventory / hard to source"),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
        ]);

        writeln!(writer, "{legend_table}")?;

        let mut table = Table::new();
        table.load_preset(comfy_table::presets::UTF8_FULL_CONDENSED);
        table.set_content_arrangement(comfy_table::ContentArrangement::DynamicFullWidth);

        let json: serde_json::Value = serde_json::from_str(&self.grouped_json()).unwrap();
        let mut entries: Vec<&serde_json::Value> = json.as_array().unwrap().iter().collect();

        // Sort entries: non-DNP first (sorted by first designator), then DNP items (sorted by first designator)
        entries.sort_by(|a, b| {
            let a_dnp = a.get("dnp").and_then(|v| v.as_bool()).unwrap_or(false);
            let b_dnp = b.get("dnp").and_then(|v| v.as_bool()).unwrap_or(false);

            // DNP status takes priority (non-DNP before DNP)
            match a_dnp.cmp(&b_dnp) {
                std::cmp::Ordering::Equal => {
                    // Within same DNP status, sort by first designator naturally
                    let a_first_designator = a["designators"]
                        .as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|d| d.as_str())
                        .unwrap_or("");

                    let b_first_designator = b["designators"]
                        .as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|d| d.as_str())
                        .unwrap_or("");

                    natord::compare(a_first_designator, b_first_designator)
                }
                other => other,
            }
        });

        for entry in entries {
            let designators_vec: Vec<&str> = entry["designators"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d.as_str().unwrap())
                .collect();

            // Designators already naturally sorted by BTreeSet<NaturalString>
            let qty = designators_vec.len();
            let designators = designators_vec.join(",");

            // Use first offer info if available, otherwise use base component info
            let (mpn, manufacturer) = entry
                .get("offers")
                .and_then(|o| o.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find(|offer| offer["distributor"].as_str() != Some("__AVL__"))
                })
                .map(|offer| {
                    (
                        offer["manufacturer_pn"].as_str().unwrap_or_default(),
                        offer["manufacturer"].as_str().unwrap_or_default(),
                    )
                })
                .unwrap_or_else(|| {
                    (
                        entry["mpn"].as_str().unwrap_or_default(),
                        entry["manufacturer"].as_str().unwrap_or_default(),
                    )
                });

            // Use description field if available, otherwise use value
            let description = entry["description"]
                .as_str()
                .or_else(|| entry["value"].as_str())
                .unwrap_or_default();

            // Check if this is DNP
            let is_dnp = entry.get("dnp").and_then(|v| v.as_bool()).unwrap_or(false);

            // Check if this is a house part (assign_house_resistor or assign_house_capacitor)
            let is_house_part = entry
                .get("matcher")
                .and_then(|m| m.as_str())
                .map(|m| m.starts_with("assign_house_"))
                .unwrap_or(false);

            // Check if missing MPN or manufacturer (hard to source)
            let is_hard_to_source = mpn.is_empty() || manufacturer.is_empty();

            // Create qty cell
            let qty_cell = if is_dnp {
                Cell::new(qty.to_string()).fg(Color::DarkGrey)
            } else {
                Cell::new(qty.to_string())
            };

            // Create cells - color designators based on sourcing, grey out DNP items
            let designators_cell = if is_dnp {
                Cell::new(designators.as_str()).fg(Color::DarkGrey)
            } else if is_house_part {
                Cell::new(designators.as_str()).fg(Color::Green)
            } else if is_hard_to_source {
                Cell::new(designators.as_str()).fg(Color::Red)
            } else {
                Cell::new(designators.as_str())
            };

            let mpn_cell = if is_dnp {
                Cell::new(mpn).fg(Color::DarkGrey)
            } else if is_house_part {
                Cell::new(mpn).fg(Color::Blue)
            } else {
                Cell::new(mpn)
            };

            let manufacturer_cell = if is_dnp {
                Cell::new(manufacturer).fg(Color::DarkGrey)
            } else {
                Cell::new(manufacturer)
            };

            let package_cell = if is_dnp {
                Cell::new(entry["package"].as_str().unwrap_or_default()).fg(Color::DarkGrey)
            } else {
                Cell::new(entry["package"].as_str().unwrap_or_default())
            };

            let description_cell = if is_dnp {
                Cell::new(description).fg(Color::DarkGrey)
            } else {
                Cell::new(description)
            };

            // Get alternatives
            let alternatives_str = entry
                .get("alternatives")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    if arr.is_empty() {
                        String::new()
                    } else {
                        arr.iter()
                            .filter_map(|alt| alt["mpn"].as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                })
                .unwrap_or_default();

            let alternatives_cell = if is_dnp {
                Cell::new(alternatives_str.as_str()).fg(Color::DarkGrey)
            } else if is_house_part {
                Cell::new(alternatives_str.as_str()).fg(Color::Blue)
            } else {
                Cell::new(alternatives_str.as_str())
            };

            table.add_row(vec![
                qty_cell,
                designators_cell,
                mpn_cell,
                alternatives_cell,
                manufacturer_cell,
                package_cell,
                description_cell,
            ]);
        }

        // Set headers
        table.set_header(vec![
            "Qty",
            "Designators",
            "MPN",
            "Alternatives",
            "Manufacturer",
            "Package",
            "Description",
        ]);

        writeln!(writer, "{table}")?;
        Ok(())
    }
}
