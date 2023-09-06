fn main() -> std::io::Result<()> {
    cfg_if::cfg_if! {
        if #[cfg(feature = "korosensei")] {
            Ok(())
        } else if #[cfg(feature = "boost")] {
            Ok(())
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData,"need to enable korosensei or boost feature"))
        }
    }
}
