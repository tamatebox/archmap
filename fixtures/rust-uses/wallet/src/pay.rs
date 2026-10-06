pub fn charge() -> u32 {
    1
}

pub mod inner {
    pub fn charge() -> u32 {
        2
    }
}

pub fn total() -> u32 {
    inner::charge() + charge()
}
