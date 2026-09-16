//! The only place a user-supplied name is judged. File names, folder names and account
//! names all pass through here before touching the disk or the database.

use crate::{Error, Result};

/// A single path component: no separators, no traversal, nothing invisible.
pub fn file_name(name: &str) -> Result<&str> {
    if name.is_empty() {
        return Err(Error::BadRequest("the name is empty".into()));
    }
    if name.len() > 255 {
        return Err(Error::BadRequest("the name is longer than 255 bytes".into()));
    }
    if name == "." || name == ".." {
        return Err(Error::BadRequest("that name is reserved".into()));
    }
    if name.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return Err(Error::BadRequest("the name contains a character that is not allowed".into()));
    }
    if name != name.trim() {
        return Err(Error::BadRequest("the name starts or ends with a space".into()));
    }
    Ok(name)
}

/// Account names: letters, digits, dot, dash, underscore; 1 to 32 characters; ASCII so
/// the same name works as a mail local part and a WebDAV login.
pub fn user_name(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
    if ok { Ok(name) } else { Err(Error::BadRequest("a name uses letters, digits, dots, dashes or underscores (1 to 32)".into())) }
}

pub fn password(p: &str) -> Result<&str> {
    if p.chars().count() < 8 {
        return Err(Error::BadRequest("a password needs at least 8 characters".into()));
    }
    if p.len() > 1024 {
        return Err(Error::BadRequest("that password is too long".into()));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        for bad in ["", ".", "..", "a/b", "a\\b", "a\0b", " a", "a ", "tab\tx", &"x".repeat(256)] {
            assert!(file_name(bad).is_err(), "{bad:?} should be rejected");
        }
        for good in ["a", "photo.jpg", "été.txt", "...", ".hidden", "a b", &"x".repeat(255)] {
            assert!(file_name(good).is_ok(), "{good:?} should pass");
        }
    }

    #[test]
    fn user_names() {
        for bad in ["", "-a", ".a", "a b", "a@b", "é", &"x".repeat(33)] {
            assert!(user_name(bad).is_err(), "{bad:?}");
        }
        for good in ["a", "alice", "a.b-c_d", "x1"] {
            assert!(user_name(good).is_ok(), "{good:?}");
        }
    }
}
