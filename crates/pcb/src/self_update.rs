use clap::{Args, Subcommand};
use colored::Colorize;
use rand::seq::SliceRandom;
use semver::Version;

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
                    // Update was performed - print changelog from the NEW binary.
                    println!();
                    let selector =
                        changelog_selector(result.old_version.as_ref(), &result.new_version);
                    let _ = std::process::Command::new("pcb")
                        .arg("changelog")
                        .arg(selector)
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

fn changelog_selector(old: Option<&Version>, new: &Version) -> String {
    old.map_or_else(|| "latest".to_string(), |old| format!("{old}..{new}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changelog_selector_uses_old_to_new_range() {
        assert_eq!(
            changelog_selector(Some(&Version::new(0, 3, 78)), &Version::new(0, 3, 80)),
            "0.3.78..0.3.80"
        );
    }

    #[test]
    fn changelog_selector_falls_back_to_latest_without_old_version() {
        assert_eq!(changelog_selector(None, &Version::new(0, 3, 80)), "latest");
    }
}
