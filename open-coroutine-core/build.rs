fn main() -> std::io::Result<()> {
    cfg_if::cfg_if! {
        if #[cfg(all(feature = "korosensei", feature = "boost"))] {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                "korosensei and boost feature cannot be enabled simultaneously"))
        } else if #[cfg(any(feature = "korosensei", feature = "boost"))] {
            Ok(())
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                "need to enable korosensei or boost feature"))
        }
    }
}
