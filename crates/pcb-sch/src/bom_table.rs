use std::io::{self, Write};

use comfy_table::{Cell, Color, Table};
use terminal_hyperlink::Hyperlink as _;
use urlencoding::encode as urlencode;

use crate::bom::availability::{is_small_generic_passive, tier_for_stock, Tier, NUM_BOARDS};
use crate::{AvailabilityData, Bom, GenericComponent};

/// Map availability tier to table cell color
fn color_for_tier(tier: Tier) -> Color {
    match tier {
        Tier::NoInventory => Color::Red,
        Tier::Limited => Color::Yellow,
        Tier::Plenty => Color::Green,
    }
}

/// Determine sourcing status tier from availability data
fn get_sourcing_tier(
    stock_data: Option<&AvailabilityData>,
    mpn: &str,
    manufacturer: &str,
    qty: usize,
    generic_data: Option<&GenericComponent>,
    package: Option<&str>,
) -> Tier {
    let Some(avail) = stock_data else {
        return if mpn.is_empty() || manufacturer.is_empty() {
            Tier::NoInventory
        } else {
            Tier::Plenty
        };
    };

    let stock = avail.stock_total;
    if stock == 0 {
        return Tier::NoInventory;
    }

    // Missing MPN/manufacturer makes it harder to source (Limited)
    if mpn.is_empty() || manufacturer.is_empty() {
        return Tier::Limited;
    }

    let is_small_passive = is_small_generic_passive(generic_data, package);
    tier_for_stock(stock, qty as i32, is_small_passive)
}

/// Create a hyperlink if the terminal supports it, otherwise return plain text
fn hyperlink(url: &str, text: &str) -> String {
    if supports_hyperlinks::on(supports_hyperlinks::Stream::Stdout) {
        text.hyperlink(url)
    } else {
        text.to_string()
    }
}

impl Bom {
    /// Write BOM as a formatted table to the given writer
    ///
    /// # Arguments
    /// * `writer` - Output destination
    pub fn write_table<W: Write>(&self, mut writer: W) -> io::Result<()> {
        let has_availability = !self.availability.is_empty();
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

            // Get path and availability data for this component
            let path = self
                .designators
                .iter()
                .find(|(_, d)| designators_vec.contains(&d.as_str()))
                .map(|(p, _)| p);

            let stock_data = if has_availability {
                path.and_then(|p| self.availability.get(p))
            } else {
                None
            };

            // Get generic_data and package for sourcing status
            let generic_data = entry
                .get("generic_data")
                .and_then(|gd| serde_json::from_value::<GenericComponent>(gd.clone()).ok());

            let package = entry.get("package").and_then(|p| p.as_str());

            let tier = get_sourcing_tier(
                stock_data,
                mpn,
                manufacturer,
                qty,
                generic_data.as_ref(),
                package,
            );

            // Create qty cell
            let qty_cell = if is_dnp {
                Cell::new(qty.to_string()).fg(Color::DarkGrey)
            } else {
                Cell::new(qty.to_string())
            };

            // Create cells - color designators based on sourcing tier, grey out DNP items
            let designators_cell = if is_dnp {
                Cell::new(designators.as_str()).fg(Color::DarkGrey)
            } else {
                Cell::new(designators.as_str()).fg(color_for_tier(tier))
            };

            // Make MPN clickable with Digikey search link
            let mpn_display = if mpn.is_empty() {
                String::new()
            } else {
                let digikey_url = format!(
                    "https://www.digikey.com/en/products/result?keywords={}",
                    urlencode(mpn)
                );
                hyperlink(&digikey_url, mpn)
            };

            let mpn_cell = if is_dnp {
                Cell::new(mpn_display).fg(Color::DarkGrey)
            } else if is_house_part {
                Cell::new(mpn_display).fg(Color::Blue)
            } else {
                Cell::new(mpn_display)
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

            // Build row with stock as 2nd column when availability is present
            let mut row = vec![qty_cell];

            // Add stock cell early if availability data is present
            let (stock_cell_opt, price_cell_opt, lcsc_cell_opt) = if has_availability {
                let (stock_cell, price_cell, lcsc_cell) = if let Some(path) = path {
                    if let Some(avail) = self.availability.get(path) {
                        // Stock: total or "-" if no data
                        let stock = avail.stock_total;
                        let stock_str = if stock == 0 && avail.price_single.is_none() {
                            "-".to_string()
                        } else {
                            stock.to_string()
                        };

                        // Price: "$X.XX ($Y.YY)" - unit price and total for NUM_BOARDS boards
                        let price_str = match (avail.price_single, avail.price_boards) {
                            (Some(unit), Some(total)) => {
                                format!("${:.2} (${:.2})", unit, total)
                            }
                            (Some(unit), None) => format!("${:.2}", unit),
                            _ => String::new(),
                        };

                        // Color stock cell based on availability
                        let qty_for_boards = (qty as i32) * NUM_BOARDS;
                        let stock_cell = if is_dnp {
                            Cell::new(stock_str).fg(Color::DarkGrey)
                        } else if stock == 0 {
                            Cell::new(stock_str).fg(Color::Red)
                        } else if stock < qty_for_boards {
                            Cell::new(stock_str).fg(Color::Yellow)
                        } else {
                            Cell::new(stock_str).fg(Color::Green)
                        };

                        let price_cell = if is_dnp || stock == 0 {
                            Cell::new(price_str).fg(Color::DarkGrey)
                        } else {
                            Cell::new(price_str)
                        };

                        // Make all LCSC part IDs clickable with their URLs
                        let lcsc_display = avail
                            .lcsc_part_ids
                            .iter()
                            .map(|(id, url)| hyperlink(url, id))
                            .collect::<Vec<_>>()
                            .join(", ");

                        let lcsc_cell = if is_dnp {
                            Cell::new(lcsc_display).fg(Color::DarkGrey)
                        } else {
                            Cell::new(lcsc_display).fg(Color::Grey)
                        };

                        (stock_cell, price_cell, lcsc_cell)
                    } else {
                        (Cell::new("-"), Cell::new(""), Cell::new(""))
                    }
                } else {
                    (Cell::new("-"), Cell::new(""), Cell::new(""))
                };
                (Some(stock_cell), Some(price_cell), Some(lcsc_cell))
            } else {
                (None, None, None)
            };

            // Add stock as 2nd column if available
            if let Some(stock_cell) = stock_cell_opt {
                row.push(stock_cell);
            }

            // Add rest of standard columns
            row.extend(vec![
                designators_cell,
                mpn_cell,
                manufacturer_cell,
                package_cell,
            ]);

            // Add remaining availability columns
            if let Some(lcsc_cell) = lcsc_cell_opt {
                row.push(lcsc_cell);
            }
            if let Some(price_cell) = price_cell_opt {
                row.push(price_cell);
            }

            row.push(description_cell);
            table.add_row(row);
        }

        // Set headers with Stock as 2nd column when available
        let price_header = format!("Price ({}x boards)", NUM_BOARDS);
        let mut headers = vec!["Qty"];

        if has_availability {
            headers.push("Stock");
        }

        headers.extend(vec!["Designators", "MPN", "Manufacturer", "Package"]);

        if has_availability {
            headers.push("LCSC");
            headers.push(&price_header);
        }

        headers.push("Description");

        table.set_header(headers);

        writeln!(writer, "{table}")?;
        Ok(())
    }
}
