use crate::charge;

// a macro whose body names nothing of `charge`
macro_rules! double {
    ($n:expr) => {
        $n * 2
    };
}

pub fn back() -> u32 {
    double!(charge::refund(1))
}
