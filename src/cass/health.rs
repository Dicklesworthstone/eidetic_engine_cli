//! CASS health check parser (EE-102).
//!
//! Parses the output of `cass health --json` into typed Rust structs.
//! The health check is the first command `ee` runs to verify CASS is
//! available and the index is usable.

use super::error::CassError;
use super::process::CassOutcome;

/// Parsed result of `cass health --json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassHealth {
    /// Overall health status string (e.g., "healthy", "unhealthy").
    pub status: String,
    /// True if CASS considers itself healthy.
    pub healthy: bool,
    /// Error messages if unhealthy.
    pub errors: Vec<String>,
    /// Latency of the health check in milliseconds.
    pub latency_ms: u64,
    /// Database state summary.
    pub db: CassDbHealth,
    /// Index state summary.
    pub index: CassIndexHealth,
}

/// Database health from `cass health --json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassDbHealth {
    /// True if the database file exists.
    pub exists: bool,
    /// True if the database could be opened.
    pub opened: bool,
    /// Number of indexed conversations.
    pub conversations: u64,
    /// Number of indexed messages.
    pub messages: u64,
}

/// Index health from `cass health --json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassIndexHealth {
    /// True if the index exists.
    pub exists: bool,
    /// Index status string (e.g., "fresh", "stale", "empty").
    pub status: String,
    /// True if the index is fresh.
    pub fresh: bool,
    /// True if the index is stale.
    pub stale: bool,
    /// Number of documents in the index.
    pub documents: u64,
}

impl CassHealth {
    /// Parse health from a successful `cass health --json` outcome.
    ///
    /// # Errors
    ///
    /// Returns [`CassError::InvalidStdoutJson`] if the JSON is malformed
    /// or missing required fields.
    pub fn parse(outcome: &CassOutcome) -> Result<Self, CassError> {
        let stdout = outcome.stdout_utf8_lossy();
        Self::parse_json(&stdout)
    }

    /// Parse health from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`CassError::InvalidStdoutJson`] if the JSON is malformed.
    pub fn parse_json(json: &str) -> Result<Self, CassError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| CassError::InvalidStdoutJson {
                hint: format!("failed to parse cass health JSON: {e}"),
            })?;

        let status = get_string(&value, "status")?;
        let healthy = get_bool(&value, "healthy")?;
        let errors = get_string_array(&value, "errors")?;
        let latency_ms = get_u64(&value, "latency_ms")?;

        let db_value = value
            .get("db")
            .ok_or_else(|| CassError::InvalidStdoutJson {
                hint: "missing 'db' field".to_string(),
            })?;
        let db = CassDbHealth {
            exists: get_bool(db_value, "exists")?,
            opened: get_bool(db_value, "opened")?,
            conversations: get_u64(db_value, "conversations")?,
            messages: get_u64(db_value, "messages")?,
        };

        let state_value = value
            .get("state")
            .ok_or_else(|| CassError::InvalidStdoutJson {
                hint: "missing 'state' field".to_string(),
            })?;
        let index_value = state_value
            .get("index")
            .ok_or_else(|| CassError::InvalidStdoutJson {
                hint: "missing 'state.index' field".to_string(),
            })?;
        let index = CassIndexHealth {
            exists: get_bool(index_value, "exists")?,
            status: get_string(index_value, "status")?,
            fresh: get_bool(index_value, "fresh")?,
            stale: get_bool(index_value, "stale")?,
            documents: get_u64(index_value, "documents")?,
        };

        Ok(Self {
            status,
            healthy,
            errors,
            latency_ms,
            db,
            index,
        })
    }

    /// True if both DB and index are ready for queries.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.healthy && self.db.opened && self.index.fresh
    }

    /// True if the index exists but is stale.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.index.exists && self.index.stale
    }
}

fn get_string(value: &serde_json::Value, field: &str) -> Result<String, CassError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(String::from)
        .ok_or_else(|| CassError::InvalidStdoutJson {
            hint: format!("missing or invalid '{field}' string field"),
        })
}

fn get_bool(value: &serde_json::Value, field: &str) -> Result<bool, CassError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| CassError::InvalidStdoutJson {
            hint: format!("missing or invalid '{field}' bool field"),
        })
}

fn get_u64(value: &serde_json::Value, field: &str) -> Result<u64, CassError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| CassError::InvalidStdoutJson {
            hint: format!("missing or invalid '{field}' u64 field"),
        })
}

fn get_string_array(value: &serde_json::Value, field: &str) -> Result<Vec<String>, CassError> {
    let arr = value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| CassError::InvalidStdoutJson {
            hint: format!("missing or invalid '{field}' array field"),
        })?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(String::from)
                .ok_or_else(|| CassError::InvalidStdoutJson {
                    hint: format!("non-string element in '{field}' array"),
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::CassHealth;

    const HEALTH_FIXTURE: &str = include_str!("../../tests/fixtures/cass/health.v1.json");

    type TestResult = Result<(), String>;

    #[test]
    fn parse_health_fixture_succeeds() -> TestResult {
        let health = CassHealth::parse_json(HEALTH_FIXTURE)
            .map_err(|e| format!("failed to parse health fixture: {e}"))?;

        assert_eq!(health.status, "healthy");
        assert!(health.healthy);
        assert!(health.errors.is_empty());
        assert!(health.db.exists);
        assert!(health.db.opened);
        assert!(health.index.exists);
        assert!(health.index.fresh);
        Ok(())
    }

    #[test]
    fn is_ready_requires_healthy_and_fresh() {
        let healthy_fresh = CassHealth {
            status: "healthy".to_string(),
            healthy: true,
            errors: vec![],
            latency_ms: 10,
            db: super::CassDbHealth {
                exists: true,
                opened: true,
                conversations: 10,
                messages: 100,
            },
            index: super::CassIndexHealth {
                exists: true,
                status: "fresh".to_string(),
                fresh: true,
                stale: false,
                documents: 100,
            },
        };
        assert!(healthy_fresh.is_ready());

        let unhealthy = CassHealth {
            healthy: false,
            ..healthy_fresh.clone()
        };
        assert!(!unhealthy.is_ready());

        let stale = CassHealth {
            index: super::CassIndexHealth {
                fresh: false,
                stale: true,
                status: "stale".to_string(),
                ..healthy_fresh.index.clone()
            },
            ..healthy_fresh
        };
        assert!(!stale.is_ready());
    }

    #[test]
    fn is_stale_detects_stale_index() {
        let stale = CassHealth {
            status: "unhealthy".to_string(),
            healthy: false,
            errors: vec!["index stale".to_string()],
            latency_ms: 100,
            db: super::CassDbHealth {
                exists: true,
                opened: true,
                conversations: 10,
                messages: 100,
            },
            index: super::CassIndexHealth {
                exists: true,
                status: "stale".to_string(),
                fresh: false,
                stale: true,
                documents: 50,
            },
        };
        assert!(stale.is_stale());
    }

    #[test]
    fn parse_rejects_missing_fields() -> TestResult {
        let result = CassHealth::parse_json("{}");
        match result {
            Err(e) => {
                assert_eq!(e.kind_str(), "invalid_stdout_json");
                Ok(())
            }
            Ok(_) => Err("expected parse to fail on empty object".to_string()),
        }
    }
}
