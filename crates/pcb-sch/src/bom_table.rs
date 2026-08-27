use std::io::{self, Write};

use colored::Colorize;
use comfy_table::{Cell, Color, Table};
use terminal_hyperlink::Hyperlink as _;
use urlencoding::encode as urlencode;

use crate::bom::availability::BOARD_QUANTITY;
use crate::bom::{AvailabilitySummary, Bom, PartCollection, SourcingStockClass};

const NO_MATCH_LABEL: &str = "No match (unknown part)";
const NO_MATCH_DATA_LABEL: &str = "No match data";

/// Create a cell with quantity and percentage (percentage in grey)
fn qty_with_percentage_cell(qty: usize, percentage: f64) -> Cell {
    Cell::new(format!(
        "{:>4} {}",
        qty,
        format!("({:>5.1}%)", percentage).dimmed()
    ))
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

fn percentage(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64) * 100.0
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
        qty_with_percentage_cell(count, percentage(count, count_total)),
        qty_with_percentage_cell(qty, percentage(qty, qty_total)),
    ]
}

fn collection_color(collection: Option<PartCollection>) -> Option<Color> {
    match collection {
        Some(PartCollection::House) => Some(Color::Blue),
        Some(PartCollection::Extended) => Some(Color::Cyan),
        None => None,
    }
}

fn common_part_collection(
    mut collections: impl Iterator<Item = Option<PartCollection>>,
) -> Option<PartCollection> {
    let collection = collections.next()??;
    collections
        .all(|candidate| candidate == Some(collection))
        .then_some(collection)
}

/// Apply styling to a cell based on component flags.
fn styled_cell(content: impl ToString, is_dnp: bool, collection: Option<PartCollection>) -> Cell {
    let cell = Cell::new(content);
    if is_dnp {
        cell.fg(Color::DarkGrey)
    } else if let Some(color) = collection_color(collection) {
        cell.fg(color)
    } else {
        cell
    }
}

/// Map the selected planner result to its presentation color.
fn color_for_status(
    is_dnp: bool,
    no_match: Option<bool>,
    sourceability: Option<SourcingStockClass>,
) -> Option<Color> {
    if is_dnp {
        Some(Color::DarkGrey)
    } else {
        match no_match {
            None => Some(Color::Grey),
            Some(true) => Some(Color::Magenta),
            Some(false) => sourceability.map(|sourceability| match sourceability {
                SourcingStockClass::Plenty => Color::Green,
                SourcingStockClass::Limited | SourcingStockClass::Unknown => Color::Yellow,
                SourcingStockClass::Insufficient => Color::Red,
            }),
        }
    }
}

/// Apply styling to response-state cells.
fn styled_status_cell(
    content: impl ToString,
    is_dnp: bool,
    no_match: Option<bool>,
    sourceability: Option<SourcingStockClass>,
) -> Cell {
    let cell = Cell::new(content);
    match color_for_status(is_dnp, no_match, sourceability) {
        Some(color) => cell.fg(color),
        None => cell,
    }
}

fn line_sourceability(
    us: Option<SourcingStockClass>,
    global: Option<SourcingStockClass>,
) -> Option<SourcingStockClass> {
    match (us, global) {
        (None, class) | (class, None) => class,
        (Some(SourcingStockClass::Insufficient), Some(SourcingStockClass::Insufficient)) => {
            Some(SourcingStockClass::Insufficient)
        }
        (Some(SourcingStockClass::Plenty), Some(SourcingStockClass::Plenty)) => {
            Some(SourcingStockClass::Plenty)
        }
        _ => Some(SourcingStockClass::Limited),
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
    alt_stock: i32,
    price_single: Option<f64>,
    price_boards: Option<f64>,
    sourceability: Option<SourcingStockClass>,
    lcsc_ids: Vec<(String, String)>,
}

impl RegionDisplayData {
    fn from_region_avail(avail: Option<&AvailabilitySummary>, qty: usize) -> Self {
        let Some(a) = avail else {
            return Self::default();
        };

        let (price_single, price_boards) = match &a.price_breaks {
            Some(breaks) => {
                let unit_single = unit_price_from_breaks(breaks, qty as i32);
                let unit_boards = unit_price_from_breaks(breaks, (qty as i32) * BOARD_QUANTITY);
                (
                    unit_single.map(|p| p * qty as f64),
                    unit_boards.map(|p| p * (qty as i32 * BOARD_QUANTITY) as f64),
                )
            }
            None => (None, None),
        };

        RegionDisplayData {
            stock: a.stock,
            alt_stock: a.alt_stock,
            price_single,
            price_boards,
            sourceability: Some(a.stock_class),
            lcsc_ids: a.lcsc_part_ids.clone(),
        }
    }

    fn format_stock(&self) -> String {
        if self.stock <= 0 && self.price_single.is_none() {
            "-".to_string()
        } else if self.alt_stock > 0 {
            format!(
                "{} {}",
                self.stock,
                format!("(+{})", self.alt_stock).dimmed()
            )
        } else {
            self.stock.to_string()
        }
    }

    fn format_price(&self) -> String {
        match (self.price_single, self.price_boards) {
            (Some(single), Some(boards)) => {
                format!("${:.2} (${:.2})", ceil_cents(single), ceil_cents(boards))
            }
            (Some(single), None) => format!("${:.2}", ceil_cents(single)),
            _ => "-".to_string(),
        }
    }
}

/// Round up to nearest cent
fn ceil_cents(value: f64) -> f64 {
    (value * 100.0).ceil() / 100.0
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

        if has_availability {
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
                Cell::new("  "),
                Cell::new("■").fg(Color::Magenta),
                Cell::new(NO_MATCH_LABEL),
            ]);
            legend_table.add_row(vec![
                Cell::new("■").fg(Color::Cyan),
                Cell::new("Extended component"),
                Cell::new("  "),
                Cell::new("■").fg(Color::Grey),
                Cell::new(NO_MATCH_DATA_LABEL),
            ]);
        } else {
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
            ]);
        }

        writeln!(writer, "{legend_table}")?;

        // Track summary stats (only used when has_availability)
        let mut matched_count = 0;
        let mut matched_qty = 0;
        let mut no_match_count = 0;
        let mut no_match_qty = 0;
        let mut no_match_data_count = 0;
        let mut no_match_data_qty = 0;
        let mut dnp_count = 0;
        let mut dnp_qty = 0;
        let mut house_count = 0;
        let mut house_qty = 0;
        let mut extended_count = 0;
        let mut extended_qty = 0;
        let mut unclassified_count = 0;
        let mut unclassified_qty = 0;

        let mut table = Table::new();
        table.load_preset(comfy_table::presets::UTF8_FULL_CONDENSED);
        table.set_content_arrangement(comfy_table::ContentArrangement::DynamicFullWidth);

        let mut entries = self.grouped_entries();
        // Sort entries: non-DNP first (sorted by first designator), then DNP items (sorted by first designator)
        entries.sort_by(|a, b| {
            a.entry.dnp.cmp(&b.entry.dnp).then_with(|| {
                a.designators
                    .iter()
                    .next()
                    .cmp(&b.designators.iter().next())
            })
        });

        for grouped in entries {
            let designators_vec: Vec<&str> =
                grouped.designators.iter().map(AsRef::as_ref).collect();

            // Designators already naturally sorted by BTreeSet<NaturalString>
            let qty = designators_vec.len();
            let designators = designators_vec.join(",");
            let entry = &grouped.entry;

            let mpn = entry
                .mpn
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            let manufacturer = entry
                .manufacturer
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            let description = entry
                .description
                .as_deref()
                .or(entry.value.as_deref())
                .unwrap_or_default();
            let is_dnp = entry.dnp;

            let paths: Vec<&String> = self
                .designators
                .iter()
                .filter(|(_, d)| designators_vec.contains(&d.as_str()))
                .map(|(p, _)| p)
                .collect();

            // A grouped row is sourceable only when every represented line has match data.
            let availabilities = paths
                .iter()
                .map(|path| self.availability.get(*path))
                .collect::<Option<Vec<_>>>();
            let avail = availabilities
                .as_deref()
                .and_then(|availabilities| availabilities.first().copied());
            let no_match = avail.map(|availability| availability.no_match);
            let collection = availabilities.as_deref().and_then(|availabilities| {
                common_part_collection(
                    availabilities
                        .iter()
                        .map(|availability| availability.selected_part_collection()),
                )
            });

            let us_data =
                RegionDisplayData::from_region_avail(avail.and_then(|a| a.us.as_ref()), qty);
            let global_data =
                RegionDisplayData::from_region_avail(avail.and_then(|a| a.global.as_ref()), qty);

            let line_sourceability =
                line_sourceability(us_data.sourceability, global_data.sourceability);

            // Track summary stats
            if has_availability {
                if is_dnp {
                    dnp_count += 1;
                    dnp_qty += qty;
                } else if avail.is_none() {
                    no_match_data_count += 1;
                    no_match_data_qty += qty;
                } else if matches!(no_match, Some(true)) {
                    no_match_count += 1;
                    no_match_qty += qty;
                } else {
                    matched_count += 1;
                    matched_qty += qty;

                    match collection {
                        Some(PartCollection::House) => {
                            house_count += 1;
                            house_qty += qty;
                        }
                        Some(PartCollection::Extended) => {
                            extended_count += 1;
                            extended_qty += qty;
                        }
                        None => {
                            unclassified_count += 1;
                            unclassified_qty += qty;
                        }
                    }
                }
            }

            // Create qty and designators cells
            let qty_cell = styled_cell(format!("{:>4}", qty), is_dnp, None);
            let designators_cell = (if has_availability {
                styled_status_cell(designators.as_str(), is_dnp, no_match, line_sourceability)
            } else {
                styled_cell(designators.as_str(), is_dnp, None)
            })
            .set_delimiter(',');

            let mpn_display = if mpn.is_empty() {
                String::new()
            } else {
                hyperlink(
                    &format!(
                        "https://www.digikey.com/en/products/result?keywords={}",
                        urlencode(mpn)
                    ),
                    mpn,
                )
            };
            let mpn_cell = styled_cell(mpn_display, is_dnp, collection);

            let manufacturer_cell = styled_cell(manufacturer, is_dnp, None);
            let package_cell =
                styled_cell(entry.package.as_deref().unwrap_or_default(), is_dnp, None);
            let description_cell = styled_cell(description, is_dnp, None);

            // Build row
            let mut row = vec![qty_cell];

            // Add stock columns (US and Global)
            if has_availability {
                row.push(styled_status_cell(
                    us_data.format_stock(),
                    is_dnp,
                    no_match,
                    us_data.sourceability,
                ));
                row.push(styled_status_cell(
                    global_data.format_stock(),
                    is_dnp,
                    no_match,
                    global_data.sourceability,
                ));
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
                row.push(styled_cell(us_data.format_price(), is_dnp, None));
                row.push(styled_cell(global_data.format_price(), is_dnp, None));
            }

            row.push(description_cell);
            table.add_row(row);
        }

        // Set headers
        let mut headers = vec!["Qty"];

        if has_availability {
            headers.push("Stock US (+alt)");
            headers.push("Stock Global (+alt)");
        }

        headers.extend(vec!["Designators", "MPN", "Manufacturer", "Package"]);

        if has_availability {
            headers.push("LCSC");
        }

        let price_us_header = format!("Price US ({}x)", BOARD_QUANTITY);
        let price_global_header = format!("Price Global ({}x)", BOARD_QUANTITY);
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

            let total_count = matched_count + no_match_count + no_match_data_count + dnp_count;
            let total_with_dnp = matched_qty + no_match_qty + no_match_data_qty + dnp_qty;

            summary_table.add_row(summary_row(
                Color::White,
                "Matched / planner-ranked",
                matched_count,
                total_count,
                matched_qty,
                total_with_dnp,
            ));
            summary_table.add_row(summary_row(
                Color::Magenta,
                NO_MATCH_LABEL,
                no_match_count,
                total_count,
                no_match_qty,
                total_with_dnp,
            ));
            summary_table.add_row(summary_row(
                Color::Grey,
                NO_MATCH_DATA_LABEL,
                no_match_data_count,
                total_count,
                no_match_data_qty,
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

            let collection_total_count = house_count + extended_count + unclassified_count;
            let collection_total_qty = house_qty + extended_qty + unclassified_qty;

            if collection_total_count > 0 {
                writeln!(writer)?;
                writeln!(writer, "Part Collection Summary:")?;

                let mut collection_table = Table::new();
                configure_summary_table(&mut collection_table);

                collection_table.add_row(summary_row(
                    Color::Blue,
                    "House component",
                    house_count,
                    collection_total_count,
                    house_qty,
                    collection_total_qty,
                ));
                collection_table.add_row(summary_row(
                    Color::Cyan,
                    "Extended component",
                    extended_count,
                    collection_total_count,
                    extended_qty,
                    collection_total_qty,
                ));
                collection_table.add_row(summary_row(
                    Color::White,
                    "Unclassified component",
                    unclassified_count,
                    collection_total_count,
                    unclassified_qty,
                    collection_total_qty,
                ));

                writeln!(writer, "{collection_table}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::bom::{Availability, BomEntry};

    #[test]
    fn grouped_collection_requires_consistent_provenance() {
        assert_eq!(
            common_part_collection([Some(PartCollection::House); 2].into_iter()),
            Some(PartCollection::House)
        );
        assert_eq!(
            common_part_collection(
                [Some(PartCollection::House), Some(PartCollection::Extended),].into_iter()
            ),
            None
        );
    }

    #[test]
    fn no_match_status_is_magenta() {
        assert_eq!(
            color_for_status(false, Some(true), Some(SourcingStockClass::Plenty)),
            Some(Color::Magenta)
        );
    }

    #[test]
    fn api_stock_classes_map_to_sourceability_colors() {
        assert_eq!(
            color_for_status(false, Some(false), Some(SourcingStockClass::Plenty)),
            Some(Color::Green)
        );
        assert_eq!(
            color_for_status(false, Some(false), Some(SourcingStockClass::Limited)),
            Some(Color::Yellow)
        );
        assert_eq!(
            color_for_status(false, Some(false), Some(SourcingStockClass::Insufficient)),
            Some(Color::Red)
        );
        assert_eq!(
            color_for_status(false, Some(false), Some(SourcingStockClass::Unknown)),
            Some(Color::Yellow)
        );
    }

    #[test]
    fn missing_match_data_is_grey_but_missing_region_is_uncolored() {
        assert_eq!(color_for_status(false, None, None), Some(Color::Grey));
        assert_eq!(color_for_status(false, Some(false), None), None);
        assert_eq!(
            line_sourceability(Some(SourcingStockClass::Plenty), None),
            Some(SourcingStockClass::Plenty)
        );
        assert_eq!(
            line_sourceability(None, Some(SourcingStockClass::Insufficient)),
            Some(SourcingStockClass::Insufficient)
        );
    }

    #[test]
    fn bom_table_no_match_rendering_includes_legend_without_nan_summary() {
        let mut bom = Bom {
            entries: HashMap::new(),
            designators: HashMap::new(),
            availability: HashMap::new(),
        };
        bom.entries.insert(
            "root.U1".to_string(),
            BomEntry {
                mpn: Some("MISSING-MPN".to_string()),
                alternatives: vec![],
                manufacturer: Some("Acme".to_string()),
                package: Some("QFN".to_string()),
                value: None,
                description: Some("Missing part".to_string()),
                generic_data: None,
                dnp: false,
                skip_bom: false,
                properties: Default::default(),
            },
        );
        bom.designators
            .insert("root.U1".to_string(), "U1".to_string());
        bom.availability.insert(
            "root.U1".to_string(),
            Availability {
                no_match: true,
                ..Default::default()
            },
        );

        let mut out = Vec::new();
        bom.write_table(&mut out).unwrap();
        let rendered = String::from_utf8(out).unwrap();

        assert!(rendered.contains("Legend:"));
        assert!(rendered.contains(NO_MATCH_LABEL));
        assert!(!rendered.contains("NaN"));
        assert!(!rendered.contains("Part Collection Summary:"));
    }
}
