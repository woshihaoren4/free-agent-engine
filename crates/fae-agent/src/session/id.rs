use std::{fmt, str::FromStr};

use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// Limit identifiers at the DTO boundary, independently of storage path validation.
pub const MAX_ID_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DataValidationError {
    #[error("{kind} must not be empty or contain surrounding whitespace")]
    EmptyOrPaddedId { kind: &'static str },
    #[error("{kind} exceeds {MAX_ID_BYTES} bytes")]
    IdTooLong { kind: &'static str },
    #[error("{kind} must not contain control characters")]
    InvalidId { kind: &'static str },
    #[error("timestamp must be a valid RFC3339 date and time")]
    InvalidTimestamp,
}

fn validate_id(value: &str, kind: &'static str) -> Result<(), DataValidationError> {
    if value.is_empty() || value.trim() != value {
        return Err(DataValidationError::EmptyOrPaddedId { kind });
    }
    if value.len() > MAX_ID_BYTES {
        return Err(DataValidationError::IdTooLong { kind });
    }
    if value.chars().any(char::is_control) {
        return Err(DataValidationError::InvalidId { kind });
    }
    Ok(())
}

macro_rules! identifier {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DataValidationError> {
                let value = value.into();
                validate_id(&value, stringify!($name))?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = DataValidationError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl FromStr for $name {
            type Err = DataValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    )+};
}

identifier!(
    UserId,
    SessionId,
    ExecutionId,
    InvocationId,
    PlanInstanceId,
    TaskId,
    RequestId,
);

/// A validated timestamp, encoded as an RFC3339 string normalized to UTC.
/// This does not change the existing UserMemory.updated_at storage format.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Timestamp(String);

impl Timestamp {
    pub fn new(value: impl AsRef<str>) -> Result<Self, DataValidationError> {
        let date = DateTime::parse_from_rfc3339(value.as_ref())
            .map_err(|_| DataValidationError::InvalidTimestamp)?;
        Self::from_utc(date.with_timezone(&Utc))
    }

    pub fn from_utc(date: DateTime<Utc>) -> Result<Self, DataValidationError> {
        // Normalizing an offset near a year boundary can leave RFC3339's four-digit range.
        if !(0..=9999).contains(&date.year()) {
            return Err(DataValidationError::InvalidTimestamp);
        }
        Ok(Self(date.to_rfc3339_opts(SecondsFormat::AutoSi, true)))
    }

    pub fn now() -> Self {
        Self::from_utc(Utc::now()).expect("current UTC time must fit RFC3339")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Timestamp {
    type Err = DataValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
