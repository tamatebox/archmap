use lib_core::billing::CURRENCY;
use lib_core::{greet, User};

mod config;

fn main() {
    let user = User::new(1);
    println!("{} {}", greet(&user), CURRENCY);
    config::load();
}

#[cfg(test)]
mod tests {
    use assert_cmd::Command;
}
