//! The score engraver: Verovio, through the `verovio` crate. Nothing engraves yet; the call below
//! links its C++ so that the build, its cache and the system packages are settled before the
//! score screen needs them.

/// The version of the Verovio linked in.
pub fn version() -> String {
    verovio::Toolkit::new().version()
}

#[cfg(test)]
mod tests {
    #[test]
    fn verovio_is_linked_and_answers() {
        let version = super::version();
        assert!(
            version.starts_with(|c: char| c.is_ascii_digit()),
            "{version:?}"
        );
    }
}
