pub fn unwrap_fail_fixture() {
    let _err = Result::<(), &str>::Err("fixture should report unwrap_used").unwrap_err();
}
