mod common;
use common::TestProject;

#[test]
fn module_loader_name_override() {
    let env = TestProject::new();

    env.add_file("sub.zen", "# empty sub module\n");

    env.add_file(
        "top.zen",
        r#"
load(".", Sub = "sub")
Sub(name = "PowerStage")
"#,
    );

    star_snapshot!(env, "top.zen");
}
