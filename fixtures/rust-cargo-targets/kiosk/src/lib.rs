pub mod clock;
pub mod stamp;
pub mod till;
pub mod util;

pub fn total() -> u32 {
    till::sum() + 1
}

pub fn helper() -> u32 {
    0
}
