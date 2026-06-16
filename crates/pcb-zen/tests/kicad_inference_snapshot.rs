mod common;
use common::TestProject;

#[test]
fn snapshot_kicad_symbol_footprint_inference() {
    let env = TestProject::new();
    env.add_file("pcb.toml", common::KICAD_WORKSPACE_TOML);

    env.add_file(
        "top.zen",
        r#"
Component(
    name = "U1",
    part = Part(mpn = "TEST", manufacturer = "TEST"),
    symbol = Symbol(library = "@kicad-symbols/Amplifier_Current.kicad_sym", name = "INA240A1D"),
    pins = {
        "+": Net("INP"),
        "-": Net("INN"),
        "V+": Power("VP"),
        "GND": Ground("GND"),
        "REF1": Net("R1"),
        "REF2": Net("R2"),
        "5": Net("OUT"),
    },
)
"#,
    );

    star_snapshot!(env, "top.zen");
}
