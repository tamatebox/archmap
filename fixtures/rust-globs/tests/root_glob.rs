use market::*;

#[test]
fn pays() {
    assert_eq!(pay(1), 1);
    let _receipt: Receipt = Receipt::new();
    assert_eq!(charge::refund(2), 2);
    assert_eq!(settle!(3), 3);
    // the standard macro, not the function `concat` the glob brings in
    let _joined = concat!("a", "b");
}
