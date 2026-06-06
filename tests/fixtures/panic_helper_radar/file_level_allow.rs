#![allow(clippy::expect_used, clippy::unwrap_used)]

pub fn file_level_allow_fixture() {
    let _value = Some(1).expect("fixture intentionally allows expect");
    let _err = Result::<(), &str>::Err("fixture").unwrap_err();
}
