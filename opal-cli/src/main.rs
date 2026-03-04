use opalc::dummy_compile;

fn main() {
    let source = r#"
        (type ['a Option (
            None
            (Some ~ 'a)))

    "#;

    dummy_compile(source);
}
