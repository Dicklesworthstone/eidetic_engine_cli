//! Query filter types for ee.query.v1 structured queries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A parsed filter from an ee.query.v1 document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryFilters {
    pub filters: Vec<QueryFilter>,
    pub tags: TagFilters,
    pub temporal: QueryTemporalFilters,
    pub trust: TrustFilters,
    pub redaction: RedactionFilters,
    pub graph: QueryGraphHints,
}

impl QueryFilters {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
            && self.tags.is_empty()
            && self.temporal.is_empty()
            && self.trust.is_empty()
            && self.redaction.is_empty()
            && self.graph.is_empty()
    }

    /// Check if a metadata value matches all filters.
    #[must_use]
    pub fn matches(&self, metadata: Option<&serde_json::Value>) -> bool {
        self.filters.iter().all(|filter| filter.matches(metadata))
    }

    /// Check if a memory's tags match the tag filters.
    #[must_use]
    pub fn matches_tags(&self, memory_tags: &[String]) -> bool {
        self.tags.matches(memory_tags)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryTemporalFilters {
    pub after: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub as_of: Option<DateTime<Utc>>,
    pub validity: Option<QueryTemporalValidity>,
}

impl QueryTemporalFilters {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.after.is_none()
            && self.before.is_none()
            && self.as_of.is_none()
            && self.validity.is_none()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryTemporalValidity {
    pub posture: QueryTemporalValidityPosture,
    pub reference_time: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueryTemporalValidityPosture {
    Strict,
    #[default]
    Relaxed,
    Ignore,
}

/// Graph traversal hints from an ee.query.v1 graph object.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryGraphHints {
    pub enabled: bool,
    pub seed_memories: Vec<String>,
    pub traversal: QueryGraphTraversal,
    pub max_hops: u32,
    pub link_types: Vec<String>,
    pub include_orphans: bool,
}

impl Default for QueryGraphHints {
    fn default() -> Self {
        Self {
            enabled: false,
            seed_memories: Vec::new(),
            traversal: QueryGraphTraversal::Bidirectional,
            max_hops: 1,
            link_types: Vec::new(),
            include_orphans: true,
        }
    }
}

impl QueryGraphHints {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.enabled
    }
}

/// Direction for ee.query.v1 graph expansion hints.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueryGraphTraversal {
    Outbound,
    Inbound,
    #[default]
    Bidirectional,
}

impl QueryGraphTraversal {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outbound => "outbound",
            Self::Inbound => "inbound",
            Self::Bidirectional => "bidirectional",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "outbound" => Some(Self::Outbound),
            "inbound" => Some(Self::Inbound),
            "bidirectional" => Some(Self::Bidirectional),
            _ => None,
        }
    }
}

impl QueryTemporalValidityPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Relaxed => "relaxed",
            Self::Ignore => "ignore",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "strict" => Some(Self::Strict),
            "relaxed" => Some(Self::Relaxed),
            "ignore" => Some(Self::Ignore),
            _ => None,
        }
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
            FilterOperator::Exists => self
                .value
                .as_bool()
                .is_some_and(|should_exist| actual.is_some() == should_exist),
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

    #[must_use]
    pub fn accepts_json_value(self, value: &serde_json::Value) -> bool {
        match self {
            Self::Eq | Self::Neq => is_scalar_filter_value(value),
            Self::In | Self::NotIn => value
                .as_array()
                .is_some_and(|items| items.iter().all(is_scalar_filter_value)),
            Self::Gt | Self::Gte | Self::Lt | Self::Lte => value.is_number() || value.is_string(),
            Self::Exists => value.is_boolean(),
            Self::StartsWith | Self::EndsWith | Self::Contains => value.is_string(),
        }
    }

    #[must_use]
    pub const fn expected_json_value(self) -> &'static str {
        match self {
            Self::Eq | Self::Neq => "a scalar value",
            Self::In | Self::NotIn => "a list of scalar values",
            Self::Gt | Self::Gte | Self::Lt | Self::Lte => "a number or string",
            Self::Exists => "a boolean",
            Self::StartsWith | Self::EndsWith | Self::Contains => "a string",
        }
    }
}

fn is_scalar_filter_value(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::String(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Bool(_)
            | serde_json::Value::Null
    )
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
            if !operator.accepts_json_value(value) {
                return None;
            }
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

    Some(QueryFilters {
        filters,
        tags: TagFilters::default(),
        temporal: QueryTemporalFilters::default(),
        trust: TrustFilters::default(),
        redaction: RedactionFilters::default(),
        graph: QueryGraphHints::default(),
    })
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
        QueryFilters {
            filters,
            tags: TagFilters::default(),
            temporal: QueryTemporalFilters::default(),
            trust: TrustFilters::default(),
            redaction: RedactionFilters::default(),
            graph: QueryGraphHints::default(),
        }
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
    validate_unit_confidence_bound("min", min)?;
    validate_unit_confidence_bound("max", max)?;
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

fn validate_unit_confidence_bound(field: &str, value: Option<f64>) -> Result<(), EqlQueryError> {
    if let Some(value) = value
        && !(0.0..=1.0).contains(&value)
    {
        return Err(EqlQueryError::new(
            nested_field_name("confidence", field),
            "expected a unit score between 0.0 and 1.0",
        ));
    }
    Ok(())
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

/// Tag filters from ee.query.v1 tags object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TagFilters {
    /// All tags must be present (AND).
    pub require: Vec<String>,
    /// At least one tag must be present (OR).
    pub require_any: Vec<String>,
    /// None of these tags may be present.
    pub exclude: Vec<String>,
}

impl TagFilters {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.require.is_empty() && self.require_any.is_empty() && self.exclude.is_empty()
    }

    /// Check if the given memory tags satisfy all tag filter constraints.
    #[must_use]
    pub fn matches(&self, memory_tags: &[String]) -> bool {
        if !self.require.is_empty() && !self.require.iter().all(|req| memory_tags.contains(req)) {
            return false;
        }
        if !self.require_any.is_empty()
            && !self.require_any.iter().any(|req| memory_tags.contains(req))
        {
            return false;
        }
        if self.exclude.iter().any(|excl| memory_tags.contains(excl)) {
            return false;
        }
        true
    }
}

/// Parse tag filters from an ee.query.v1 tags object.
///
/// The tags object may contain:
/// - `require`: string[] - all tags must be present (AND)
/// - `requireAny`: string[] - at least one tag must be present (OR)
/// - `exclude`: string[] - none of these tags may be present
#[must_use]
pub fn parse_tags(value: &serde_json::Value) -> TagFilters {
    let Some(object) = value.as_object() else {
        return TagFilters::default();
    };

    let require = object
        .get("require")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let require_any = object
        .get("requireAny")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let exclude = object
        .get("exclude")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    TagFilters {
        require,
        require_any,
        exclude,
    }
}

/// Trust filters from ee.query.v1 trust object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrustFilters {
    /// Minimum trust class to include (memories with lower trust are excluded).
    pub min_class: Option<String>,
    /// Trust classes to exclude entirely.
    pub exclude_classes: Vec<String>,
    /// Required trust posture (authoritative, provisional, untrusted).
    pub require_posture: Option<String>,
}

impl TrustFilters {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.min_class.is_none()
            && self.exclude_classes.is_empty()
            && self.require_posture.is_none()
    }

    /// Check if the given trust class satisfies all trust filter constraints.
    #[must_use]
    pub fn matches(&self, trust_class: &str, trust_posture: &str) -> bool {
        if let Some(ref min_class) = self.min_class {
            if !trust_class_meets_minimum(trust_class, min_class) {
                return false;
            }
        }
        if self.exclude_classes.iter().any(|excl| excl == trust_class) {
            return false;
        }
        if let Some(ref required) = self.require_posture {
            if trust_posture != required {
                return false;
            }
        }
        true
    }
}

/// Check if a trust class meets the minimum threshold.
fn trust_class_meets_minimum(actual: &str, min: &str) -> bool {
    let order = [
        "human_explicit",
        "agent_validated",
        "agent_assertion",
        "cass_evidence",
        "legacy_import",
    ];
    let actual_rank = order
        .iter()
        .position(|&c| c == actual)
        .unwrap_or(order.len());
    let min_rank = order.iter().position(|&c| c == min).unwrap_or(0);
    actual_rank <= min_rank
}

/// Derive trust posture from trust class string (EE-260, ADR-0009).
#[must_use]
pub fn posture_for_trust_class(trust_class: &str) -> &'static str {
    match trust_class {
        "human_explicit" | "agent_validated" => "authoritative",
        "agent_assertion" | "cass_evidence" => "advisory",
        "legacy_import" => "legacy_evidence",
        _ => "advisory",
    }
}

/// Parse trust filters from an ee.query.v1 trust object.
#[must_use]
pub fn parse_trust(value: &serde_json::Value) -> TrustFilters {
    let Some(object) = value.as_object() else {
        return TrustFilters::default();
    };

    let min_class = object
        .get("minClass")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    let exclude_classes = object
        .get("excludeClasses")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let require_posture = object
        .get("requirePosture")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    TrustFilters {
        min_class,
        exclude_classes,
        require_posture,
    }
}

/// Redaction filters from ee.query.v1 redaction object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RedactionFilters {
    /// Redaction policy: "respect" (apply redaction) or "bypass" (requires elevated permission).
    pub policy: Option<String>,
    /// Redaction categories that are acceptable to include.
    pub allow_categories: Vec<String>,
}

impl RedactionFilters {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.policy.is_none() && self.allow_categories.is_empty()
    }

    /// Check if bypass is requested (requires elevated permission).
    #[must_use]
    pub fn requests_bypass(&self) -> bool {
        self.policy.as_deref() == Some("bypass")
    }
}

/// Parse redaction filters from an ee.query.v1 redaction object.
#[must_use]
pub fn parse_redaction(value: &serde_json::Value) -> RedactionFilters {
    let Some(object) = value.as_object() else {
        return RedactionFilters::default();
    };

    let policy = object
        .get("policy")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    let allow_categories = object
        .get("allowCategories")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    RedactionFilters {
        policy,
        allow_categories,
    }
}

/// Pagination controls from ee.query.v1 pagination object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryPagination {
    /// Page size limit.
    pub limit: Option<u32>,
    /// Opaque cursor from previous response.
    pub cursor: Option<String>,
}

impl QueryPagination {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.limit.is_none() && self.cursor.is_none()
    }

    /// Effective page size (default 50, max 200).
    #[must_use]
    pub fn effective_limit(&self) -> u32 {
        self.limit.unwrap_or(50).min(200)
    }
}

/// Decoded cursor state for pagination.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaginationCursor {
    /// Offset into the result set.
    pub offset: u32,
    /// Hash of query shape for stale cursor detection.
    pub query_hash: String,
}

impl PaginationCursor {
    /// Encode cursor to opaque base64 token.
    #[must_use]
    pub fn encode(&self) -> String {
        let json = serde_json::json!({
            "o": self.offset,
            "h": self.query_hash,
        });
        base64_encode(json.to_string().as_bytes())
    }

    /// Decode cursor from opaque base64 token.
    pub fn decode(token: &str) -> Result<Self, PaginationCursorError> {
        let bytes = base64_decode(token).map_err(|_| PaginationCursorError::MalformedBase64)?;
        let text = String::from_utf8(bytes).map_err(|_| PaginationCursorError::MalformedBase64)?;
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| PaginationCursorError::MalformedJson)?;
        let obj = value
            .as_object()
            .ok_or(PaginationCursorError::MalformedJson)?;
        let offset = obj
            .get("o")
            .and_then(|v| v.as_u64())
            .ok_or(PaginationCursorError::MissingOffset)?;
        let query_hash = obj
            .get("h")
            .and_then(|v| v.as_str())
            .ok_or(PaginationCursorError::MissingQueryHash)?
            .to_string();
        Ok(Self {
            offset: u32::try_from(offset).unwrap_or(u32::MAX),
            query_hash,
        })
    }
}

/// Error decoding a pagination cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaginationCursorError {
    MalformedBase64,
    MalformedJson,
    MissingOffset,
    MissingQueryHash,
    QueryShapeMismatch,
}

impl std::fmt::Display for PaginationCursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedBase64 => f.write_str("cursor is not valid base64"),
            Self::MalformedJson => f.write_str("cursor does not contain valid JSON"),
            Self::MissingOffset => f.write_str("cursor missing offset field"),
            Self::MissingQueryHash => f.write_str("cursor missing query hash field"),
            Self::QueryShapeMismatch => {
                f.write_str("cursor was generated for a different query shape")
            }
        }
    }
}

impl std::error::Error for PaginationCursorError {}

/// Compute deterministic hash of query shape for cursor scoping.
#[must_use]
pub fn compute_query_shape_hash(query: &str, filters: &QueryFilters) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(query.as_bytes());
    hasher.update(format!("{:?}", filters).as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Parse pagination from an ee.query.v1 pagination object.
#[must_use]
pub fn parse_pagination(value: &serde_json::Value) -> QueryPagination {
    let Some(object) = value.as_object() else {
        return QueryPagination::default();
    };

    let limit = object
        .get("limit")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());

    let cursor = object
        .get("cursor")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    QueryPagination { limit, cursor }
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(input)
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

    fn parse_test_confidence(value: serde_json::Value) -> Result<EqlConfidenceFilter, String> {
        parse_eql_confidence(Some(&value))
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "confidence filter should parse".to_owned())
    }

    fn parse_test_confidence_error(value: serde_json::Value) -> Result<EqlQueryError, String> {
        match parse_eql_confidence(Some(&value)) {
            Ok(_) => Err("confidence filter should reject invalid input".to_owned()),
            Err(err) => Ok(err),
        }
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
    fn filter_exists_false_checks_absence() -> TestResult {
        let filters = parse_test_filters(serde_json::json!({
            "validated_at": {"exists": false}
        }))?;

        let metadata = serde_json::json!({"level": "procedural"});
        ensure(
            filters.matches(Some(&metadata)),
            "missing field should match exists false",
        )?;

        let metadata = serde_json::json!({"validated_at": "2026-05-04T12:00:00Z"});
        ensure(
            !filters.matches(Some(&metadata)),
            "present field should not match exists false",
        )
    }

    #[test]
    fn filter_parser_rejects_mistyped_operator_values() -> TestResult {
        ensure(
            parse_filters(&serde_json::json!({
                "validated_at": {"exists": "false"}
            }))
            .is_none(),
            "exists must be a typed boolean",
        )?;
        ensure(
            parse_filters(&serde_json::json!({
                "kind": {"in": "rule"}
            }))
            .is_none(),
            "in must be a list",
        )?;
        ensure(
            parse_filters(&serde_json::json!({
                "confidence": {"gte": {"min": 0.8}}
            }))
            .is_none(),
            "comparison operators must be scalar comparable values",
        )?;
        ensure(
            parse_filters(&serde_json::json!({
                "content": {"contains": false}
            }))
            .is_none(),
            "string operators must receive strings",
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
    fn eql_confidence_filter_accepts_unit_bounds() -> TestResult {
        let confidence = parse_test_confidence(serde_json::json!({
            "min": 0.0,
            "max": 1.0
        }))?;

        ensure(
            confidence
                .min
                .is_some_and(|value| (value - 0.0).abs() < f64::EPSILON),
            "min should accept lower unit-score bound",
        )?;
        ensure(
            confidence
                .max
                .is_some_and(|value| (value - 1.0).abs() < f64::EPSILON),
            "max should accept upper unit-score bound",
        )
    }

    #[test]
    fn eql_confidence_filter_rejects_out_of_unit_bounds() -> TestResult {
        let err = parse_test_confidence_error(serde_json::json!({
            "min": -0.01
        }))?;
        ensure(
            err.field == "confidence.min",
            "negative min should identify confidence.min",
        )?;
        ensure(
            err.message.contains("unit score"),
            "negative min should report unit-score domain",
        )?;

        let err = parse_test_confidence_error(serde_json::json!({
            "max": 1.01
        }))?;
        ensure(
            err.field == "confidence.max",
            "greater-than-one max should identify confidence.max",
        )?;
        ensure(
            err.message.contains("unit score"),
            "greater-than-one max should report unit-score domain",
        )
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

    #[test]
    fn trust_filters_empty_allows_all() -> TestResult {
        let filters = TrustFilters::default();
        ensure(filters.is_empty(), "default TrustFilters should be empty")?;
        ensure(
            filters.matches("human_explicit", "authoritative"),
            "empty filter should allow human_explicit",
        )?;
        ensure(
            filters.matches("legacy_import", "legacy_evidence"),
            "empty filter should allow legacy_import",
        )
    }

    #[test]
    fn trust_filters_min_class_enforced() -> TestResult {
        let filters = TrustFilters {
            min_class: Some("agent_validated".to_string()),
            exclude_classes: vec![],
            require_posture: None,
        };
        ensure(
            filters.matches("human_explicit", "authoritative"),
            "human_explicit should pass min_class=agent_validated",
        )?;
        ensure(
            filters.matches("agent_validated", "authoritative"),
            "agent_validated should pass min_class=agent_validated",
        )?;
        ensure(
            !filters.matches("agent_assertion", "advisory"),
            "agent_assertion should fail min_class=agent_validated",
        )?;
        ensure(
            !filters.matches("legacy_import", "legacy_evidence"),
            "legacy_import should fail min_class=agent_validated",
        )
    }

    #[test]
    fn trust_filters_exclude_classes_enforced() -> TestResult {
        let filters = TrustFilters {
            min_class: None,
            exclude_classes: vec!["legacy_import".to_string(), "cass_evidence".to_string()],
            require_posture: None,
        };
        ensure(
            filters.matches("human_explicit", "authoritative"),
            "human_explicit should pass exclusion filter",
        )?;
        ensure(
            !filters.matches("legacy_import", "legacy_evidence"),
            "legacy_import should be excluded",
        )?;
        ensure(
            !filters.matches("cass_evidence", "advisory"),
            "cass_evidence should be excluded",
        )
    }

    #[test]
    fn trust_filters_require_posture_enforced() -> TestResult {
        let filters = TrustFilters {
            min_class: None,
            exclude_classes: vec![],
            require_posture: Some("authoritative".to_string()),
        };
        ensure(
            filters.matches("human_explicit", "authoritative"),
            "authoritative posture should pass",
        )?;
        ensure(
            !filters.matches("agent_assertion", "advisory"),
            "advisory posture should fail require_posture=authoritative",
        )
    }

    #[test]
    fn parse_trust_from_json() -> TestResult {
        let json = serde_json::json!({
            "minClass": "agent_validated",
            "excludeClasses": ["legacy_import"],
            "requirePosture": "authoritative"
        });
        let filters = parse_trust(&json);
        ensure(
            filters.min_class.as_deref() == Some("agent_validated"),
            "should parse minClass",
        )?;
        ensure(
            filters.exclude_classes == vec!["legacy_import"],
            "should parse excludeClasses",
        )?;
        ensure(
            filters.require_posture.as_deref() == Some("authoritative"),
            "should parse requirePosture",
        )
    }

    #[test]
    fn redaction_filters_empty_is_default() -> TestResult {
        let filters = RedactionFilters::default();
        ensure(
            filters.is_empty(),
            "default RedactionFilters should be empty",
        )?;
        ensure(
            !filters.requests_bypass(),
            "default should not request bypass",
        )
    }

    #[test]
    fn redaction_filters_bypass_detected() -> TestResult {
        let filters = RedactionFilters {
            policy: Some("bypass".to_string()),
            allow_categories: vec![],
        };
        ensure(
            filters.requests_bypass(),
            "policy=bypass should be detected",
        )?;

        let respect_filters = RedactionFilters {
            policy: Some("respect".to_string()),
            allow_categories: vec![],
        };
        ensure(
            !respect_filters.requests_bypass(),
            "policy=respect should not be bypass",
        )
    }

    #[test]
    fn parse_redaction_from_json() -> TestResult {
        let json = serde_json::json!({
            "policy": "bypass",
            "allowCategories": ["pii", "internal"]
        });
        let filters = parse_redaction(&json);
        ensure(
            filters.policy.as_deref() == Some("bypass"),
            "should parse policy",
        )?;
        ensure(
            filters.allow_categories == vec!["pii", "internal"],
            "should parse allowCategories",
        )
    }

    #[test]
    fn posture_for_trust_class_mapping() -> TestResult {
        ensure(
            posture_for_trust_class("human_explicit") == "authoritative",
            "human_explicit should map to authoritative",
        )?;
        ensure(
            posture_for_trust_class("agent_validated") == "authoritative",
            "agent_validated should map to authoritative",
        )?;
        ensure(
            posture_for_trust_class("agent_assertion") == "advisory",
            "agent_assertion should map to advisory",
        )?;
        ensure(
            posture_for_trust_class("cass_evidence") == "advisory",
            "cass_evidence should map to advisory",
        )?;
        ensure(
            posture_for_trust_class("legacy_import") == "legacy_evidence",
            "legacy_import should map to legacy_evidence",
        )?;
        ensure(
            posture_for_trust_class("unknown_class") == "advisory",
            "unknown class should default to advisory",
        )
    }

    #[test]
    fn pagination_empty_is_default() -> TestResult {
        let pagination = QueryPagination::default();
        ensure(
            pagination.is_empty(),
            "default QueryPagination should be empty",
        )?;
        ensure(
            pagination.effective_limit() == 50,
            "default limit should be 50",
        )
    }

    #[test]
    fn pagination_limit_capped_at_200() -> TestResult {
        let pagination = QueryPagination {
            limit: Some(500),
            cursor: None,
        };
        ensure(
            pagination.effective_limit() == 200,
            "limit should be capped at 200",
        )
    }

    #[test]
    fn pagination_cursor_roundtrip() -> TestResult {
        let cursor = PaginationCursor {
            offset: 42,
            query_hash: "abc123def456".to_string(),
        };
        let encoded = cursor.encode();
        let decoded =
            PaginationCursor::decode(&encoded).map_err(|e| format!("decode failed: {e}"))?;
        ensure(decoded.offset == 42, "offset should survive roundtrip")?;
        ensure(
            decoded.query_hash == "abc123def456",
            "query_hash should survive roundtrip",
        )
    }

    #[test]
    fn pagination_cursor_rejects_malformed() -> TestResult {
        ensure(
            PaginationCursor::decode("not-base64!!!").is_err(),
            "should reject invalid base64",
        )?;
        ensure(
            PaginationCursor::decode("bm90LWpzb24").is_err(),
            "should reject non-JSON (base64 of 'not-json')",
        )
    }

    #[test]
    fn parse_pagination_from_json() -> TestResult {
        let json = serde_json::json!({
            "limit": 25,
            "cursor": "eyJvIjoxMCwiaCI6InRlc3QifQ"
        });
        let pagination = parse_pagination(&json);
        ensure(pagination.limit == Some(25), "should parse limit")?;
        ensure(
            pagination.cursor.as_deref() == Some("eyJvIjoxMCwiaCI6InRlc3QifQ"),
            "should parse cursor",
        )
    }

    #[test]
    fn compute_query_shape_hash_deterministic() -> TestResult {
        let filters = QueryFilters::default();
        let hash1 = compute_query_shape_hash("test query", &filters);
        let hash2 = compute_query_shape_hash("test query", &filters);
        ensure(hash1 == hash2, "same input should produce same hash")?;

        let hash3 = compute_query_shape_hash("different query", &filters);
        ensure(
            hash1 != hash3,
            "different query should produce different hash",
        )
    }
}
