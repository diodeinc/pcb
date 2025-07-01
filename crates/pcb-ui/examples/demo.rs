//! Example demonstrating the diode-ui API

use pcb_ui::prelude::*;
use std::{thread, time::Duration};

fn main() -> anyhow::Result<()> {
    println!("diode-ui Demo\n");

    // Simple spinner example
    println!("1. Basic spinner:");
    let spinner = Spinner::builder("Processing...").start();
    thread::sleep(Duration::from_secs(2));
    spinner.success("Processing complete!");

    println!("\n2. Customized spinner:");
    let spinner = Spinner::builder("Loading data")
        .style(Style::Blue)
        .tick_chars("◐◓◑◒")
        .tick_interval(Duration::from_millis(200))
        .start();
    thread::sleep(Duration::from_secs(2));
    spinner.finish_with_message("Data loaded");

    // Progress bar examples
    println!("\n3. Basic progress bar:");
    let pb = ProgressBar::builder(100).message("Downloading").start();
    for i in 0..=100 {
        pb.set_position(i);
        thread::sleep(Duration::from_millis(20));
    }
    pb.success("Download complete!");

    println!("\n4. Styled progress bar:");
    let pb = ProgressBar::builder(50)
        .message("Building")
        .style(Style::Yellow)
        .progress_chars("#>-")
        .start();

    for i in 0..=50 {
        pb.set_position(i);
        pb.set_message(format!("Building ({i}/50)"));
        thread::sleep(Duration::from_millis(40));
    }
    pb.finish_with_message("Build finished");

    // Error example
    println!("\n5. Error handling:");
    let spinner = Spinner::builder("Connecting to server").start();
    thread::sleep(Duration::from_secs(1));
    spinner.error("Connection failed!");

    // Warning example
    println!("\n6. Warning:");
    let spinner = Spinner::builder("Checking dependencies").start();
    thread::sleep(Duration::from_secs(1));
    spinner.warning("Some dependencies are outdated");

    // Using styled text
    println!("\n7. Styled text:");
    println!("{}", "Operation successful".success());
    println!("{}", "Operation failed".error());
    println!("{}", "Proceed with caution".warning());
    println!("{}", "For your information".info());

    // Terminal utilities
    println!("\n8. Terminal utilities:");
    if let Some(size) = pcb_ui::get_terminal_size() {
        println!("Terminal size: {}x{}", size.width, size.height);
    }

    let long_text =
        "This is a very long text that might need to be truncated to fit in the terminal";
    let truncated = pcb_ui::truncate_text(long_text, 40);
    println!("Truncated: {truncated}");

    Ok(())
}
