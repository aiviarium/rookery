/// Invalid filter syntax or a value incompatible with a field's kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
