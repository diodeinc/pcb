#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;

#[test]
fn print_macros_exit_cleanly_when_output_pipe_closes() {
    let source = (0..20_000)
        .map(|index| format!("value_{index}=[1,2,3]\n"))
        .collect::<String>();
    let mut sandbox = Sandbox::new();
    sandbox.write("large.zen", source);

    let command = format!(
        "\"{}\" fmt --diff large.zen | head -n 1 >/dev/null",
        env!("CARGO_BIN_EXE_pcbc")
    );
    let output = sandbox
        .cmd("bash", ["-o", "pipefail", "-c", &command])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run pcb fmt through head");

    assert!(
        output.status.success(),
        "pcb fmt failed after its output pipe closed: {output:?}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("panicked"),
        "pcb fmt panicked after its output pipe closed: {output:?}"
    );
}
