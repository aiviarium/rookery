use crate::{Error, Result};

/// Validates a reference and returns its scheme and identifier without changing either.
pub fn parse_reference(value: &str) -> Result<(&str, &str)> {
    let s = value;
    let (scheme, id) = s
        .split_once("://")
        .ok_or_else(|| Error::invalid(format!("not an entity URI: {s:?}")))?;
    if !valid_scheme(scheme) {
        return Err(Error::invalid(format!("bad scheme in URI {s:?}")));
    }
    if id.is_empty() || id.len() > 512 || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::invalid(format!("bad id in URI {s:?}")));
    }
    Ok((scheme, id))
}

pub fn valid_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && s.len() <= 32
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))
}
