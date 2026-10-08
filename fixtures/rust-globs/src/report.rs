use crate::*;

// a macro, not the module `audit` the glob brings in
pub fn run() -> u32 {
    audit!(1)
}
