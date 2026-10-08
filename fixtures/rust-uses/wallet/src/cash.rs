use crate::method::*;

pub struct Cash;

// the trait, brought in by the glob
impl Method for Cash {
    fn fee(&self) -> u32 {
        flat()
    }
}
