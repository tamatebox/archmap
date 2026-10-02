use kiosk::clock::now;

pub fn helper() {}

macro_rules! tally2 {
    ($($tokens:tt)*) => {
        0
    };
}

fn main() {
    let _ = now();
    let _ = counted();
}

// arguments that are no expressions, naming the library's `till`
fn counted() -> u32 {
    tally2!(kiosk::till::sum => 1)
}

#[cfg(test)]
mod tests {
    use kiosk::stamp::mark;

    #[test]
    fn runs() {
        mark();
    }
}
