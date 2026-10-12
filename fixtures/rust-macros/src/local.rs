pub fn local() -> u32 {
    // a block's binding of the module, which a macro of the block pastes
    use crate::charge as c;
    macro_rules! here {
        ($f:ident, $n:expr) => {
            c::$f($n)
        };
    }
    here!(pay, 7)
}
