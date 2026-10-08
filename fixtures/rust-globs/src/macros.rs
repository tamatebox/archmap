// `audit!` shares its name with the module `audit`, which a glob of the
// crate root brings in; only `settle!` is exported
macro_rules! audit {
    ($e:expr) => {
        $e
    };
}

#[macro_export]
macro_rules! settle {
    ($n:expr) => {
        $crate::pay($n)
    };
}
