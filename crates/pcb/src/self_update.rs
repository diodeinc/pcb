use clap::{Args, Subcommand};
use colored::Colorize;
use rand::seq::SliceRandom;

const FORTUNES: &str = include_str!("fortune.txt");

#[derive(Args)]
pub struct SelfUpdateArgs {
    #[command(subcommand)]
    command: SelfUpdateCommands,
}

#[derive(Subcommand)]
enum SelfUpdateCommands {
    /// Update the pcb tool to the latest version
    Update,
}

pub fn execute(args: SelfUpdateArgs) -> anyhow::Result<()> {
    match args.command {
        SelfUpdateCommands::Update => {
            let mut updater = axoupdater::AxoUpdater::new_for("pcb");
            if updater.load_receipt().is_err() {
                anyhow::bail!(
                    "Self-update is only available for pcb installed via the standalone installer.\n\
                     If you installed pcb via a package manager, please update using that tool."
                );
            }

            match updater.run_sync()? {
                Some(result) => {
                    // Update was performed - print changelog from NEW binary
                    println!();
                    let _ = std::process::Command::new("pcb")
                        .args(["doc", "--changelog", "--latest"])
                        .status();

                    // Print a random fortune
                    let fortunes: Vec<&str> = FORTUNES.lines().filter(|l| !l.is_empty()).collect();
                    if let Some(fortune) = fortunes.choose(&mut rand::thread_rng()) {
                        println!();
                        println!("{}", format!("> {}", fortune).truecolor(90, 90, 90));
                    }

                    if let Some(old) = result.old_version {
                        println!();
                        println!(
                            "Updated {} → {}",
                            old.to_string().dimmed(),
                            result.new_version.to_string().green()
                        );
                    }
                }
                None => {
                    println!("Already up to date.");
                }
            }

            Ok(())
        }
    }
}
