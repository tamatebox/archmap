use crate::charge as c;

// the call gives the module and the function
macro_rules! dispatch {
    ($m:ident, $f:ident, $n:expr) => {
        $m::$f($n)
    };
}

pub fn pass() -> u32 {
    dispatch!(c, pay, 6)
}
