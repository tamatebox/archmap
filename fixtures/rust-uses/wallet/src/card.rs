use crate::method::Method;

pub struct Card;

impl Method for Card {
    fn fee(&self) -> u32 {
        1
    }
}

pub fn total<T: Method>(m: &T) -> u32 {
    m.fee()
}

pub fn boxed() -> Box<dyn Method> {
    Box::new(Card)
}

pub fn opaque() -> impl Method {
    Card
}

pub fn bound<T>(m: &T) -> u32
where
    T: Method,
{
    m.fee()
}
