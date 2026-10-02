use kiosk::clock::now;

pub fn helper() {}

fn main() {
    let _ = now();
}

#[cfg(test)]
mod tests {
    use kiosk::stamp::mark;

    #[test]
    fn runs() {
        mark();
    }
}
