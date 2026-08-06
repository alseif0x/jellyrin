use std::fmt;
use time::OffsetDateTime;

use crate::MagstvProviderError;

/// Authenticated app session. Tokens remain runtime-only and are redacted by
/// `Debug`; the optional client IP records the egress binding observed during
/// the authorised session.
#[derive(Clone, PartialEq, Eq)]
pub struct MagstvSession {
    token: String,
    issued_at: OffsetDateTime,
    expires_at: Option<OffsetDateTime>,
    bound_client_ip: Option<String>,
}

impl fmt::Debug for MagstvSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvSession")
            .field("token", &"[REDACTED]")
            .field("issued_at", &self.issued_at)
            .field("expires_at", &self.expires_at)
            .field("bound_client_ip", &self.bound_client_ip)
            .finish()
    }
}

impl MagstvSession {
    pub fn new(token: impl Into<String>, issued_at: OffsetDateTime) -> Self {
        Self {
            token: token.into(),
            issued_at,
            expires_at: None,
            bound_client_ip: None,
        }
    }

    pub fn with_expires_at(mut self, expires_at: OffsetDateTime) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn with_bound_client_ip(mut self, client_ip: impl Into<String>) -> Self {
        self.bound_client_ip = Some(client_ip.into());
        self
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn issued_at(&self) -> OffsetDateTime {
        self.issued_at
    }

    pub fn expires_at(&self) -> Option<OffsetDateTime> {
        self.expires_at
    }

    pub fn bound_client_ip(&self) -> Option<&str> {
        self.bound_client_ip.as_deref()
    }

    pub fn is_valid_at(&self, now: OffsetDateTime) -> bool {
        !self.token.trim().is_empty()
            && now >= self.issued_at
            && self.expires_at.is_none_or(|expires_at| now < expires_at)
    }

    pub fn validate_at(&self, now: OffsetDateTime) -> Result<(), MagstvProviderError> {
        if self.is_valid_at(now) {
            Ok(())
        } else {
            Err(MagstvProviderError::SessionExpired)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(seconds).unwrap()
    }

    #[test]
    fn session_is_valid_only_inside_its_window() {
        let session =
            MagstvSession::new("session-token", instant(100)).with_expires_at(instant(200));
        assert!(!session.is_valid_at(instant(99)));
        assert!(session.is_valid_at(instant(100)));
        assert!(session.is_valid_at(instant(199)));
        assert!(!session.is_valid_at(instant(200)));
    }

    #[test]
    fn debug_redacts_the_token() {
        let session = MagstvSession::new("secret-session-token", instant(100));
        let debug = format!("{session:?}");
        assert!(!debug.contains("secret-session-token"));
        assert!(debug.contains("REDACTED"));
    }
}
