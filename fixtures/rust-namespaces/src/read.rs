use crate::fee::Pay;
use crate::model;

// a method of a type that its `impl` writes with a path
impl model::Order {
    pub fn make() -> Self {
        model::Order(Pay())
    }
}

impl Pay for model::Order {
    fn pay(&self) -> u32 {
        self.0
    }
}
