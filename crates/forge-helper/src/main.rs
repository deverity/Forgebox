//! Forgebox elevated helper (forgebox-helper): allowlisted privileged operations with logging.
//!
//! Placeholder: prints name and version and exits 0 until its roadmap task.

const BIN_NAME: &str = "forgebox-helper";

fn banner() -> String {
    format!("{BIN_NAME} {}", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::banner;

    #[test]
    fn banner_has_name_and_version() {
        assert_eq!(
            banner(),
            format!("forgebox-helper {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
