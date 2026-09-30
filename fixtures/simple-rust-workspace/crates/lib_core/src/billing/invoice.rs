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

pub fn issue_now(user: User) -> Invoice {
    let ledger = crate::store::open();
    let _ = crate::store::open();
    issue(user, &ledger)
}
