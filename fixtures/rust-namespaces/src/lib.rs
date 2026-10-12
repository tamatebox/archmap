mod parse;
pub mod split;

pub use parse::parse;
pub use split::split;

pub fn twice(text: &str) -> u32 {
    parse(text) * 2
}

pub mod bake;
pub mod bill;
pub mod fee;
pub mod model;
pub mod rate;
pub mod read;

// a public module and a public function of one name
pub fn bake() -> u32 {
    bake::oven()
}

pub fn twice_baked() -> u32 {
    bake() + bake()
}
