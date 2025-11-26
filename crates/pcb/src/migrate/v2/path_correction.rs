use anyhow::{Context, Result};
use pcb_zen::ast_utils::{apply_edits, collect_zen_files, visit_string_literals, SourceEdit};
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::module::AstModuleFields;
use std::path::Path;

const REGISTRY_PREFIX: &str = "github.com/diodeinc/registry/";

/// (old_path, new_path) - paths relative to registry root
const PATH_CORRECTIONS: &[(&str, &str)] = &[
    // modules/basic/* -> modules/*/
    (
        "modules/basic/CastellatedHoles/",
        "modules/CastellatedHoles/",
    ),
    ("modules/basic/CrystalWithCaps/", "modules/CrystalWithCaps/"),
    ("modules/basic/I2cPullups", "modules/I2cPullups/I2cPullups"),
    (
        "modules/basic/LedIndicator",
        "modules/LedIndicator/LedIndicator",
    ),
    (
        "modules/basic/LevelShiftFet",
        "modules/LevelShiftFet/LevelShiftFet",
    ),
    (
        "modules/basic/LowPowerFetSwitch",
        "modules/LowPowerFetSwitch/LowPowerFetSwitch",
    ),
    ("modules/basic/PushButton", "modules/PushButton/PushButton"),
    (
        "modules/basic/UartLevelShift",
        "modules/UartLevelShift/UartLevelShift",
    ),
    // modules/usb/* -> modules/*/
    ("modules/usb/UsbC", "modules/UsbC/UsbC"),
    (
        "modules/usb/UsbPdController",
        "modules/UsbPdController/UsbPdController",
    ),
    // modules/robotics/* -> modules/*/
    (
        "modules/robotics/CanTermination",
        "modules/CanTermination/CanTermination",
    ),
    (
        "modules/robotics/CanTerminationSwitch",
        "modules/CanTerminationSwitch/CanTerminationSwitch",
    ),
    (
        "modules/robotics/HighPowerDcDc",
        "modules/HighPowerDcDc/HighPowerDcDc",
    ),
    (
        "modules/robotics/HighPowerRelay",
        "modules/HighPowerRelay/HighPowerRelay",
    ),
    (
        "modules/robotics/HighSideSwitch",
        "modules/HighSideSwitch/HighSideSwitch",
    ),
    (
        "modules/robotics/IdealDiode",
        "modules/IdealDiode/IdealDiode",
    ),
    (
        "modules/robotics/InductiveLoadLowSideSwitch",
        "modules/InductiveLoadLowSideSwitch/InductiveLoadLowSideSwitch",
    ),
    (
        "modules/robotics/LcEmiFilter",
        "modules/LcEmiFilter/LcEmiFilter",
    ),
    (
        "modules/robotics/MicrocontrollerPowerDelay",
        "modules/MicrocontrollerPowerDelay/MicrocontrollerPowerDelay",
    ),
    ("modules/robotics/Ntc", "modules/Ntc/Ntc"),
    ("modules/robotics/PicoProbe", "modules/PicoProbe/PicoProbe"),
    (
        "modules/robotics/Stm32G431WithCanAndUsb",
        "modules/Stm32G431WithCanAndUsb/Stm32G431WithCanAndUsb",
    ),
    (
        "modules/robotics/STM32G4ShuntInterface",
        "modules/STM32G4ShuntInterface/STM32G4ShuntInterface",
    ),
    (
        "modules/robotics/VsenseResistorDivider",
        "modules/VsenseResistorDivider/VsenseResistorDivider",
    ),
    (
        "modules/robotics/AnalogBatteryVoltageLedBar",
        "modules/AnalogBatteryVoltageLedBar/AnalogBatteryVoltageLedBar",
    ),
    // modules/dsp/* -> modules/*/
    (
        "modules/dsp/DualRailSupply",
        "modules/DualRailSupply/DualRailSupply",
    ),
    // common/fuse.zen -> modules/Fuse/Fuse.zen
    ("common/fuse", "modules/Fuse/Fuse"),
    // modules/fuse/* -> modules/Fuse/ (intermediate state)
    ("modules/fuse/fuse", "modules/Fuse/Fuse"),
    // graphics/logos/* -> modules/Logo/
    ("graphics/logos/Logo", "modules/Logo/Logo"),
    // harness/* -> modules/Harness/
    ("harness/harness", "modules/Harness/Harness"),
];

pub fn correct_paths(workspace_root: &Path) -> Result<()> {
    let zen_files = collect_zen_files(workspace_root)?;

    if zen_files.is_empty() {
        eprintln!("  No .zen files found");
        return Ok(());
    }

    let mut converted_count = 0;

    for zen_file in &zen_files {
        let content = std::fs::read_to_string(zen_file)
            .with_context(|| format!("Failed to read {}", zen_file.display()))?;

        if let Some(updated) = convert_file(&content)? {
            std::fs::write(zen_file, &updated)
                .with_context(|| format!("Failed to write {}", zen_file.display()))?;
            eprintln!("  ✓ {}", zen_file.display());
            converted_count += 1;
        }
    }

    if converted_count == 0 {
        eprintln!("  No paths to correct");
    } else {
        eprintln!("  Corrected {} file(s)", converted_count);
    }

    Ok(())
}

fn try_correct_path(path_str: &str) -> Option<String> {
    let rel_path = path_str.strip_prefix(REGISTRY_PREFIX)?;
    for (old_prefix, new_prefix) in PATH_CORRECTIONS {
        if let Some(rest) = rel_path.strip_prefix(old_prefix) {
            return Some(format!("{}{}{}", REGISTRY_PREFIX, new_prefix, rest));
        }
    }
    None
}

fn convert_file(content: &str) -> Result<Option<String>> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = match AstModule::parse("<memory>", content.to_owned(), &dialect) {
        Ok(a) => a,
        Err(_) => return Ok(None),
    };

    let mut edits: Vec<SourceEdit> = Vec::new();

    ast.statement().visit_expr(|expr| {
        visit_string_literals(expr, &mut |s, lit_expr| {
            if let Some(corrected) = try_correct_path(s) {
                let span = ast.codemap().resolve_span(lit_expr.span);
                edits.push((
                    span.begin.line,
                    span.begin.column,
                    span.end.line,
                    span.end.column,
                    format!("\"{}\"", corrected),
                ));
            }
        });
    });

    for stmt in starlark_syntax::syntax::top_level_stmts::top_level_stmts(ast.statement()) {
        let StmtP::Load(load) = &stmt.node else {
            continue;
        };

        let module_path: &str = &load.module.node;
        if let Some(corrected) = try_correct_path(module_path) {
            let span = ast.codemap().resolve_span(load.module.span);
            edits.push((
                span.begin.line,
                span.begin.column,
                span.end.line,
                span.end.column,
                format!("\"{}\"", corrected),
            ));
        }
    }

    if edits.is_empty() {
        return Ok(None);
    }

    let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
    apply_edits(&mut lines, edits);
    Ok(Some(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_correct_path() {
        // modules/basic/* -> modules/*/
        assert_eq!(
            try_correct_path(
                "github.com/diodeinc/registry/modules/basic/CastellatedHoles/CastellatedHoles.zen"
            ),
            Some(
                "github.com/diodeinc/registry/modules/CastellatedHoles/CastellatedHoles.zen"
                    .to_string()
            )
        );

        // graphics/logos/* -> modules/Logo/
        assert_eq!(
            try_correct_path("github.com/diodeinc/registry/graphics/logos/Logo.zen"),
            Some("github.com/diodeinc/registry/modules/Logo/Logo.zen".to_string())
        );

        // modules/usb/* -> modules/*/
        assert_eq!(
            try_correct_path("github.com/diodeinc/registry/modules/usb/UsbC.zen"),
            Some("github.com/diodeinc/registry/modules/UsbC/UsbC.zen".to_string())
        );

        // modules/robotics/* -> modules/*/
        assert_eq!(
            try_correct_path("github.com/diodeinc/registry/modules/robotics/IdealDiode.zen"),
            Some("github.com/diodeinc/registry/modules/IdealDiode/IdealDiode.zen".to_string())
        );

        // harness/* -> modules/Harness/
        assert_eq!(
            try_correct_path("github.com/diodeinc/registry/harness/harness.zen"),
            Some("github.com/diodeinc/registry/modules/Harness/Harness.zen".to_string())
        );

        // No change needed
        assert_eq!(
            try_correct_path("github.com/diodeinc/registry/components/LED/LED.zen"),
            None
        );

        assert_eq!(try_correct_path("@stdlib/units.zen"), None);
    }

    #[test]
    fn test_convert_file() -> Result<()> {
        let content = r#"load("github.com/diodeinc/registry/modules/usb/UsbC.zen", "UsbC")
MyModule = Module("github.com/diodeinc/registry/graphics/logos/Logo.zen")
"#;

        let result = convert_file(content)?;
        assert!(result.is_some());

        let updated = result.unwrap();
        assert!(updated.contains("\"github.com/diodeinc/registry/modules/UsbC/UsbC.zen\""));
        assert!(updated.contains("\"github.com/diodeinc/registry/modules/Logo/Logo.zen\""));

        Ok(())
    }
}
