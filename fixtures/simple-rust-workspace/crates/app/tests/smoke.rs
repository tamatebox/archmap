use assert_cmd::Command;

#[test]
fn runs() {
    Command::cargo_bin("app").unwrap().assert().success();
}
