use bumpalo::Bump;
use clap::{Parser, Subcommand};
use ipc_2581::html_generator::generate_html;
use ipc_2581::{Ipc2581, LayerFunction, PlatingStatus};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(name = "ipc2581")]
#[command(about = "IPC-2581 PCB data format validator and analyzer", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Validate and analyze IPC-2581 file(s)
    Check {
        /// Path(s) to IPC-2581 XML file(s)
        files: Vec<PathBuf>,

        /// Show detailed information
        #[arg(short, long)]
        verbose: bool,
    },
    /// Export IPC-2581 file to various formats
    Export {
        /// Path to IPC-2581 XML file
        file: PathBuf,

        /// Output file path
        #[arg(short, long)]
        output: PathBuf,

        /// Export format (currently only html)
        #[arg(short, long, default_value = "html")]
        format: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Check { files, verbose } => {
            if files.is_empty() {
                eprintln!("Error: No files specified");
                process::exit(1);
            }

            let mut total_errors = 0;
            let mut total_checked = 0;

            for file in &files {
                if files.len() > 1 {
                    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                }

                match check_file(file, verbose) {
                    Ok(_) => {
                        total_checked += 1;
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        total_errors += 1;
                    }
                }

                if files.len() > 1 {
                    println!();
                }
            }

            if files.len() > 1 {
                println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                println!(
                    "Checked {} file(s): {} passed, {} failed",
                    files.len(),
                    total_checked,
                    total_errors
                );
            }

            if total_errors > 0 {
                process::exit(1);
            }
        }
        Commands::Export {
            file,
            output,
            format,
        } => {
            if format != "html" {
                eprintln!("Error: Only 'html' format is currently supported");
                process::exit(1);
            }

            match export_html(&file, &output) {
                Ok(_) => {
                    println!("✓ Exported to {}", output.display());
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    process::exit(1);
                }
            }
        }
    }
}

fn check_file(path: &PathBuf, verbose: bool) -> Result<(), Box<dyn std::error::Error>> {
    println!("Checking IPC-2581 file: {}", path.display());
    println!();

    // Parse the file
    let arena = Bump::new();
    let start = std::time::Instant::now();
    let doc = Ipc2581::parse_file(&arena, path)?;
    let parse_time = start.elapsed();

    println!("✓ File parsed successfully in {:?}", parse_time);
    println!("  Revision: {}", doc.revision());
    println!();

    // Run data integrity checks
    let mut errors = 0;
    let mut warnings = 0;

    // Determine file type based on function mode
    let content = doc.content();
    let is_reduced_layer_file = matches!(
        content.function_mode.mode,
        ipc_2581::Mode::Assembly | ipc_2581::Mode::Test | ipc_2581::Mode::Stencil
    );

    // Check if we have Ecad data
    if let Some(ecad) = doc.ecad() {
        println!("━━━ ECAD Data ━━━");

        let step = &ecad.cad_data.steps[0];
        let step_name = doc.resolve(step.name);

        println!("Step: {}", step_name);
        println!();

        // === COMPONENT & PACKAGE VALIDATION ===
        println!("Components & Packages:");
        println!("  {} padstack definitions", step.padstack_defs.len());
        println!("  {} package definitions", step.packages.len());
        println!("  {} component instances", step.components.len());

        // Build package name set
        let mut package_names = HashSet::new();
        for pkg in &step.packages {
            package_names.insert(doc.resolve(pkg.name));
        }

        // Validate component package references
        let mut invalid_pkg_refs = 0;
        for comp in &step.components {
            let pkg_name = doc.resolve(comp.package_ref);
            if !package_names.contains(pkg_name) {
                invalid_pkg_refs += 1;
                if verbose {
                    eprintln!(
                        "  ⚠ Component {} references non-existent package {}",
                        doc.resolve(comp.ref_des),
                        pkg_name
                    );
                }
            }
        }

        if invalid_pkg_refs > 0 {
            println!(
                "  ✗ {} components reference invalid packages",
                invalid_pkg_refs
            );
            errors += invalid_pkg_refs;
        } else {
            println!("  ✓ All component package references valid");
        }
        println!();

        // === CONNECTIVITY VALIDATION ===
        println!("Connectivity:");
        println!("  {} logical nets", step.logical_nets.len());

        let total_pin_refs: usize = step.logical_nets.iter().map(|net| net.pin_refs.len()).sum();
        println!("  {} total pin references", total_pin_refs);

        // Build component RefDes set
        let mut component_refdes = HashSet::new();
        for comp in &step.components {
            component_refdes.insert(doc.resolve(comp.ref_des));
        }

        // Validate pin references
        let mut invalid_pin_refs = 0;
        for net in &step.logical_nets {
            for pin_ref in &net.pin_refs {
                let comp_name = doc.resolve(pin_ref.component_ref);
                if !component_refdes.contains(comp_name) {
                    invalid_pin_refs += 1;
                    if verbose && invalid_pin_refs <= 5 {
                        eprintln!(
                            "  ⚠ Net {} references non-existent component {}",
                            doc.resolve(net.name),
                            comp_name
                        );
                    }
                }
            }
        }

        if invalid_pin_refs > 0 {
            println!(
                "  ✗ {} pin references to non-existent components",
                invalid_pin_refs
            );
            errors += invalid_pin_refs;
        } else {
            println!("  ✓ All pin references valid");
        }
        println!();

        // === LAYER VALIDATION ===
        println!("Layers:");
        println!("  {} total layers", ecad.cad_data.layers.len());

        let plane_layers = ecad
            .cad_data
            .layers
            .iter()
            .filter(|l| l.layer_function == LayerFunction::Plane)
            .count();
        let conductor_layers = ecad
            .cad_data
            .layers
            .iter()
            .filter(|l| l.layer_function == LayerFunction::Conductor)
            .count();
        let drill_layers = ecad
            .cad_data
            .layers
            .iter()
            .filter(|l| l.layer_function == LayerFunction::Drill)
            .count();

        println!(
            "  {} copper layers ({} plane + {} conductor)",
            plane_layers + conductor_layers,
            plane_layers,
            conductor_layers
        );
        println!("  {} drill layers", drill_layers);

        // Build layer name set
        let mut layer_names = HashSet::new();
        for layer in &ecad.cad_data.layers {
            layer_names.insert(doc.resolve(layer.name));
        }

        // Validate component layer references
        let mut invalid_layer_refs = 0;
        for comp in &step.components {
            let layer_name = doc.resolve(comp.layer_ref);
            if !layer_names.contains(layer_name) {
                invalid_layer_refs += 1;
                if verbose {
                    eprintln!(
                        "  ⚠ Component {} references layer {} (not in this file)",
                        doc.resolve(comp.ref_des),
                        layer_name
                    );
                }
            }
        }

        if invalid_layer_refs > 0 {
            if is_reduced_layer_file {
                println!("  ⚠ {} components reference layers not in this file (expected for specialized views)", invalid_layer_refs);
                warnings += 1;
            } else {
                println!(
                    "  ✗ {} components reference invalid layers",
                    invalid_layer_refs
                );
                errors += invalid_layer_refs;
            }
        } else {
            println!("  ✓ All component layer references valid");
        }
        println!();

        // === DRILL VALIDATION ===
        println!("Drills:");
        let mut total_drills = 0;
        let mut via_drills = 0;
        let mut plated_drills = 0;
        let mut nonplated_drills = 0;

        for feature in &step.layer_features {
            let layer_name = doc.resolve(feature.layer_ref);
            let is_drill_layer = ecad.cad_data.layers.iter().any(|l| {
                doc.resolve(l.name) == layer_name && l.layer_function == LayerFunction::Drill
            });

            if is_drill_layer {
                for set in &feature.sets {
                    for hole in &set.holes {
                        total_drills += 1;
                        match hole.plating_status {
                            PlatingStatus::Via => via_drills += 1,
                            PlatingStatus::Plated => plated_drills += 1,
                            PlatingStatus::NonPlated => nonplated_drills += 1,
                        }
                    }
                }
            }
        }

        let total_plated = via_drills + plated_drills;
        println!("  {} total drills", total_drills);
        println!(
            "  {} plated ({} via + {} tht)",
            total_plated, via_drills, plated_drills
        );
        println!("  {} non-plated", nonplated_drills);

        if total_drills == 0 {
            println!("  ⚠ No drills found (might be assembly-only file)");
            warnings += 1;
        } else {
            println!("  ✓ Drill data present");
        }
        println!();

        // === LAYER FEATURES VALIDATION ===
        if verbose {
            println!("Layer Features:");
            let mut total_pads = 0;
            let mut total_traces = 0;

            for feature in &step.layer_features {
                for set in &feature.sets {
                    total_pads += set.pads.len();
                    total_traces += set.traces.len();
                }
            }

            println!("  {} pad instances", total_pads);
            println!("  {} trace segments", total_traces);
            println!();
        }

        // === STACKUP VALIDATION ===
        if let Some(stackup) = &ecad.cad_data.stackup {
            println!("Stackup:");
            println!("  Name: {}", doc.resolve(stackup.name));
            if let Some(thickness) = stackup.overall_thickness {
                println!(
                    "  Overall thickness: {:.4}\" ({:.1} mils)",
                    thickness,
                    thickness * 1000.0
                );
            }
            if !stackup.layers.is_empty() {
                println!("  {} stackup layers defined", stackup.layers.len());

                if verbose {
                    for (i, layer) in stackup.layers.iter().enumerate() {
                        let layer_name = doc.resolve(layer.layer_ref);
                        print!("    Layer {}: {} ", i + 1, layer_name);
                        if let Some(t) = layer.thickness {
                            print!("({:.4}\")", t);
                        }
                        if let Some(dk) = layer.dielectric_constant {
                            print!(" Dk={:.2}", dk);
                        }
                        println!();
                    }
                }
            }
            println!("  ✓ Stackup data present");
            println!();
        }

        // === BOARD DIMENSIONS ===
        if let Some(profile) = &step.profile {
            println!("Board Profile:");
            let polygon = &profile.polygon;

            let mut min_x = polygon.begin.x;
            let mut max_x = polygon.begin.x;
            let mut min_y = polygon.begin.y;
            let mut max_y = polygon.begin.y;

            for poly_step in &polygon.steps {
                let (x, y) = match poly_step {
                    ipc_2581::PolyStep::Segment(s) => (s.x, s.y),
                    ipc_2581::PolyStep::Curve(c) => (c.x, c.y),
                };
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }

            let width = max_x - min_x;
            let height = max_y - min_y;

            println!("  Dimensions: {:.4}\" × {:.4}\"", width, height);
            println!("  ✓ Board outline defined");
            println!();
        }
    }

    // Check BOM if present
    if let Some(bom) = doc.bom() {
        println!("━━━ BOM Data ━━━");
        println!("  {} BOM items", bom.items.len());

        let mut mechanical_qty = 0u32;
        let mut electrical_qty = 0u32;
        let mut mechanical_types = 0;

        for item in &bom.items {
            match item.category {
                Some(ipc_2581::BomCategory::Mechanical) => {
                    mechanical_qty += item.quantity.unwrap_or(0);
                    mechanical_types += 1;
                }
                Some(ipc_2581::BomCategory::Electrical) => {
                    electrical_qty += item.quantity.unwrap_or(0);
                }
                None => {}
            }
        }

        println!("  {} electrical components", electrical_qty);
        println!(
            "  {} mechanical components ({} types)",
            mechanical_qty, mechanical_types
        );
        println!("  ✓ BOM data present");
        println!();
    }

    // === SUMMARY ===
    println!("━━━ Summary ━━━");
    if errors == 0 && warnings == 0 {
        println!("✓ All checks passed!");
    } else {
        if errors > 0 {
            println!("✗ {} error(s) found", errors);
        }
        if warnings > 0 {
            println!("⚠ {} warning(s)", warnings);
        }
    }

    if errors > 0 {
        process::exit(1);
    }

    Ok(())
}

fn export_html(path: &PathBuf, output: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let arena = Bump::new();
    let doc = Ipc2581::parse_file(&arena, path)?;

    // Read the original XML file
    let xml_content = fs::read_to_string(path)?;

    // Get filename for download
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("design.xml");

    let html = generate_html(&doc, &xml_content, filename);
    fs::write(output, html)?;

    Ok(())
}
