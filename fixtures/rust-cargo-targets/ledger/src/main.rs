mod cli;

fn main() {
    cli::run();
    ledger::entry::post();
}
