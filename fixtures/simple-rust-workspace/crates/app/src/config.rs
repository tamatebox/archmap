use lib_core::{Invoice, User};

pub fn load() -> Option<User> {
    None
}

pub fn last_invoice() -> Option<Invoice> {
    None
}

pub fn greeting() -> String {
    lib_core::greet(&User::new(0))
}
