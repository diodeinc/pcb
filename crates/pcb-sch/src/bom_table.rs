use std::io::{self, Write};

use colored::Colorize;
use comfy_table::{Cell, Color, Table};
use terminal_hyperlink::Hyperlink as _;
use urlencoding::encode as urlencode;

use crate::bom::availability::{is_small_generic_passive, tier_for_stock, Tier, NUM_BOARDS};
use crate::{Bom, GenericComponent};

/// Create a cell with quantity and percentage (percentage in grey)
fn qty_with_percentage_cell(qty: usize, percentage: f64) -> Cell {
    Cell::new(format!(
        "{:>4} {}",
        qty,
        format!("({:>5.1}%)", percentage).dimmed()
    ))
}

/// Fill in missing value from availability data, returning (value, is_autofilled)
fn autofill_from_availability<'a>(
    original: &'a str,
    availability: &'a Option<String>,
) -> (&'a str, bool) {
    if original.is_empty() {
        availability
            .as_ref()
            .map(|s| (s.as_str(), true))
            .unwrap_or((original, false))
    } else {
        (original, false)
    }
}

/// Apply dimmed+italic styling if autofilled
fn style_if_autofilled(value: String, is_autofilled: bool) -> String {
    if is_autofilled && !value.is_empty() {
        format!("{}", value.dimmed().italic())
    } else {
        value
    }
}

/// Configure a summary table with standard layout
fn configure_summary_table(table: &mut Table) {
    table.load_preset(comfy_table::presets::UTF8_FULL_CONDENSED);
    table.set_content_arrangement(comfy_table::ContentArrangement::Disabled);
    table.set_header(vec!["", "Category", "Unique Parts", "Total Qty"]);

    // Column 0: icon (content width)
    table
        .column_mut(0)
        .unwrap()
        .set_constraint(comfy_table::ColumnConstraint::ContentWidth);

    // Column 1: category (fixed 40 chars)
    table
        .column_mut(1)
        .unwrap()
        .set_constraint(comfy_table::ColumnConstraint::LowerBoundary(
            comfy_table::Width::Fixed(40),
        ));

    // Columns 2-3: right-aligned numeric columns (fixed 18 chars)
    for col_idx in 2..=3 {
        let col = table.column_mut(col_idx).unwrap();
        col.set_constraint(comfy_table::ColumnConstraint::LowerBoundary(
            comfy_table::Width::Fixed(18),
        ));
        col.set_cell_alignment(comfy_table::CellAlignment::Right);
    }
}

/// Create a summary row with icon, label, and two qty+percentage cells
fn summary_row(
    icon_color: Color,
    label: &str,
    count: usize,
    count_total: usize,
    qty: usize,
    qty_total: usize,
) -> Vec<Cell> {
    vec![
        Cell::new("■").fg(icon_color),
        Cell::new(label),
        qty_with_percentage_cell(count, (count as f64 / count_total as f64) * 100.0),
        qty_with_percentage_cell(qty, (qty as f64 / qty_total as f64) * 100.0),
    ]
}

/// Map availability tier to table cell color
fn color_for_tier(tier: Tier) -> Color {
    match tier {
        Tier::Insufficient => Color::Red,
        Tier::Limited => Color::Yellow,
        Tier::Plenty => Color::Green,
    }
}

/// Apply styling to a cell based on component flags
fn styled_cell(content: impl ToString, is_dnp: bool, is_house: bool, tier: Option<Tier>) -> Cell {
    let cell = Cell::new(content);
    match (is_dnp, is_house, tier) {
        (true, _, _) => cell.fg(Color::DarkGrey),
        (false, true, _) => cell.fg(Color::Blue),
        (false, false, Some(t)) => cell.fg(color_for_tier(t)),
        (false, false, None) => cell,
    }
}

/// Get designator tier (capped at Limited if MPN/manufacturer missing)
fn get_designator_tier(stock_tier: Tier, mpn: &str, manufacturer: &str) -> Tier {
    match (mpn.is_empty() || manufacturer.is_empty(), stock_tier) {
        (true, Tier::Plenty) => Tier::Limited,
        _ => stock_tier,
    }
}

/// Calculate unit price at a given quantity using price breaks
fn unit_price_from_breaks(price_breaks: &[(i32, f64)], qty: i32) -> Option<f64> {
    if price_breaks.is_empty() {
        return None;
    }

    // Find the highest quantity break that's <= our target quantity
    let mut best_break: Option<&(i32, f64)> = None;
    for pb in price_breaks {
        if pb.0 <= qty {
            if let Some(current_best) = best_break {
                if pb.0 > current_best.0 {
                    best_break = Some(pb);
                }
            } else {
                best_break = Some(pb);
            }
        }
    }

    // If no break applies, use the lowest quantity break
    if best_break.is_none() {
        best_break = price_breaks.iter().min_by_key(|pb| pb.0);
    }

    best_break.map(|pb| pb.1)
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
            Cell::new("Insufficient stock / hard to source"),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
        ]);

        writeln!(writer, "{legend_table}")?;

        // Track summary stats (only used when has_availability)
        let mut plenty_count = 0;
        let mut plenty_qty = 0;
        let mut limited_count = 0;
        let mut limited_qty = 0;
        let mut hard_count = 0;
        let mut hard_qty = 0;
        let mut dnp_count = 0;
        let mut dnp_qty = 0;
        let mut house_count = 0;
        let mut house_qty = 0;
        let mut non_house_count = 0;
        let mut non_house_qty = 0;

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

            // Get MPN/manufacturer from original entry
            let original_mpn = entry["mpn"].as_str().unwrap_or_default();
            let original_manufacturer = entry["manufacturer"].as_str().unwrap_or_default();

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

            // Get all paths for this grouped entry and aggregate availability
            let paths: Vec<&String> = self
                .designators
                .iter()
                .filter(|(_, d)| designators_vec.contains(&d.as_str()))
                .map(|(p, _)| p)
                .collect();

            // Get availability from best component (deterministic: prefer entries with price_breaks, then highest stock)
            let (
                aggregated_stock,
                aggregated_price_single,
                aggregated_price_boards,
                lcsc_ids,
                avail_mpn,
                avail_manufacturer,
                has_avail,
            ) = if has_availability {
                let best_avail = paths
                    .iter()
                    .filter_map(|path| self.availability.get(*path))
                    .max_by_key(|avail| (avail.price_breaks.is_some(), avail.stock_total));

                let (stock, price_breaks_data, lcsc_part_ids, offer_mpn, offer_mfr) =
                    match best_avail {
                        Some(avail) => (
                            avail.stock_total,
                            avail.price_breaks.clone(),
                            avail.lcsc_part_ids.clone(),
                            avail.mpn.clone(),
                            avail.manufacturer.clone(),
                        ),
                        None => (0, None, Vec::new(), None, None),
                    };

                // Recalculate prices using grouped quantity and price breaks
                let (price_single, price_boards) = if let Some(breaks) = price_breaks_data {
                    let unit_single = unit_price_from_breaks(&breaks, qty as i32);
                    let unit_boards = unit_price_from_breaks(&breaks, (qty as i32) * NUM_BOARDS);

                    let total_single = unit_single.map(|p| p * qty as f64);
                    let total_boards = unit_boards.map(|p| p * (qty as i32 * NUM_BOARDS) as f64);
                    (total_single, total_boards)
                } else {
                    (None, None)
                };

                (
                    stock,
                    price_single,
                    price_boards,
                    lcsc_part_ids,
                    offer_mpn,
                    offer_mfr,
                    best_avail.is_some(),
                )
            } else {
                (0, None, None, Vec::new(), None, None, false)
            };

            // Fill in missing MPN/manufacturer from availability data
            let (mpn, is_mpn_autofilled) = autofill_from_availability(original_mpn, &avail_mpn);
            let (manufacturer, is_manufacturer_autofilled) =
                autofill_from_availability(original_manufacturer, &avail_manufacturer);

            // Get generic_data and package for sourcing status
            let generic_data = entry
                .get("generic_data")
                .and_then(|gd| serde_json::from_value::<GenericComponent>(gd.clone()).ok());

            let package = entry.get("package").and_then(|p| p.as_str());

            // Calculate tier when we have availability data for this group
            let stock_tier = if has_avail {
                let is_small_passive = is_small_generic_passive(generic_data.as_ref(), package);
                tier_for_stock(aggregated_stock, qty as i32, is_small_passive)
            } else {
                Tier::Insufficient
            };
            let designator_tier = get_designator_tier(stock_tier, mpn, manufacturer);

            // Track summary stats
            if has_availability {
                if is_dnp {
                    dnp_count += 1;
                    dnp_qty += qty;
                } else {
                    match designator_tier {
                        Tier::Plenty => {
                            plenty_count += 1;
                            plenty_qty += qty;
                        }
                        Tier::Limited => {
                            limited_count += 1;
                            limited_qty += qty;
                        }
                        Tier::Insufficient => {
                            hard_count += 1;
                            hard_qty += qty;
                        }
                    }

                    // Track house vs non-house (excluding DNP)
                    if is_house_part {
                        house_count += 1;
                        house_qty += qty;
                    } else {
                        non_house_count += 1;
                        non_house_qty += qty;
                    }
                }
            }

            // Create qty and designators cells
            let qty_cell = styled_cell(format!("{:>4}", qty), is_dnp, false, None);
            let designators_cell = styled_cell(
                designators.as_str(),
                is_dnp,
                false,
                has_availability.then_some(designator_tier),
            )
            .set_delimiter(',');

            // MPN: create hyperlink and style if auto-filled
            let mpn_display = if mpn.is_empty() {
                String::new()
            } else {
                let link = hyperlink(
                    &format!(
                        "https://www.digikey.com/en/products/result?keywords={}",
                        urlencode(mpn)
                    ),
                    mpn,
                );
                style_if_autofilled(link, is_mpn_autofilled)
            };
            let mpn_cell = styled_cell(mpn_display, is_dnp, is_house_part, None);

            // Manufacturer: style if auto-filled
            let manufacturer_cell = styled_cell(
                style_if_autofilled(manufacturer.to_string(), is_manufacturer_autofilled),
                is_dnp,
                false,
                None,
            );
            let package_cell = styled_cell(
                entry["package"].as_str().unwrap_or_default(),
                is_dnp,
                false,
                None,
            );
            let description_cell = styled_cell(description, is_dnp, false, None);

            // Build row with stock as 2nd column when availability is present
            let mut row = vec![qty_cell];

            // Build availability cells
            let (stock_cell_opt, price_cell_opt, lcsc_cell_opt) = if has_availability {
                let stock_str = if aggregated_stock == 0 && aggregated_price_single.is_none() {
                    format!("{:>7}", "-")
                } else {
                    format!("{:>7}", aggregated_stock)
                };

                // Price: "$X.XX ($Y.YY)" - total for 1 board and total for NUM_BOARDS boards
                // Round up to nearest cent (ceiling)
                let price_str = match (aggregated_price_single, aggregated_price_boards) {
                    (Some(single), Some(boards)) => {
                        let single_cents = (single * 100.0).ceil() / 100.0;
                        let boards_cents = (boards * 100.0).ceil() / 100.0;
                        format!("${:.2} (${:.2})", single_cents, boards_cents)
                    }
                    (Some(single), None) => {
                        let single_cents = (single * 100.0).ceil() / 100.0;
                        format!("${:.2}", single_cents)
                    }
                    _ => String::new(),
                };

                let stock_cell = styled_cell(stock_str, is_dnp, false, Some(stock_tier));

                let price_cell = match (is_dnp, aggregated_stock) {
                    (true, _) | (false, 0) => Cell::new(price_str).fg(Color::DarkGrey),
                    _ => Cell::new(price_str),
                };

                let lcsc_display = lcsc_ids
                    .iter()
                    .map(|(id, url)| hyperlink(url, id))
                    .collect::<Vec<_>>()
                    .join(", ");

                let lcsc_cell = match is_dnp {
                    true => Cell::new(lcsc_display).fg(Color::DarkGrey),
                    false => Cell::new(lcsc_display).fg(Color::Grey),
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

        // Print summary tables if availability data is present
        if has_availability {
            writeln!(writer)?;
            writeln!(writer, "Availability Summary:")?;

            let mut summary_table = Table::new();
            configure_summary_table(&mut summary_table);

            let total_count = plenty_count + limited_count + hard_count + dnp_count;
            let total_with_dnp = plenty_qty + limited_qty + hard_qty + dnp_qty;

            summary_table.add_row(summary_row(
                Color::Green,
                "Plenty available / easy to source",
                plenty_count,
                total_count,
                plenty_qty,
                total_with_dnp,
            ));
            summary_table.add_row(summary_row(
                Color::Yellow,
                "Limited inventory / harder to source",
                limited_count,
                total_count,
                limited_qty,
                total_with_dnp,
            ));
            summary_table.add_row(summary_row(
                Color::Red,
                "Insufficient stock / hard to source",
                hard_count,
                total_count,
                hard_qty,
                total_with_dnp,
            ));
            summary_table.add_row(summary_row(
                Color::DarkGrey,
                "DNP (Do Not Populate)",
                dnp_count,
                total_count,
                dnp_qty,
                total_with_dnp,
            ));

            writeln!(writer, "{summary_table}")?;

            writeln!(writer)?;
            writeln!(writer, "House Component Summary:")?;

            let mut house_table = Table::new();
            configure_summary_table(&mut house_table);

            let house_total_count = house_count + non_house_count;
            let house_total_qty = house_qty + non_house_qty;

            house_table.add_row(summary_row(
                Color::Blue,
                "House component",
                house_count,
                house_total_count,
                house_qty,
                house_total_qty,
            ));
            house_table.add_row(summary_row(
                Color::White,
                "Non-house component",
                non_house_count,
                house_total_count,
                non_house_qty,
                house_total_qty,
            ));

            writeln!(writer, "{house_table}")?;
        }
        Ok(())
    }
}
