pub mod model;
pub mod ops;
pub mod pay;

pub fn parse(text: &str) -> Option<u32> {
    text.parse().ok()
}

pub fn use_it() -> u32 {
    pay::inner::charge() + model::Order::make().0
}

#[cfg(test)]
mod tests {
    // a helper of the same name, which calls the crate's own
    fn parse(text: &str) -> u32 {
        super::parse(text).unwrap()
    }

    #[test]
    fn reads() {
        assert_eq!(parse("1"), 1);
    }
}
