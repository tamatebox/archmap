mod parse;
pub mod split;

pub use parse::parse;
pub use split::split;

pub fn twice(text: &str) -> u32 {
    parse(text) * 2
}
