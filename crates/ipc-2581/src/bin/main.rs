use bumpalo::Bump;
use clap::{Parser, Subcommand};
use ipc_2581::html_generator::generate_html;
use ipc_2581::svg_export::{
    build_board_context, convert_to_paths, expand_padstacks, flatten_layers, resolve_features,
    FeatureBucket, PipelineTiming, ResolvedFeature, ResolvedGeometry,
};
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
    /// Export copper layers to SVG (staged pipeline)
    ExportSvg {
        /// Path to IPC-2581 XML file
        file: PathBuf,

        /// Output SVG file path
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Comma-separated list of layers to export (e.g., "TOP,BOTTOM")
        #[arg(short, long)]
        layers: Option<String>,

        /// Dump Stage 1 debug JSON to file
        #[arg(long)]
        dump_stage1: Option<PathBuf>,

        /// Export Stage 3 debug SVG (paths before boolean ops)
        #[arg(long)]
        debug_stage3: Option<PathBuf>,

        /// Export Stage 4 debug SVG (paths after boolean ops)
        #[arg(long)]
        debug_stage4: Option<PathBuf>,

        /// Export only specific bucket (Smd, Pth, Via, Trace, Fill)
        #[arg(long)]
        debug_bucket: Option<String>,

        /// Show timing information
        #[arg(long)]
        timings: bool,
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
        Commands::ExportSvg {
            file,
            output,
            layers,
            dump_stage1,
            debug_stage3,
            debug_stage4,
            debug_bucket,
            timings,
        } => {
            match export_svg(
                &file,
                output.as_ref(),
                layers.as_ref(),
                dump_stage1.as_ref(),
                debug_stage3.as_ref(),
                debug_stage4.as_ref(),
                debug_bucket.as_ref(),
                timings,
            ) {
                Ok(_) => {}
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
        if let Some(stackup) = ecad.cad_data.stackups.first() {
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

fn export_svg(
    input_path: &PathBuf,
    _output_path: Option<&PathBuf>,
    layers: Option<&String>,
    dump_stage1_path: Option<&PathBuf>,
    debug_stage3_path: Option<&PathBuf>,
    debug_stage4_path: Option<&PathBuf>,
    debug_bucket: Option<&String>,
    show_timings: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("━━━ SVG Export Pipeline ━━━");
    println!("Input: {}", input_path.display());

    let mut timing = PipelineTiming::new();

    // Parse layer filter if provided
    let layer_filter: Option<Vec<String>> =
        layers.map(|s| s.split(',').map(|l| l.trim().to_string()).collect());

    if let Some(ref layers) = layer_filter {
        println!("Layers: {}", layers.join(", "));
    } else {
        println!("Layers: ALL");
    }
    println!();

    // Stage 0: Input Readiness
    println!("Stage 0: Input Readiness");
    let stage0_timer = std::time::Instant::now();

    let arena = Bump::new();
    let doc = Ipc2581::parse_file(&arena, input_path)?;
    let context = build_board_context(&doc)?;

    timing.stage0_input = Some(stage0_timer.elapsed());

    println!("  ✓ Parsed IPC-2581 file");
    println!("  Board: {}", context.board_name);
    println!(
        "  Units: {} → mm (factor: {:.4})",
        context.original_units, context.to_mm_factor
    );
    context.stats.print_summary();

    // Debug: Show line descriptors
    if !context.line_descriptors.is_empty() {
        println!("\n  Line Descriptors (first 5):");
        for (sym, line_desc) in context.line_descriptors.iter().take(5) {
            println!(
                "    {}: width={:.6} mm",
                doc.resolve(*sym),
                line_desc.line_width
            );
        }
    }

    println!();

    // Stage 1: Transform Resolution
    println!("Stage 1: Transform Resolution");
    let stage1_timer = std::time::Instant::now();

    let layer_resolutions = resolve_features(&doc, &context, layer_filter.as_deref())?;

    timing.stage1_transforms = Some(stage1_timer.elapsed());

    println!("  ✓ Resolved {} layers", layer_resolutions.len());

    for (layer_name, resolution) in &layer_resolutions {
        println!("  Layer: {}", layer_name);
        println!("    Features:  {}", resolution.features.len());
        println!("      SMD:     {}", resolution.stats.smd_count);
        println!("      PTH:     {}", resolution.stats.pth_count);
        println!("      Via:     {}", resolution.stats.via_count);
        println!("      Trace:   {}", resolution.stats.trace_count);
        println!("      Fill:    {}", resolution.stats.fill_count);
        println!("      Cutout:  {}", resolution.stats.cutout_count);
        println!(
            "    BBox: ({:.3}, {:.3}) to ({:.3}, {:.3}) mm",
            resolution.bbox.min_x,
            resolution.bbox.min_y,
            resolution.bbox.max_x,
            resolution.bbox.max_y
        );
        println!(
            "    Size: {:.3} × {:.3} mm",
            resolution.bbox.width(),
            resolution.bbox.height()
        );

        // Show sample net names
        let mut nets_seen = std::collections::HashSet::new();
        for feature in &resolution.features {
            if let Some(net_sym) = feature.net {
                nets_seen.insert(doc.resolve(net_sym));
            }
        }
        if !nets_seen.is_empty() {
            let nets: Vec<&str> = nets_seen.iter().take(5).copied().collect();
            println!(
                "    Sample nets: {}{}",
                nets.join(", "),
                if nets_seen.len() > 5 {
                    format!(" (+{} more)", nets_seen.len() - 5)
                } else {
                    "".to_string()
                }
            );
        }
    }
    println!();

    // Dump Stage 1 debug info if requested
    if let Some(dump_path) = dump_stage1_path {
        println!("Writing Stage 1 debug info to {}", dump_path.display());
        let mut output = String::new();
        for (layer_name, resolution) in &layer_resolutions {
            output.push_str(&format!("Layer: {}\n", layer_name));
            output.push_str(&format!("  Features: {}\n\n", resolution.features.len()));

            // Group by bucket for clearer output
            let mut by_bucket: std::collections::HashMap<FeatureBucket, Vec<&ResolvedFeature>> =
                std::collections::HashMap::new();
            for feature in &resolution.features {
                by_bucket.entry(feature.bucket).or_default().push(feature);
            }

            for (bucket, features) in &by_bucket {
                output.push_str(&format!("  {:?} ({} features):\n", bucket, features.len()));
                for feature in features.iter().take(10) {
                    let net_str = feature.net.map(|s| doc.resolve(s)).unwrap_or("<no net>");
                    output.push_str(&format!("    [{:?}] net={}\n", bucket, net_str));
                    output.push_str(&format!("        {:?}\n", feature.geometry));
                    output.push_str(&format!(
                        "        bbox: ({:.3}, {:.3}) to ({:.3}, {:.3})\n",
                        feature.bbox.min_x,
                        feature.bbox.min_y,
                        feature.bbox.max_x,
                        feature.bbox.max_y
                    ));
                }
                if features.len() > 10 {
                    output.push_str(&format!("    ... and {} more\n", features.len() - 10));
                }
                output.push('\n');
            }
        }
        let output_len = output.len();
        fs::write(dump_path, output)?;
        println!("  ✓ Debug info written ({} bytes)", output_len);
        println!();
    }

    // Stage 2: Padstack Expansion
    println!("Stage 2: Padstack Expansion");
    let stage2_timer = std::time::Instant::now();

    let layer_resolutions = expand_padstacks(&doc, &context, layer_resolutions)?;

    timing.stage2_padstacks = Some(stage2_timer.elapsed());

    println!("  ✓ Expanded padstack references to concrete geometries");

    // Show expanded pad statistics
    for (layer_name, resolution) in &layer_resolutions {
        let mut padstack_refs = 0;
        let mut circles = 0;
        let mut rectangles = 0;
        let mut polygons = 0;
        let mut polylines = 0;
        let mut ellipses = 0;
        let mut ovals = 0;
        let mut donuts = 0;
        let mut thermals = 0;
        let mut groups = 0;

        for feature in &resolution.features {
            match &feature.geometry {
                ResolvedGeometry::PadstackRef { .. } => padstack_refs += 1,
                ResolvedGeometry::Circle { .. } => circles += 1,
                ResolvedGeometry::Rectangle { .. } => rectangles += 1,
                ResolvedGeometry::RoundedRectangle { .. } => rectangles += 1,
                ResolvedGeometry::ChamferedRectangle { .. } => rectangles += 1,
                ResolvedGeometry::Polygon { .. } => polygons += 1,
                ResolvedGeometry::Polyline { .. } => polylines += 1,
                ResolvedGeometry::Ellipse { .. } => ellipses += 1,
                ResolvedGeometry::Oval { .. } => ovals += 1,
                ResolvedGeometry::Donut { .. } => donuts += 1,
                ResolvedGeometry::Thermal { .. } => thermals += 1,
                ResolvedGeometry::Group { .. } => groups += 1,
            }
        }

        let total = padstack_refs
            + circles
            + rectangles
            + polygons
            + polylines
            + ellipses
            + ovals
            + donuts
            + thermals
            + groups;
        if total > 0 {
            println!("  Layer: {}", layer_name);
            if circles > 0 {
                println!("    Circles:        {}", circles);
            }
            if rectangles > 0 {
                println!("    Rectangles:     {}", rectangles);
            }
            if ellipses > 0 {
                println!("    Ellipses:       {}", ellipses);
            }
            if ovals > 0 {
                println!("    Ovals:          {}", ovals);
            }
            if polygons > 0 {
                println!("    Polygons:       {}", polygons);
            }
            if polylines > 0 {
                println!("    Polylines:      {}", polylines);
            }
            if donuts > 0 {
                println!("    Donuts:         {}", donuts);
            }
            if thermals > 0 {
                println!("    Thermals:       {}", thermals);
            }
            if groups > 0 {
                println!("    Groups:         {}", groups);
            }
            if padstack_refs > 0 {
                println!("    PadstackRefs:   {} (unexpanded)", padstack_refs);
            }
        }
    }
    println!();

    // Stage 3: Path Conversion
    println!("Stage 3: Path Conversion");
    let stage3_timer = std::time::Instant::now();

    let layer_paths = convert_to_paths(layer_resolutions)?;

    timing.stage3_primitives = Some(stage3_timer.elapsed());

    println!("  ✓ Converted geometries to Skia paths");

    // Show path statistics
    for (layer_name, paths) in &layer_paths {
        let total_paths = paths.features.len();
        if total_paths > 0 {
            println!("  Layer: {}", layer_name);
            println!("    Total paths:    {}", total_paths);
            println!(
                "    BBox: ({:.3}, {:.3}) to ({:.3}, {:.3}) mm",
                paths.bbox.min_x, paths.bbox.min_y, paths.bbox.max_x, paths.bbox.max_y
            );
            println!(
                "    Size: {:.3} × {:.3} mm",
                paths.bbox.width(),
                paths.bbox.height()
            );
        }
    }
    println!();

    // Export Stage 3 debug SVG if requested
    if let Some(debug_path) = debug_stage3_path {
        println!("Exporting Stage 3 debug SVG...");
        for (layer_name, paths) in &layer_paths {
            let output_path = if layer_paths.len() > 1 {
                // Multiple layers - create one file per layer
                let base = debug_path.file_stem().unwrap().to_str().unwrap();
                let ext = debug_path.extension().and_then(|e| e.to_str()).unwrap_or("svg");
                debug_path.with_file_name(format!("{}_{}.{}", base, layer_name, ext))
            } else {
                debug_path.clone()
            };

            use ipc_2581::svg_export::debug::export_layer_paths_svg;
            export_layer_paths_svg(paths, output_path.to_str().unwrap())?;
        }
        println!();
    }

    // Export individual bucket if requested
    if let Some(bucket_name) = debug_bucket {
        use ipc_2581::svg_export::debug::export_bucket_svg;

        let bucket = match bucket_name.to_lowercase().as_str() {
            "smd" => FeatureBucket::Smd,
            "pth" => FeatureBucket::Pth,
            "via" => FeatureBucket::Via,
            "trace" => FeatureBucket::Trace,
            "fill" => FeatureBucket::Fill,
            "thermal" => FeatureBucket::Thermal,
            "antipad" => FeatureBucket::Antipad,
            "cutout" => FeatureBucket::Cutout,
            _ => {
                eprintln!("Unknown bucket: {}", bucket_name);
                eprintln!("Valid buckets: Smd, Pth, Via, Trace, Fill, Thermal, Antipad, Cutout");
                process::exit(1);
            }
        };

        println!("Exporting {:?} bucket...", bucket);
        for (layer_name, paths) in &layer_paths {
            let output_path = format!("debug_{}_{:?}.svg", layer_name, bucket);
            match export_bucket_svg(paths, bucket, &output_path) {
                Ok(_) => {}
                Err(e) => eprintln!("  Warning: {} - {}", layer_name, e),
            }
        }
        println!();
    }

    // Stage 4: Layer Flattening (copper + drills)
    println!("Stage 4: Layer Flattening");
    let stage4_timer = std::time::Instant::now();

    let flattened_layers = flatten_layers(&doc, layer_paths)?;

    timing.stage4_booleans = Some(stage4_timer.elapsed());

    println!("  ✓ Applied boolean operations per bucket");

    // Show flattened layer statistics
    for (layer_name, flattened) in &flattened_layers {
        println!("  Layer: {}", layer_name);
        println!("    Buckets: {}", flattened.buckets.len());
        for bucket in flattened.buckets.keys() {
            if let Some(stats) = flattened.stats.get(bucket) {
                println!(
                    "      {:?}: {} vertices, {:.2} mm², {} pos + {} neg (union: {}ms, diff: {}ms)",
                    bucket,
                    stats.vertex_count,
                    stats.area_mm2,
                    stats.positive_count,
                    stats.negative_count,
                    stats.union_time_ms,
                    stats.difference_time_ms
                );
            }
        }
        println!(
            "    BBox: ({:.3}, {:.3}) to ({:.3}, {:.3}) mm",
            flattened.bbox.min_x, flattened.bbox.min_y, flattened.bbox.max_x, flattened.bbox.max_y
        );
        println!(
            "    Size: {:.3} × {:.3} mm",
            flattened.bbox.width(),
            flattened.bbox.height()
        );
    }
    println!();

    // Export Stage 4 debug SVG if requested (with drill layer composited on top)
    if let Some(debug_path) = debug_stage4_path {
        println!("Exporting Stage 4 debug SVG (with drill layer)...");
        for (layer_name, flattened) in &flattened_layers {
            let output_path = if flattened_layers.len() > 1 {
                // Multiple layers - create one file per layer
                let base = debug_path.file_stem().unwrap().to_str().unwrap();
                let ext = debug_path.extension().and_then(|e| e.to_str()).unwrap_or("svg");
                debug_path.with_file_name(format!("{}_{}.{}", base, layer_name, ext))
            } else {
                debug_path.clone()
            };

            use ipc_2581::svg_export::debug::export_flattened_svg_with_drill_mask;

            // Get drill layer mask if available
            let drill_mask = flattened_layers
                .get("DRILLS")
                .and_then(|dl| dl.buckets.get(&FeatureBucket::Cutout));

            export_flattened_svg_with_drill_mask(
                flattened,
                drill_mask,
                output_path.to_str().unwrap(),
            )?;
        }
        println!();
    }

    // Print timing summary
    if show_timings {
        timing.print_summary();
        println!();
    }

    // Stage 5-6 not yet implemented
    println!("━━━ Status ━━━");
    println!("✓ Stage 0, 1, 2, 3, 4 complete");
    println!("⚠ Stages 5-6 not yet implemented");
    println!();
    println!("To continue development, next implement:");
    println!("  - Stage 5: Styling (colors per bucket)");
    println!("  - Stage 6: SVG emission (write final document)");

    Ok(())
}
