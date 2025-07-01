mod common;
use common::TestProject;

#[test]
fn module_loader_name_override() {
    let env = TestProject::new();

    env.add_file("sub.star", "# empty sub module\n");

    env.add_file(
        "top.star",
        r#"
load(".", Sub = "sub")
Sub(name = "PowerStage")
"#,
    );

    star_snapshot!(env, "top.star");
}
