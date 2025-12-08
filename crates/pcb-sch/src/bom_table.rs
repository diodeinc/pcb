use std::io::{self, Write};

use colored::Colorize;
use comfy_table::{Cell, Color, Table};
use terminal_hyperlink::Hyperlink as _;
use urlencoding::encode as urlencode;

use crate::bom::availability::{is_small_generic_passive, tier_for_stock, Tier, NUM_BOARDS};
use crate::bom::RegionAvailability;
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

/// Computed display data for a region's availability
#[derive(Default)]
struct RegionDisplayData {
    stock: i32,
    price_single: Option<f64>,
    price_boards: Option<f64>,
    tier: Tier,
    lcsc_ids: Vec<(String, String)>,
    mpn: Option<String>,
    manufacturer: Option<String>,
}

impl RegionDisplayData {
    fn from_region_avail(
        avail: Option<&RegionAvailability>,
        qty: usize,
        is_small_passive: bool,
    ) -> Self {
        let Some(a) = avail else {
            return Self::default();
        };

        let tier = tier_for_stock(a.stock_total, qty as i32, is_small_passive);
        let (price_single, price_boards) = match &a.price_breaks {
            Some(breaks) => {
                let unit_single = unit_price_from_breaks(breaks, qty as i32);
                let unit_boards = unit_price_from_breaks(breaks, (qty as i32) * NUM_BOARDS);
                (
                    unit_single.map(|p| p * qty as f64),
                    unit_boards.map(|p| p * (qty as i32 * NUM_BOARDS) as f64),
                )
            }
            None => (None, None),
        };

        RegionDisplayData {
            stock: a.stock_total,
            price_single,
            price_boards,
            tier,
            lcsc_ids: a.lcsc_part_ids.clone(),
            mpn: a.mpn.clone(),
            manufacturer: a.manufacturer.clone(),
        }
    }

    fn has_data(&self) -> bool {
        self.stock > 0 || self.price_single.is_some()
    }

    fn format_stock(&self) -> String {
        if !self.has_data() {
            "-".to_string()
        } else {
            format!("{}", self.stock)
        }
    }

    fn format_price(&self) -> String {
        if !self.has_data() {
            return "-".to_string();
        }
        match (self.price_single, self.price_boards) {
            (Some(single), Some(boards)) => {
                let single_cents = (single * 100.0).ceil() / 100.0;
                let boards_cents = (boards * 100.0).ceil() / 100.0;
                format!("${:.2} (${:.2})", single_cents, boards_cents)
            }
            (Some(single), None) => {
                let single_cents = (single * 100.0).ceil() / 100.0;
                format!("${:.2}", single_cents)
            }
            _ => "-".to_string(),
        }
    }

    fn create_stock_cell(&self, is_dnp: bool) -> Cell {
        let cell = Cell::new(self.format_stock());
        if is_dnp {
            cell.fg(Color::DarkGrey)
        } else {
            cell.fg(color_for_tier(self.tier))
        }
    }

    fn create_price_cell(&self, is_dnp: bool) -> Cell {
        let cell = Cell::new(self.format_price());
        if is_dnp {
            cell.fg(Color::DarkGrey)
        } else {
            cell
        }
    }
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

            // Priority: component's own fields, then first offer, then empty
            let original_mpn = entry["mpn"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    entry
                        .get("offers")?
                        .as_array()?
                        .first()?
                        .get("manufacturer_pn")?
                        .as_str()
                })
                .unwrap_or_default();

            let original_manufacturer = entry["manufacturer"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    entry
                        .get("offers")?
                        .as_array()?
                        .first()?
                        .get("manufacturer")?
                        .as_str()
                })
                .unwrap_or_default();

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

            // Get generic_data and package for sourcing status
            let generic_data = entry
                .get("generic_data")
                .and_then(|gd| serde_json::from_value::<GenericComponent>(gd.clone()).ok());

            let package = entry.get("package").and_then(|p| p.as_str());
            let is_small_passive = is_small_generic_passive(generic_data.as_ref(), package);

            // Get per-region availability from first matching path
            let avail = paths.iter().find_map(|path| self.availability.get(*path));

            let us_data = RegionDisplayData::from_region_avail(
                avail.and_then(|a| a.us.as_ref()),
                qty,
                is_small_passive,
            );
            let global_data = RegionDisplayData::from_region_avail(
                avail.and_then(|a| a.global.as_ref()),
                qty,
                is_small_passive,
            );

            // Use US offer data for MPN/Manufacturer autofill
            let avail_mpn = us_data.mpn.clone();
            let avail_manufacturer = us_data.manufacturer.clone();

            // Fill in missing MPN/manufacturer from availability data
            let (mpn, is_mpn_autofilled) = autofill_from_availability(original_mpn, &avail_mpn);
            let (manufacturer, is_manufacturer_autofilled) =
                autofill_from_availability(original_manufacturer, &avail_manufacturer);

            // Designator tier: green only if both regions are green AND has MPN/manufacturer
            let designator_tier =
                if us_data.tier == Tier::Plenty && global_data.tier == Tier::Plenty {
                    // Both regions have plenty - eligible for green, but check MPN/mfr
                    get_designator_tier(Tier::Plenty, original_mpn, original_manufacturer)
                } else {
                    // Not both green - demote to yellow
                    Tier::Limited
                };

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

            // Build row
            let mut row = vec![qty_cell];

            // Add stock columns (US and Global)
            if has_availability {
                row.push(us_data.create_stock_cell(is_dnp));
                row.push(global_data.create_stock_cell(is_dnp));
            }

            // Add standard columns
            row.extend(vec![
                designators_cell,
                mpn_cell,
                manufacturer_cell,
                package_cell,
            ]);

            // Add LCSC column (from global data only, as LCSC is a global distributor)
            if has_availability {
                let lcsc_display = global_data
                    .lcsc_ids
                    .iter()
                    .map(|(id, url)| hyperlink(url, id))
                    .collect::<Vec<_>>()
                    .join(", ");

                let lcsc_cell = match is_dnp {
                    true => Cell::new(lcsc_display).fg(Color::DarkGrey),
                    false => Cell::new(lcsc_display).fg(Color::Grey),
                };
                row.push(lcsc_cell);
            }

            // Add price columns (US and Global)
            if has_availability {
                row.push(us_data.create_price_cell(is_dnp));
                row.push(global_data.create_price_cell(is_dnp));
            }

            row.push(description_cell);
            table.add_row(row);
        }

        // Set headers
        let mut headers = vec!["Qty"];

        if has_availability {
            headers.push("Stock US");
            headers.push("Stock Global");
        }

        headers.extend(vec!["Designators", "MPN", "Manufacturer", "Package"]);

        if has_availability {
            headers.push("LCSC");
        }

        let price_us_header = format!("Price US ({}x)", NUM_BOARDS);
        let price_global_header = format!("Price Global ({}x)", NUM_BOARDS);
        if has_availability {
            headers.push(&price_us_header);
            headers.push(&price_global_header);
        }

        headers.push("Description");

        table.set_header(headers);

        writeln!(writer, "{table}")?;

        // Calculate and print total BOM cost per region if availability data is present
        if has_availability {
            let (total_us, total_global) =
                self.entries
                    .iter()
                    .fold((0.0, 0.0), |(acc_us, acc_global), (path, _entry)| {
                        let qty = self
                            .designators
                            .iter()
                            .filter(|(p, _)| p.as_str() == path)
                            .count() as i32;

                        if let Some(avail) = self.availability.get(path) {
                            let us_price = avail
                                .us
                                .as_ref()
                                .and_then(|r| r.price_breaks.as_ref())
                                .and_then(|breaks| unit_price_from_breaks(breaks, qty))
                                .map(|unit_price| unit_price * qty as f64)
                                .unwrap_or(0.0);

                            let global_price = avail
                                .global
                                .as_ref()
                                .and_then(|r| r.price_breaks.as_ref())
                                .and_then(|breaks| unit_price_from_breaks(breaks, qty))
                                .map(|unit_price| unit_price * qty as f64)
                                .unwrap_or(0.0);

                            (acc_us + us_price, acc_global + global_price)
                        } else {
                            (acc_us, acc_global)
                        }
                    });

            let total_us_cents = (total_us * 100.0).ceil() / 100.0;
            let total_global_cents = (total_global * 100.0).ceil() / 100.0;
            writeln!(
                writer,
                "Total: US ${:.2} | Global ${:.2}",
                total_us_cents, total_global_cents
            )?;
        }

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
