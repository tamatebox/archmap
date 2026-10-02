use kiosk::total;

pub fn helper() {}

fn main() {
    let _ = total();
}

#[cfg(test)]
mod tests {
    use kiosk::helper;

    #[test]
    fn runs() {
        helper();
    }
}
