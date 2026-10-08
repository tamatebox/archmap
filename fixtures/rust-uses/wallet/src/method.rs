pub trait Method {
    fn fee(&self) -> u32;
}

pub fn flat() -> u32 {
    1
}
