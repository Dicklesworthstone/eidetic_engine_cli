//! Query filter types for ee.query.v1 structured queries.

use chrono::{DateTime, Utc};
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
    pub fn parse_name(s: &str) -> Option<Self> {
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
            let operator = FilterOperator::parse_name(operator_str)?;
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

const EQL_TOP_LEVEL_FIELDS: &[&str] = &[
    "q",
    "query",
    "workspace",
    "levels",
    "kinds",
    "tags",
    "tags_mode",
    "scope",
    "time",
    "confidence",
    "graph",
    "limit",
    "speed",
    "rerank",
    "return_subgraph",
    "explain",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EqlTagsMode {
    Any,
    All,
}

impl EqlTagsMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "any" => Some(Self::Any),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EqlSpeedMode {
    Instant,
    Default,
    Quality,
}

impl EqlSpeedMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Instant => "instant",
            Self::Default => "default",
            Self::Quality => "quality",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "instant" => Some(Self::Instant),
            "default" => Some(Self::Default),
            "quality" => Some(Self::Quality),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EqlTimeFilter {
    pub since: Option<String>,
    pub until: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EqlConfidenceFilter {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EqlGraphFilter {
    pub center: Option<String>,
    pub hops: Option<u32>,
    pub relations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EqlQuery {
    pub q: String,
    pub workspace: Option<String>,
    pub levels: Vec<String>,
    pub kinds: Vec<String>,
    pub tags: Vec<String>,
    pub tags_mode: EqlTagsMode,
    pub scope: Vec<String>,
    pub time: Option<EqlTimeFilter>,
    pub confidence: Option<EqlConfidenceFilter>,
    pub graph: Option<EqlGraphFilter>,
    pub limit: u32,
    pub speed: EqlSpeedMode,
    pub rerank: bool,
    pub return_subgraph: bool,
    pub explain: bool,
}

impl EqlQuery {
    #[must_use]
    pub fn metadata_filters(&self) -> QueryFilters {
        let mut filters = Vec::new();
        if let Some(workspace) = &self.workspace {
            filters.push(QueryFilter {
                field: "workspace".to_string(),
                predicates: vec![FilterPredicate {
                    operator: FilterOperator::Eq,
                    value: FilterValue::String(workspace.clone()),
                }],
            });
        }
        if !self.levels.is_empty() {
            filters.push(QueryFilter {
                field: "level".to_string(),
                predicates: vec![FilterPredicate {
                    operator: FilterOperator::In,
                    value: string_list_value(&self.levels),
                }],
            });
        }
        if !self.kinds.is_empty() {
            filters.push(QueryFilter {
                field: "kind".to_string(),
                predicates: vec![FilterPredicate {
                    operator: FilterOperator::In,
                    value: string_list_value(&self.kinds),
                }],
            });
        }
        if !self.scope.is_empty() {
            filters.push(QueryFilter {
                field: "scope".to_string(),
                predicates: vec![FilterPredicate {
                    operator: FilterOperator::In,
                    value: string_list_value(&self.scope),
                }],
            });
        }
        if let Some(confidence) = &self.confidence {
            let mut predicates = Vec::new();
            if let Some(min) = confidence.min {
                predicates.push(FilterPredicate {
                    operator: FilterOperator::Gte,
                    value: FilterValue::Number(min),
                });
            }
            if let Some(max) = confidence.max {
                predicates.push(FilterPredicate {
                    operator: FilterOperator::Lte,
                    value: FilterValue::Number(max),
                });
            }
            if !predicates.is_empty() {
                filters.push(QueryFilter {
                    field: "confidence".to_string(),
                    predicates,
                });
            }
        }
        QueryFilters { filters }
    }

    #[must_use]
    pub fn matches_metadata(&self, metadata: Option<&serde_json::Value>) -> bool {
        self.metadata_filters().matches(metadata)
            && self.matches_tags(metadata)
            && self.matches_time(metadata)
            && self.matches_graph(metadata)
    }

    #[must_use]
    pub fn execute_metadata<'a>(
        &self,
        candidates: impl IntoIterator<Item = &'a serde_json::Value>,
    ) -> Vec<&'a serde_json::Value> {
        candidates
            .into_iter()
            .filter(|candidate| self.matches_metadata(Some(candidate)))
            .take(usize::try_from(self.limit).unwrap_or(usize::MAX))
            .collect()
    }

    fn matches_tags(&self, metadata: Option<&serde_json::Value>) -> bool {
        if self.tags.is_empty() {
            return true;
        }
        let Some(metadata) = metadata else {
            return false;
        };
        let Some(tags) = metadata_string_values(metadata, "tags") else {
            return false;
        };
        match self.tags_mode {
            EqlTagsMode::Any => self
                .tags
                .iter()
                .any(|tag| tags.iter().any(|item| item == tag)),
            EqlTagsMode::All => self
                .tags
                .iter()
                .all(|tag| tags.iter().any(|item| item == tag)),
        }
    }

    fn matches_time(&self, metadata: Option<&serde_json::Value>) -> bool {
        let Some(time) = &self.time else {
            return true;
        };
        if time.since.is_none() && time.until.is_none() {
            return true;
        }
        let Some(metadata) = metadata else {
            return false;
        };
        if let Some(since) = &time.since
            && !matches_since(metadata, since)
        {
            return false;
        }
        if let Some(until) = &time.until
            && !matches_until(metadata, until)
        {
            return false;
        }
        true
    }

    fn matches_graph(&self, metadata: Option<&serde_json::Value>) -> bool {
        let Some(graph) = &self.graph else {
            return true;
        };
        if graph.center.is_none() && graph.hops.is_none() && graph.relations.is_empty() {
            return true;
        }
        let Some(metadata) = metadata.and_then(|value| value.get("graph")) else {
            return false;
        };
        if let Some(center) = &graph.center
            && metadata.get("center").and_then(serde_json::Value::as_str) != Some(center.as_str())
        {
            return false;
        }
        if let Some(hops) = graph.hops
            && metadata
                .get("hops")
                .and_then(serde_json::Value::as_u64)
                .is_none_or(|value| value > u64::from(hops))
        {
            return false;
        }
        if !graph.relations.is_empty() {
            let Some(relations) = metadata_string_values(metadata, "relations") else {
                return false;
            };
            if !graph
                .relations
                .iter()
                .any(|relation| relations.iter().any(|item| item == relation))
            {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EqlQueryError {
    pub field: String,
    pub message: String,
}

impl EqlQueryError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for EqlQueryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for EqlQueryError {}

pub fn parse_eql_query(value: &serde_json::Value) -> Result<EqlQuery, EqlQueryError> {
    let object = value
        .as_object()
        .ok_or_else(|| EqlQueryError::new("$", "EQL query must be a JSON object"))?;
    for key in object.keys() {
        if !EQL_TOP_LEVEL_FIELDS.contains(&key.as_str()) {
            return Err(EqlQueryError::new(
                key,
                "unknown EQL query field; expected the ee.query.v1 section 13.5 shape",
            ));
        }
    }
    let q = string_field(object, "q")
        .or_else(|| string_field(object, "query"))
        .ok_or_else(|| EqlQueryError::new("q", "query text is required"))?;
    if q.trim().is_empty() {
        return Err(EqlQueryError::new("q", "query text must not be empty"));
    }

    let workspace = optional_string_field(object, "workspace")?;
    let levels = string_array_field(object, "levels")?;
    let kinds = string_array_field(object, "kinds")?;
    let tags = string_array_field(object, "tags")?;
    let tags_mode = match optional_string_field(object, "tags_mode")? {
        Some(raw) => EqlTagsMode::parse(&raw).ok_or_else(|| {
            EqlQueryError::new("tags_mode", "expected `any` or `all` for tags_mode")
        })?,
        None => EqlTagsMode::Any,
    };
    let scope = string_array_field(object, "scope")?;
    let time = parse_eql_time(object.get("time"))?;
    let confidence = parse_eql_confidence(object.get("confidence"))?;
    let graph = parse_eql_graph(object.get("graph"))?;
    let limit = optional_u32_field(object, "limit")?.unwrap_or(10);
    if limit == 0 {
        return Err(EqlQueryError::new(
            "limit",
            "limit must be greater than zero",
        ));
    }
    let speed = match optional_string_field(object, "speed")? {
        Some(raw) => EqlSpeedMode::parse(&raw)
            .ok_or_else(|| EqlQueryError::new("speed", "expected instant, default, or quality"))?,
        None => EqlSpeedMode::Default,
    };

    Ok(EqlQuery {
        q: q.trim().to_string(),
        workspace,
        levels,
        kinds,
        tags,
        tags_mode,
        scope,
        time,
        confidence,
        graph,
        limit,
        speed,
        rerank: bool_field(object, "rerank")?.unwrap_or(false),
        return_subgraph: bool_field(object, "return_subgraph")?.unwrap_or(false),
        explain: bool_field(object, "explain")?.unwrap_or(false),
    })
}

fn string_list_value(values: &[String]) -> FilterValue {
    FilterValue::List(values.iter().cloned().map(FilterValue::String).collect())
}

fn string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Option<String> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn optional_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<String>, EqlQueryError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(|raw| Some(raw.trim().to_string()))
        .ok_or_else(|| EqlQueryError::new(field, "expected a string"))
}

fn string_array_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<String>, EqlQueryError> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| EqlQueryError::new(field, "expected an array of strings"))?;
    array
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(|raw| raw.trim().to_string())
                .filter(|raw| !raw.is_empty())
                .ok_or_else(|| {
                    EqlQueryError::new(format!("{field}[{index}]"), "expected a non-empty string")
                })
        })
        .collect()
}

fn bool_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<bool>, EqlQueryError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| EqlQueryError::new(field, "expected a boolean"))
}

fn optional_u32_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u32>, EqlQueryError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let raw = value
        .as_u64()
        .ok_or_else(|| EqlQueryError::new(field, "expected an unsigned integer"))?;
    u32::try_from(raw)
        .map(Some)
        .map_err(|_| EqlQueryError::new(field, "integer is too large"))
}

fn parse_eql_time(
    value: Option<&serde_json::Value>,
) -> Result<Option<EqlTimeFilter>, EqlQueryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| EqlQueryError::new("time", "expected an object"))?;
    for key in object.keys() {
        if !matches!(key.as_str(), "since" | "until") {
            return Err(EqlQueryError::new(
                nested_field_name("time", key),
                "expected since or until",
            ));
        }
    }
    Ok(Some(EqlTimeFilter {
        since: optional_string_field(object, "since")?,
        until: optional_string_field(object, "until")?,
    }))
}

fn parse_eql_confidence(
    value: Option<&serde_json::Value>,
) -> Result<Option<EqlConfidenceFilter>, EqlQueryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| EqlQueryError::new("confidence", "expected an object"))?;
    for key in object.keys() {
        if !matches!(key.as_str(), "min" | "max") {
            return Err(EqlQueryError::new(
                nested_field_name("confidence", key),
                "expected min or max",
            ));
        }
    }
    let min = optional_f64_field(object, "min")?;
    let max = optional_f64_field(object, "max")?;
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        return Err(EqlQueryError::new(
            "confidence",
            "min must be less than or equal to max",
        ));
    }
    Ok(Some(EqlConfidenceFilter { min, max }))
}

fn parse_eql_graph(
    value: Option<&serde_json::Value>,
) -> Result<Option<EqlGraphFilter>, EqlQueryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| EqlQueryError::new("graph", "expected an object"))?;
    for key in object.keys() {
        if !matches!(key.as_str(), "center" | "hops" | "relations") {
            return Err(EqlQueryError::new(
                nested_field_name("graph", key),
                "expected center, hops, or relations",
            ));
        }
    }
    Ok(Some(EqlGraphFilter {
        center: optional_string_field(object, "center")?,
        hops: optional_u32_field(object, "hops")?,
        relations: string_array_field(object, "relations")?,
    }))
}

fn optional_f64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<f64>, EqlQueryError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let number = value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| EqlQueryError::new(field, "expected a finite number"))?;
    Ok(Some(number))
}

fn nested_field_name(parent: &str, child: &str) -> String {
    let mut name = String::with_capacity(parent.len() + 1 + child.len());
    name.push_str(parent);
    name.push('.');
    name.push_str(child);
    name
}

fn metadata_string_values(value: &serde_json::Value, field: &str) -> Option<Vec<String>> {
    match value.get(field)? {
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::String(item) => Some(vec![item.clone()]),
        _ => None,
    }
}

fn metadata_created_at(metadata: &serde_json::Value) -> Option<&str> {
    metadata
        .get("createdAt")
        .or_else(|| metadata.get("created_at"))
        .and_then(serde_json::Value::as_str)
}

fn metadata_created_at_instant(metadata: &serde_json::Value) -> Option<DateTime<Utc>> {
    metadata_created_at(metadata).and_then(parse_rfc3339_instant)
}

fn matches_since(metadata: &serde_json::Value, since: &str) -> bool {
    if let Some(days) = parse_relative_days(since) {
        return metadata
            .get("ageDays")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|age_days| age_days <= days);
    }
    let Some(since) = parse_rfc3339_instant(since) else {
        return false;
    };
    metadata_created_at_instant(metadata).is_some_and(|created_at| created_at >= since)
}

fn matches_until(metadata: &serde_json::Value, until: &str) -> bool {
    let Some(until) = parse_rfc3339_instant(until) else {
        return false;
    };
    metadata_created_at_instant(metadata).is_some_and(|created_at| created_at <= until)
}

fn parse_relative_days(value: &str) -> Option<f64> {
    value
        .strip_suffix('d')
        .and_then(|days| days.parse::<f64>().ok())
        .filter(|days| days.is_finite() && *days >= 0.0)
}

fn parse_rfc3339_instant(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
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

    fn parse_test_filters(value: serde_json::Value) -> Result<QueryFilters, String> {
        parse_filters(&value).ok_or_else(|| "filters should parse".to_owned())
    }

    #[test]
    fn filter_eq_matches_string() -> TestResult {
        let filters = parse_test_filters(serde_json::json!({
            "level": {"eq": "procedural"}
        }))?;

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
        let filters = parse_test_filters(serde_json::json!({
            "confidence": {"gte": 0.8}
        }))?;

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
        let filters = parse_test_filters(serde_json::json!({
            "kind": {"in": ["rule", "pattern", "workflow"]}
        }))?;

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
        let filters = parse_test_filters(serde_json::json!({
            "validated_at": {"exists": true}
        }))?;

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
        let filters = parse_test_filters(serde_json::json!({
            "content": {"contains": "cargo"}
        }))?;

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
        let filters = parse_test_filters(serde_json::json!({
            "source.type": {"eq": "cass"}
        }))?;

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
        let filters = parse_test_filters(serde_json::json!({
            "level": {"eq": "procedural"},
            "confidence": {"gte": 0.7}
        }))?;

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

    #[test]
    fn eql_since_filter_compares_rfc3339_instants() -> TestResult {
        let metadata = serde_json::json!({
            "createdAt": "2026-05-04T09:00:00-04:00"
        });

        ensure(
            matches_since(&metadata, "2026-05-04T12:30:00Z"),
            "13:00Z should match since 12:30Z despite local hour sorting earlier",
        )?;

        ensure(
            !matches_since(&metadata, "2026-05-04T13:30:00Z"),
            "13:00Z should not match since 13:30Z",
        )
    }

    #[test]
    fn eql_until_filter_compares_rfc3339_instants() -> TestResult {
        let metadata = serde_json::json!({
            "created_at": "2026-05-04T13:30:00+02:00"
        });

        ensure(
            matches_until(&metadata, "2026-05-04T12:00:00Z"),
            "11:30Z should match until 12:00Z despite local hour sorting later",
        )?;

        ensure(
            !matches_until(&metadata, "2026-05-04T11:00:00Z"),
            "11:30Z should not match until 11:00Z",
        )
    }

    #[test]
    fn eql_absolute_time_filters_reject_invalid_timestamps() -> TestResult {
        let metadata = serde_json::json!({
            "createdAt": "2026-05-04T12:00:00Z"
        });
        ensure(
            !matches_since(&metadata, "not-a-timestamp"),
            "invalid since timestamp should not match",
        )?;

        let metadata = serde_json::json!({
            "createdAt": "not-a-timestamp"
        });
        ensure(
            !matches_until(&metadata, "2026-05-04T12:00:00Z"),
            "invalid metadata timestamp should not match",
        )
    }
}
