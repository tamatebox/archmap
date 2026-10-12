pub trait Pay {
    fn pay(&self) -> u32;
}

// a function of the trait's name, which the compiler keeps apart
#[allow(non_snake_case)]
pub fn Pay() -> u32 {
    1
}
