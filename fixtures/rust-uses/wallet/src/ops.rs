use crate::model::Order as Purchase;

// methods of a type another file defines, through an alias
impl Purchase {
    pub fn make() -> Self {
        Purchase(1)
    }
}
