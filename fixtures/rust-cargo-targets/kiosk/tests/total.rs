mod common;
mod shared;

use kiosk::total;
use pretty_assertions::assert_eq;

pub fn helper() {}

#[test]
fn adds() {
    common::setup();
    shared::warm();
    assert_eq!(total(), 3);
}
