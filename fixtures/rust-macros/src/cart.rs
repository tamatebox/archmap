use crate::charge;

pub fn total() -> u32 {
    settle!(1) + charge::refund(0)
}
