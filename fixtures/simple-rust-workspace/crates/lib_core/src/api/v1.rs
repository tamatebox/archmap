use crate::User;

pub fn lookup(id: u64) -> User {
    User::new(id)
}
