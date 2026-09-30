use crate::User;

pub fn lookup(id: u64) -> User {
    User::new(id)
}

pub fn currency() -> &'static str {
    crate::billing::CURRENCY
}

pub fn charge(item: &dyn crate::billing::Charge) -> bool {
    item.charge(1)
}
