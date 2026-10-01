//! Fixture library crate.
use serde::Serialize;

pub mod billing;
mod store;

pub use billing::invoice::Invoice;

pub mod api {
    pub mod v1;
}

#[derive(Serialize)]
pub struct User {
    pub id: u64,
}

impl User {
    pub fn new(id: u64) -> Self {
        Self { id }
    }

    fn secret(&self) -> u64 {
        self.id
    }
}

pub fn greet(user: &User) -> String {
    format!("hello {}", user.secret())
}

fn private_helper() {}

mod internal {
    pub fn hidden() {}
}

pub mod prelude {
    pub use crate::billing::invoice::issue;
}

impl Invoice {
    pub fn total(&self) -> u64 {
        0
    }
}
