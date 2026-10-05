use bakery::parse;
use bakery::split;

#[test]
fn reads() {
    assert_eq!(parse("ab"), 2);
    assert_eq!(bakery::parse("abc"), 3);
    assert_eq!(split("a,b"), 2);
    assert_eq!(split::helper(), 0);
}
