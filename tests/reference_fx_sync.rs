use std::fs;
#[test]
fn nbu_adapter_has_no_floating_point_or_raw_body_logging() {
    let source = fs::read_to_string("src/contexts/reference_data/infrastructure/nbu.rs").unwrap();
    assert!(!source.contains("f32"));
    assert!(!source.contains("f64"));
    assert!(!source.contains("raw_body"));
}
