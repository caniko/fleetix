#![cfg(feature = "pkl")]

// Required literal-union properties must evaluate under Pklr 5 (jdx/pklr#332).
#[test]
fn body_assertion_operator_evaluates_through_the_real_schema() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Schema.pkl"),
        include_str!("../lib/topology/Schema.pkl"),
    )
    .unwrap();
    let source = dir.path().join("Body.pkl");
    std::fs::write(
        &source,
        "import \"Schema.pkl\" as S\nassertion: S.BodyAssertion = new { path = \"status\"; operator = \"equals\"; value = \"ok\" }\n",
    )
    .unwrap();
    let value: serde_json::Value = fleetix::pkl::load_sync(&source).unwrap();
    assert_eq!(value["assertion"]["operator"], "equals");
}
