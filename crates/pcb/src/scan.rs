use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use colored::Colorize;
use pcb_scan::{ScanModel, ScanOptions};
use std::path::PathBuf;

use crate::auth;

fn get_valid_token() -> Result<String> {
    let tokens =
        auth::load_tokens()?.context("Not authenticated. Run `pcb auth login` to authenticate.")?;

    // If token is expired, try to refresh it automatically
    if tokens.is_expired() {
        match auth::refresh_tokens() {
            Ok(new_tokens) => {
                println!("{}", "Token refreshed".dimmed());
                return Ok(new_tokens.access_token);
            }
            Err(e) => {
                // Refresh failed - ask user to login again
                anyhow::bail!(
                    "Authentication token expired and refresh failed: {}\nRun `pcb auth login` to re-authenticate.",
                    e
                );
            }
        }
    }

    Ok(tokens.access_token)
}

fn get_api_base_url() -> String {
    if let Ok(url) = std::env::var("DIODE_API_URL") {
        return url;
    }

    #[cfg(debug_assertions)]
    return "http://localhost:3001".to_string();
    #[cfg(not(debug_assertions))]
    return "https://api.diode.computer".to_string();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModelArg {
    #[value(name = "mistral-ocr-latest")]
    MistralOcrLatest,
    #[value(name = "gpt-4o")]
    Gpt4o,
    #[value(name = "gpt-4o-mini")]
    Gpt4oMini,
}

impl From<ModelArg> for ScanModel {
    fn from(arg: ModelArg) -> Self {
        match arg {
            ModelArg::MistralOcrLatest => ScanModel::MistralOcrLatest,
            ModelArg::Gpt4o => ScanModel::Gpt4o,
            ModelArg::Gpt4oMini => ScanModel::Gpt4oMini,
        }
    }
}

#[derive(Args, Debug)]
#[command(about = "Scan PDF datasheets with OCR")]
pub struct ScanArgs {
    /// PDF file to scan
    #[arg(value_name = "FILE")]
    file: PathBuf,

    /// Output directory (default: same directory as input file)
    #[arg(short, long, value_name = "DIR")]
    output: Option<PathBuf>,

    /// OCR model to use
    #[arg(short, long, value_enum)]
    model: Option<ModelArg>,

    /// Download and extract images
    #[arg(long)]
    images: bool,
}

pub fn execute(args: ScanArgs) -> Result<()> {
    // Validate --images is only used with mistral-ocr-latest
    if args.images {
        if let Some(model) = args.model {
            if model != ModelArg::MistralOcrLatest {
                anyhow::bail!(
                    "The --images flag is only supported with the mistral-ocr-latest model"
                );
            }
        }
        // If no model specified, it defaults to mistral-ocr-latest on the backend, so allow it
    }

    // Get auth token (with auto-refresh)
    let token = get_valid_token()?;

    // Get API base URL
    let api_base_url = get_api_base_url();

    // Get output directory - default to same directory as input file
    let output_dir = args.output.unwrap_or_else(|| {
        args.file
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    });

    // Build scan options
    let options = ScanOptions {
        file: args.file,
        output_dir,
        model: args.model.map(Into::into),
        images: args.images,
    };

    // Execute scan
    pcb_scan::scan_pdf(&api_base_url, &token, options)?;

    Ok(())
}
