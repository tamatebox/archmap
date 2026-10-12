// its body names `pay`, which each call pastes where it is written
macro_rules! settle {
    ($n:expr) => {
        $crate::charge::pay($n)
    };
}

// its body names only `settle!`, which it pastes in turn
macro_rules! checkout {
    ($n:expr) => {
        settle!($n)
    };
}
