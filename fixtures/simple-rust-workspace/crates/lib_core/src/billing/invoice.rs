use super::CURRENCY;
use crate::store::Ledger;
use crate::User;

pub struct Invoice {
    pub user: User,
    pub currency: &'static str,
}

pub fn issue(user: User, ledger: &Ledger) -> Invoice {
    let currency = if ledger.open { CURRENCY } else { "" };
    Invoice { user, currency }
}
