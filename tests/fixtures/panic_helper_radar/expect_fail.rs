pub fn expect_fail_fixture() {
    let _value = Some(1).expect("fixture should report expect_used");
}
