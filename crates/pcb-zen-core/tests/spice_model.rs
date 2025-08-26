#[macro_use]
mod common;

snapshot_eval!(model_parsing, {
    "r.lib" => r#"
.SUBCKT my_resistor p n PARAMS: RVAL=1k
R1 p n {RVAL}
.ENDS my_resistor
    "#,
    "test.zen" => r#"
        P1 = io("P1", Net)
        P2 = io("P2", Net)
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
        P1 = io("P1", Net)
        P2 = io("P2", Net)
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
        P1 = io("P1", Net)
        P2 = io("P2", Net)
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
        P1 = io("P1", Net)
        P2 = io("P2", Net)
        print(SpiceModel('r.lib', 'my_resistor', nets=[P1, P2], args={"FOO": "123", "RVAL": "1"}))
    "#
});
