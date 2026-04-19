#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_error_displays_message() {
        let err = DomainError::NotFound("account 123".to_string());
        assert_eq!(err.to_string(), "not found: account 123");
    }

    #[test]
    fn unauthorized_error_displays() {
        let err = DomainError::Unauthorized;
        assert_eq!(err.to_string(), "unauthorized");
    }
}
