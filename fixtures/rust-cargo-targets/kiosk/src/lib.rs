pub mod clock;
pub mod stamp;
pub mod till;
pub mod util;

macro_rules! tally {
    ($($tokens:tt)*) => {
        0
    };
}

pub fn total() -> u32 {
    till::sum() + 1
}

pub fn helper() -> u32 {
    0
}

pub fn report() -> String {
    // a path inside a macro call that takes expressions
    format!("{:?}", util::shared())
}

pub fn counted() -> u32 {
    // arguments that are no expressions: not read, but recorded
    tally!(stamp::mark => 1)
}

pub fn named() -> &'static str {
    // tokens to print: no use of clock, though they read as an expression
    stringify!(crate::clock::now)
}
