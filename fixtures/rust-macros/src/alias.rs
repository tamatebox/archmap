use crate::charge as c;

// the module by the name this file binds, with the function a call gives
macro_rules! invoke {
    ($f:ident, $n:expr) => {
        c::$f($n)
    };
}

pub fn alias() -> u32 {
    invoke!(pay, 5)
}
