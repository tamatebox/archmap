use market::*;

#[test]
fn counts() {
    let pay = 3;
    let doubled = |pay: u32| pay * 2;
    assert_eq!(doubled(pay), 6);
}
