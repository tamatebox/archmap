pub mod invoice;

pub trait Charge {
    fn charge(&self, amount: u64) -> bool;
}

pub const CURRENCY: &str = "JPY";

#[cfg(test)]
mod tests {
    use crate::store::open;

    #[test]
    fn opens() {
        crate::store::open();
    }
}

#[derive(serde::Serialize)]
pub struct Receipt;
