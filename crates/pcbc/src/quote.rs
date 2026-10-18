use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use comfy_table::{Cell, CellAlignment, Table, presets::UTF8_FULL_CONDENSED};
use pcb_ui::prelude::*;
use pcb_zen::git;
use pcb_zen::workspace::{WorkspaceInfoExt, get_workspace_info};
use pcb_zen_core::DefaultFileProvider;
use serde::Deserialize;

use crate::config_input::CONFIG_ARG_HELP;
use crate::layout::LayoutArgs;

/// Where the bare boards are made and who assembles them.
#[derive(ValueEnum, Debug, Clone, Copy, Default)]
pub enum QuoteRoute {
    /// US bare boards, assembled by Diode in the US
    #[default]
    Us,
    /// Global bare boards, assembled by Diode in the US
    GlobalUs,
    /// Global bare boards and assembly (enterprise plans)
    Global,
}

impl QuoteRoute {
    fn as_str(self) -> &'static str {
        match self {
            Self::Us => "us",
            Self::GlobalUs => "global-us",
            Self::Global => "global",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Us => "US bare boards, assembled by Diode in the US",
            Self::GlobalUs => "Global bare boards, assembled by Diode in the US",
            Self::Global => "Global bare boards and assembly",
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, Default)]
pub enum QuoteFormat {
    #[default]
    Table,
    Json,
}

#[derive(Args, Debug, Clone)]
#[command(
    about = "Quote fabrication and assembly of a board's saved layout",
    long_about = "Quote fabrication and assembly of a board's saved layout and current \
                  source, priced as the Diode order dialog prices a release. Nothing is \
                  published or ordered."
)]
pub struct QuoteArgs {
    /// Board .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Assembled boards to quote; repeat for several quantities
    #[arg(
        short,
        long = "quantity",
        value_name = "N",
        default_values_t = [5, 10, 25, 50, 100],
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub quantities: Vec<u32>,

    /// Where the bare boards are made and who assembles them
    #[arg(long, value_enum, default_value_t)]
    pub route: QuoteRoute,

    /// Extra unassembled bare boards in each quote
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub bare_boards: u32,

    #[arg(long = "config", value_name = "KEY=VALUE", help = CONFIG_ARG_HELP)]
    pub config: Vec<String>,

    /// Output format
    #[arg(short, long, value_enum, default_value_t)]
    pub format: QuoteFormat,
}

pub fn execute(args: QuoteArgs) -> Result<()> {
    crate::file_walker::require_zen_file(&args.file)?;
    let (workspace, board) = board_identity(&args.file)?;
    let design = crate::layout::prepare_design(&LayoutArgs {
        file: args.file.clone(),
        config: args.config.clone(),
        no_open: true,
        // Quote the saved layout as-is.
        no_sync: true,
        ..Default::default()
    })?;
    let design_bom =
        serde_json::to_value(design.schematic.bom().filter_excluded().ungrouped_entries())?;
    let pcb_file = crate::dfm::saved_layout(&args.file, &design.schematic)?;

    let spinner = Spinner::builder(format!("{}: Exporting IPC-2581", design.file_name)).start();
    let (_temporary_dir, ipc_path) = crate::dfm::export_ipc(&pcb_file)?;
    let ipc_xml = std::fs::read_to_string(&ipc_path).context("Failed to read IPC-2581 export")?;
    spinner.set_message(format!("{}: Quoting {workspace}/{board}", design.file_name));
    let response = pcb_diode_api::quote_board(
        &pcb_diode_api::WorkspaceContext::from_path(&args.file),
        &workspace,
        &board,
        &pcb_diode_api::QuoteRequest {
            route: args.route.as_str(),
            quantities: &args.quantities,
            bare_board_quantity: args.bare_boards,
            ipc_xml: &ipc_xml,
            design_bom,
        },
    )?;
    spinner.finish();

    match args.format {
        QuoteFormat::Json => {
            let json = serde_json::to_string_pretty(&response)?;
            pcb_ui::write_stdout(|w| writeln!(w, "{json}"))?;
        }
        QuoteFormat::Table => {
            let quotes: QuoteResponse =
                serde_json::from_value(response).context("Invalid quote response")?;
            let report = render_quotes(&quotes, &workspace, &board, args.route);
            pcb_ui::write_stdout(|w| write!(w, "{report}"))?;
        }
    }
    Ok(())
}

/// The board's Diode workspace and URL name, as `pcb publish` resolves them.
fn board_identity(zen_path: &Path) -> Result<(String, String)> {
    let start = zen_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let workspace = get_workspace_info(&DefaultFileProvider::new(), start)?;
    let name = crate::publish::release_workspace_name(&workspace)?;
    let board = workspace
        .repository()
        .and_then(board_slug_from_repository)
        .or_else(|| workspace.board_name_for_zen(zen_path))
        .or_else(|| Some(zen_path.file_stem()?.to_string_lossy().into_owned()))
        .context("Unable to determine the board name")?;
    Ok((name, board))
}

/// A Diode board repository `<host>/<workspace>/b/<board>` names its board.
fn board_slug_from_repository(repository: &str) -> Option<String> {
    let repository = git::parse_remote_url(repository).unwrap_or_else(|_| repository.trim().into());
    match repository.split('/').collect::<Vec<_>>().as_slice() {
        [_, _, "b", board] if !board.is_empty() => Some(board.to_string()),
        _ => None,
    }
}

#[derive(Deserialize, Debug)]
struct QuoteResponse {
    results: Vec<QuoteResult>,
}

#[derive(Deserialize, Debug)]
struct QuoteResult {
    quantity: u32,
    status: String,
    issues: Vec<QuoteIssue>,
    quote: Option<Quote>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Quote {
    customer_facing_quote: CustomerQuote,
    lead_time_days: u32,
}

#[derive(Deserialize, Debug)]
struct CustomerQuote {
    sections: Vec<QuoteSection>,
    total_cost_cents: i64,
}

#[derive(Deserialize, Debug)]
struct QuoteSection {
    key: String,
    display_name: String,
    total_cost_cents: i64,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct QuoteIssue {
    kind: String,
    section: String,
    cause: String,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    designators: Vec<String>,
}

fn status_meaning(status: &str) -> Option<&'static str> {
    match status {
        "review" => Some("Diode reviews the order before manufacturing"),
        "blocked" => Some("fix the issues below before ordering"),
        "rfq" => Some("part of the order needs a manual quote from Diode"),
        "error" => Some("quoting failed on Diode's side; try again"),
        _ => None,
    }
}

fn render_quotes(
    quotes: &QuoteResponse,
    workspace: &str,
    board: &str,
    route: QuoteRoute,
) -> String {
    // Section columns in first-seen order, so every row lines up.
    let mut sections: Vec<(&str, &str)> = Vec::new();
    for quote in quotes
        .results
        .iter()
        .filter_map(|result| result.quote.as_ref())
    {
        for section in &quote.customer_facing_quote.sections {
            if !sections.iter().any(|(key, _)| *key == section.key) {
                sections.push((&section.key, &section.display_name));
            }
        }
    }

    let mut table = Table::new();
    table.load_style(UTF8_FULL_CONDENSED);
    let mut header = vec!["Qty", "Status"];
    header.extend(sections.iter().map(|(_, name)| *name));
    header.extend(["Total", "Per board", "Lead time"]);
    table.set_header(header);
    for result in &quotes.results {
        let mut row = vec![Cell::new(result.quantity), Cell::new(&result.status)];
        match &result.quote {
            Some(quote) => {
                let quote_sections = &quote.customer_facing_quote.sections;
                row.extend(sections.iter().map(|(key, _)| {
                    quote_sections
                        .iter()
                        .find(|section| section.key == *key)
                        .map(|section| usd(section.total_cost_cents))
                        .unwrap_or_default()
                        .into()
                }));
                let total = quote.customer_facing_quote.total_cost_cents;
                row.extend([
                    usd(total).into(),
                    usd(total / i64::from(result.quantity)).into(),
                    format!("{} days", quote.lead_time_days).into(),
                ]);
            }
            None => row.extend(std::iter::repeat_n(Cell::new("—"), sections.len() + 3)),
        }
        table.add_row(row);
    }
    for column in table.column_iter_mut().skip(2) {
        column.set_cell_alignment(CellAlignment::Right);
    }

    let mut out = format!("{workspace}/{board}: {}\n{table}\n", route.description());
    let mut statuses: Vec<&str> = Vec::new();
    for result in &quotes.results {
        if !statuses.contains(&result.status.as_str()) {
            statuses.push(&result.status);
        }
    }
    for status in statuses {
        if let Some(meaning) = status_meaning(status) {
            out.push_str(&format!("{status}: {meaning}\n"));
        }
    }

    // One line per issue, naming the quantities it applies to when not all.
    let mut issues: BTreeMap<&QuoteIssue, Vec<u32>> = BTreeMap::new();
    for result in &quotes.results {
        for issue in &result.issues {
            issues.entry(issue).or_default().push(result.quantity);
        }
    }
    if !issues.is_empty() {
        out.push_str("\nIssues:\n");
    }
    for (issue, quantities) in issues {
        let mut line = format!("- [{}] {}: {}", issue.kind, issue.section, issue.cause);
        if let Some(detail) = &issue.detail {
            line.push_str(&format!(" — {detail}"));
        }
        if !issue.designators.is_empty() {
            line.push_str(&format!(" ({})", issue.designators.join(", ")));
        }
        if quantities.len() < quotes.results.len() {
            let quantities: Vec<_> = quantities.iter().map(u32::to_string).collect();
            line.push_str(&format!(" at qty {}", quantities.join(", ")));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn usd(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.unsigned_abs();
    let dollars = (cents / 100).to_string();
    let mut grouped = String::new();
    for (index, digit) in dollars.chars().enumerate() {
        if index > 0 && (dollars.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{sign}${grouped}.{:02}", cents % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_slug_comes_from_diode_board_repositories_only() {
        assert_eq!(
            board_slug_from_repository("code.diode.computer/acme/b/IP0010").as_deref(),
            Some("IP0010")
        );
        assert_eq!(
            board_slug_from_repository("https://code.diode.computer/acme/b/IP0010.git").as_deref(),
            Some("IP0010")
        );
        assert_eq!(board_slug_from_repository("github.com/acme/boards"), None);
        assert_eq!(
            board_slug_from_repository("code.diode.computer/acme/registry"),
            None
        );
    }

    #[test]
    fn usd_groups_thousands_and_keeps_cents() {
        assert_eq!(usd(5), "$0.05");
        assert_eq!(usd(123_456_789), "$1,234,567.89");
        assert_eq!(usd(-100_000), "-$1,000.00");
    }

    #[test]
    fn renders_sections_statuses_and_issues_per_quantity() {
        let quotes: QuoteResponse = serde_json::from_value(serde_json::json!({
            "results": [
                {
                    "quantity": 5, "status": "orderable", "issues": [],
                    "quote": { "leadTimeDays": 12, "customerFacingQuote": {
                        "total_cost_cents": 50_000,
                        "sections": [
                            { "key": "components", "display_name": "Components", "total_cost_cents": 20_000 },
                            { "key": "assembly", "display_name": "Assembly", "total_cost_cents": 30_000 }
                        ]
                    }}
                },
                {
                    "quantity": 100, "status": "rfq",
                    "issues": [{ "section": "components", "kind": "rfq", "cause": "Insufficient stock", "designators": ["U1", "U2"] }],
                    "quote": null
                }
            ]
        }))
        .unwrap();

        let report = render_quotes(&quotes, "acme", "IP0010", QuoteRoute::Us);

        assert!(report.starts_with("acme/IP0010: US bare boards, assembled by Diode in the US\n"));
        let row = report
            .lines()
            .find(|line| line.contains("orderable"))
            .unwrap();
        for value in ["$200.00", "$300.00", "$500.00", "$100.00", "12 days"] {
            assert!(row.contains(value), "{value} missing from {row}");
        }
        assert!(report.contains("rfq: part of the order needs a manual quote from Diode"));
        assert!(report.contains("- [rfq] components: Insufficient stock (U1, U2) at qty 100"));
    }
}
