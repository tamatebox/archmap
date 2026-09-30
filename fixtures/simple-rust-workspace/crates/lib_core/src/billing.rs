pub trait Charge {
    fn charge(&self, amount: u64) -> bool;
}

pub const CURRENCY: &str = "JPY";
