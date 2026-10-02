mod common;

pub fn helper() {}

#[test]
fn counts() {
    let n = kiosk::till::sum();
    common::setup();
    let _ = n;
}
