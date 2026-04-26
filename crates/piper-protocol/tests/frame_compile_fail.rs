#[test]
fn frame_api_privacy_compile_failures() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/direct_field_access.rs");
    t.compile_fail("tests/ui/struct_literal.rs");
    t.compile_fail("tests/ui/from_exact_too_long.rs");
    t.compile_fail("tests/ui/unchecked_const_constructor.rs");
}
