use graphlib::graph::build;

#[test]
fn it_builds() {
    assert_eq!(build(9).from, 9);
}
