//! Query filter types for ee.query.v1 structured queries.

use serde::{Deserialize, Serialize};

/// A parsed filter from an ee.query.v1 document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryFilters {
    pub filters: Vec<QueryFilter>,
}

impl QueryFilters {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// Check if a metadata value matches all filters.
    #[must_use]
    pub fn matches(&self, metadata: Option<&serde_json::Value>) -> bool {
        self.filters.iter().all(|filter| filter.matches(metadata))
    }
}

/// A single field filter with one or more predicates.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryFilter {
    pub field: String,
    pub predicates: Vec<FilterPredicate>,
}

impl QueryFilter {
    #[must_use]
    pub fn matches(&self, metadata: Option<&serde_json::Value>) -> bool {
        let value = metadata.and_then(|m| get_nested_field(m, &self.field));
        self.predicates.iter().all(|pred| pred.matches(value))
    }
}

/// A predicate combining an operator with a value.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterPredicate {
    pub operator: FilterOperator,
    pub value: FilterValue,
}

impl FilterPredicate {
    #[must_use]
    pub fn matches(&self, actual: Option<&serde_json::Value>) -> bool {
        match self.operator {
            FilterOperator::Exists => {
                let should_exist = self.value.as_bool().unwrap_or(true);
                actual.is_some() == should_exist
            }
            FilterOperator::Eq => actual.is_some_and(|v| self.value.equals(v)),
            FilterOperator::Neq => !actual.is_some_and(|v| self.value.equals(v)),
            FilterOperator::In => actual.is_some_and(|v| self.value.contains(v)),
            FilterOperator::NotIn => !actual.is_some_and(|v| self.value.contains(v)),
            FilterOperator::Gt => actual.is_some_and(|v| self.value.compare_gt(v)),
            FilterOperator::Gte => actual.is_some_and(|v| self.value.compare_gte(v)),
            FilterOperator::Lt => actual.is_some_and(|v| self.value.compare_lt(v)),
            FilterOperator::Lte => actual.is_some_and(|v| self.value.compare_lte(v)),
            FilterOperator::StartsWith => actual.is_some_and(|v| self.value.starts_with(v)),
            FilterOperator::EndsWith => actual.is_some_and(|v| self.value.ends_with(v)),
            FilterOperator::Contains => actual.is_some_and(|v| self.value.contains_substring(v)),
        }
    }
}

/// Supported filter operators.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterOperator {
    Eq,
    Neq,
    In,
    NotIn,
    Gt,
    Gte,
    Lt,
    Lte,
    Exists,
    StartsWith,
    EndsWith,
    Contains,
}

impl FilterOperator {
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "eq" => Some(Self::Eq),
            "neq" => Some(Self::Neq),
            "in" => Some(Self::In),
            "notIn" => Some(Self::NotIn),
            "gt" => Some(Self::Gt),
            "gte" => Some(Self::Gte),
            "lt" => Some(Self::Lt),
            "lte" => Some(Self::Lte),
            "exists" => Some(Self::Exists),
            "startsWith" => Some(Self::StartsWith),
            "endsWith" => Some(Self::EndsWith),
            "contains" => Some(Self::Contains),
            _ => None,
        }
    }
}

/// A filter value that can be compared against JSON values.
#[derive(Clone, Debug, PartialEq)]
pub enum FilterValue {
    String(String),
    Number(f64),
    Bool(bool),
    List(Vec<FilterValue>),
    Null,
}

impl FilterValue {
    #[must_use]
    pub fn from_json(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::String(s) => Self::String(s.clone()),
            serde_json::Value::Number(n) => Self::Number(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Array(arr) => Self::List(arr.iter().map(Self::from_json).collect()),
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Object(_) => Self::Null,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    #[must_use]
    pub fn equals(&self, other: &serde_json::Value) -> bool {
        match (self, other) {
            (Self::String(s), serde_json::Value::String(o)) => s == o,
            (Self::Number(n), serde_json::Value::Number(o)) => {
                o.as_f64().is_some_and(|o| (*n - o).abs() < f64::EPSILON)
            }
            (Self::Bool(b), serde_json::Value::Bool(o)) => b == o,
            (Self::Null, serde_json::Value::Null) => true,
            _ => false,
        }
    }

    #[must_use]
    pub fn contains(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::List(items) => items.iter().any(|item| item.equals(value)),
            _ => false,
        }
    }

    #[must_use]
    pub fn compare_gt(&self, actual: &serde_json::Value) -> bool {
        match (self, actual) {
            (Self::Number(threshold), serde_json::Value::Number(n)) => {
                n.as_f64().is_some_and(|n| n > *threshold)
            }
            (Self::String(threshold), serde_json::Value::String(s)) => {
                s.as_str() > threshold.as_str()
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn compare_gte(&self, actual: &serde_json::Value) -> bool {
        match (self, actual) {
            (Self::Number(threshold), serde_json::Value::Number(n)) => {
                n.as_f64().is_some_and(|n| n >= *threshold)
            }
            (Self::String(threshold), serde_json::Value::String(s)) => {
                s.as_str() >= threshold.as_str()
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn compare_lt(&self, actual: &serde_json::Value) -> bool {
        match (self, actual) {
            (Self::Number(threshold), serde_json::Value::Number(n)) => {
                n.as_f64().is_some_and(|n| n < *threshold)
            }
            (Self::String(threshold), serde_json::Value::String(s)) => {
                s.as_str() < threshold.as_str()
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn compare_lte(&self, actual: &serde_json::Value) -> bool {
        match (self, actual) {
            (Self::Number(threshold), serde_json::Value::Number(n)) => {
                n.as_f64().is_some_and(|n| n <= *threshold)
            }
            (Self::String(threshold), serde_json::Value::String(s)) => {
                s.as_str() <= threshold.as_str()
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn starts_with(&self, actual: &serde_json::Value) -> bool {
        match (self, actual) {
            (Self::String(prefix), serde_json::Value::String(s)) => s.starts_with(prefix),
            _ => false,
        }
    }

    #[must_use]
    pub fn ends_with(&self, actual: &serde_json::Value) -> bool {
        match (self, actual) {
            (Self::String(suffix), serde_json::Value::String(s)) => s.ends_with(suffix),
            _ => false,
        }
    }

    #[must_use]
    pub fn contains_substring(&self, actual: &serde_json::Value) -> bool {
        match (self, actual) {
            (Self::String(needle), serde_json::Value::String(s)) => s.contains(needle),
            _ => false,
        }
    }
}

fn get_nested_field<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Parse filters from an ee.query.v1 filters object.
pub fn parse_filters(value: &serde_json::Value) -> Option<QueryFilters> {
    let object = value.as_object()?;
    let mut filters = Vec::new();

    for (field, predicates) in object {
        let pred_object = predicates.as_object()?;
        let mut filter_predicates = Vec::new();

        for (operator_str, value) in pred_object {
            let operator = FilterOperator::from_str(operator_str)?;
            let filter_value = FilterValue::from_json(value);
            filter_predicates.push(FilterPredicate {
                operator,
                value: filter_value,
            });
        }

        filters.push(QueryFilter {
            field: field.clone(),
            predicates: filter_predicates,
        });
    }

    Some(QueryFilters { filters })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.to_owned())
        }
    }

    #[test]
    fn filter_eq_matches_string() -> TestResult {
        let filters = parse_filters(&serde_json::json!({
            "level": {"eq": "procedural"}
        }))
        .unwrap();

        let metadata = serde_json::json!({"level": "procedural"});
        ensure(
            filters.matches(Some(&metadata)),
            "should match equal string",
        )?;

        let metadata = serde_json::json!({"level": "episodic"});
        ensure(
            !filters.matches(Some(&metadata)),
            "should not match different string",
        )
    }

    #[test]
    fn filter_gte_matches_number() -> TestResult {
        let filters = parse_filters(&serde_json::json!({
            "confidence": {"gte": 0.8}
        }))
        .unwrap();

        let metadata = serde_json::json!({"confidence": 0.9});
        ensure(filters.matches(Some(&metadata)), "0.9 >= 0.8 should match")?;

        let metadata = serde_json::json!({"confidence": 0.8});
        ensure(filters.matches(Some(&metadata)), "0.8 >= 0.8 should match")?;

        let metadata = serde_json::json!({"confidence": 0.5});
        ensure(
            !filters.matches(Some(&metadata)),
            "0.5 >= 0.8 should not match",
        )
    }

    #[test]
    fn filter_in_matches_list() -> TestResult {
        let filters = parse_filters(&serde_json::json!({
            "kind": {"in": ["rule", "pattern", "workflow"]}
        }))
        .unwrap();

        let metadata = serde_json::json!({"kind": "rule"});
        ensure(filters.matches(Some(&metadata)), "rule should be in list")?;

        let metadata = serde_json::json!({"kind": "episode"});
        ensure(
            !filters.matches(Some(&metadata)),
            "episode should not be in list",
        )
    }

    #[test]
    fn filter_exists_checks_presence() -> TestResult {
        let filters = parse_filters(&serde_json::json!({
            "validated_at": {"exists": true}
        }))
        .unwrap();

        let metadata = serde_json::json!({"validated_at": "2026-05-04T12:00:00Z"});
        ensure(
            filters.matches(Some(&metadata)),
            "field exists should match",
        )?;

        let metadata = serde_json::json!({"level": "procedural"});
        ensure(
            !filters.matches(Some(&metadata)),
            "missing field should not match",
        )
    }

    #[test]
    fn filter_contains_matches_substring() -> TestResult {
        let filters = parse_filters(&serde_json::json!({
            "content": {"contains": "cargo"}
        }))
        .unwrap();

        let metadata = serde_json::json!({"content": "Run cargo build first"});
        ensure(filters.matches(Some(&metadata)), "substring should match")?;

        let metadata = serde_json::json!({"content": "Run npm install first"});
        ensure(
            !filters.matches(Some(&metadata)),
            "no substring should not match",
        )
    }

    #[test]
    fn filter_nested_field_access() -> TestResult {
        let filters = parse_filters(&serde_json::json!({
            "source.type": {"eq": "cass"}
        }))
        .unwrap();

        let metadata = serde_json::json!({"source": {"type": "cass", "id": "123"}});
        ensure(
            filters.matches(Some(&metadata)),
            "nested field should match",
        )?;

        let metadata = serde_json::json!({"source": {"type": "manual"}});
        ensure(
            !filters.matches(Some(&metadata)),
            "different nested value should not match",
        )
    }

    #[test]
    fn multiple_filters_all_must_match() -> TestResult {
        let filters = parse_filters(&serde_json::json!({
            "level": {"eq": "procedural"},
            "confidence": {"gte": 0.7}
        }))
        .unwrap();

        let metadata = serde_json::json!({"level": "procedural", "confidence": 0.8});
        ensure(
            filters.matches(Some(&metadata)),
            "both conditions met should match",
        )?;

        let metadata = serde_json::json!({"level": "procedural", "confidence": 0.5});
        ensure(
            !filters.matches(Some(&metadata)),
            "confidence too low should not match",
        )?;

        let metadata = serde_json::json!({"level": "episodic", "confidence": 0.9});
        ensure(
            !filters.matches(Some(&metadata)),
            "wrong level should not match",
        )
    }

    #[test]
    fn empty_filters_match_everything() -> TestResult {
        let filters = QueryFilters::default();
        ensure(filters.is_empty(), "default should be empty")?;

        let metadata = serde_json::json!({"anything": "here"});
        ensure(
            filters.matches(Some(&metadata)),
            "empty filters match everything",
        )?;
        ensure(filters.matches(None), "empty filters match None metadata")
    }
}
