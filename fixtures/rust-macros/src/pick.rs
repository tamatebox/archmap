use crate::charge;

// the call gives the function's name: the body names only its module
macro_rules! call {
    ($f:ident, $n:expr) => {
        charge::$f($n)
    };
}

pub fn pick() -> u32 {
    call!(pay, 4)
}
