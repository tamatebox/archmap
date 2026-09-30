mod memory;

use crate::billing::CURRENCY;

pub struct Ledger {
    pub open: bool,
}

pub fn open() -> Ledger {
    use self::memory::Backend;
    Backend::attach(CURRENCY);
    Ledger { open: true }
}
