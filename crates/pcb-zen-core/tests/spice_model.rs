snapshot_eval!(model_parsing, {
    "r.lib" => r#"
.SUBCKT my_resistor p n PARAMS: RVAL=1k
R1 p n {RVAL}
.ENDS my_resistor
    "#,
    "test.zen" => r#"
        P1 = io(Net)
        P2 = io(Net)
        SpiceModel('r.lib', 'my_resistor', nets=[P1, P2], args={"RVAL" : "1000" })
    "#
});

snapshot_eval!(model_parsing_bad_name, {
    "r.lib" => r#"
.SUBCKT my_resistor p n PARAMS: RVAL=1k
R1 p n {RVAL}
.ENDS my_resistor
    "#,
    "test.zen" => r#"
        P1 = io(Net)
        P2 = io(Net)
        SpiceModel('r.lib', 'foo', nets=[P1, P2], args={"RVAL" : "1000" })
    "#
});

snapshot_eval!(model_parsing_missing_param, {
    "r.lib" => r#"
.SUBCKT my_resistor p n PARAMS: RVAL
R1 p n {RVAL}
.ENDS my_resistor
    "#,
    "test.zen" => r#"
        P1 = io(Net)
        P2 = io(Net)
        SpiceModel('r.lib', 'my_resistor', nets=[P1, P2], args={})
    "#
});

snapshot_eval!(model_parsing_unexpected_param, {
    "r.lib" => r#"
.SUBCKT my_resistor p n
+PARAMS: RVAL=1
R1 p n {RVAL}
.ENDS my_resistor
    "#,
    "test.zen" => r#"
        P1 = io(Net)
        P2 = io(Net)
        print(SpiceModel('r.lib', 'my_resistor', nets=[P1, P2], args={"FOO": "123", "RVAL": "1"}))
    "#
});

#[test]
fn model_parsing_empty_param_name() {
    for params in ["PARAMS: RVAL=1k =", "\n+PARAMS: RVAL=1k ="] {
        let result = crate::common::eval_zen(vec![
            (
                "r.lib".into(),
                format!(".SUBCKT my_resistor p n {params}\nR1 p n {{RVAL}}\n.ENDS"),
            ),
            (
                "test.zen".into(),
                "SpiceModel('r.lib', 'my_resistor', nets=[Net('P'), Net('N')], args={'RVAL': '1k'})".into(),
            ),
        ]);
        assert!(!result.is_success());
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .to_string()
                .contains("Invalid PARAMS syntax: '='")
        }));
    }
}
