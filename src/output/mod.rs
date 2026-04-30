use std::env;
use std::io::IsTerminal;

use crate::core::capabilities::CapabilitiesReport;
use crate::core::check::CheckReport;
use crate::core::doctor::{DoctorReport, FixPlan};
use crate::core::health::HealthReport;
use crate::core::memory::{MemoryDetails, MemoryHistoryReport, MemoryListReport, MemoryShowReport};
use crate::core::quarantine::{QuarantineEntry, QuarantineReport};
use crate::core::status::StatusReport;
use crate::eval::{
    EvaluationReport, EvaluationStatus, ScenarioValidationResult, ValidationFailureKind,
};
use crate::models::{DomainError, ERROR_SCHEMA_V1, RESPONSE_SCHEMA_V1};
use crate::pack::{
    ContextResponse, PackDraftItem, PackItemProvenance, PackOmissionMetrics, PackQualityMetrics,
    PackSectionMetric, RenderedPackProvenance,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Renderer {
    #[default]
    Human,
    Json,
    Toon,
    Jsonl,
    Compact,
    Hook,
    Markdown,
}

impl Renderer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
            Self::Toon => "toon",
            Self::Jsonl => "jsonl",
            Self::Compact => "compact",
            Self::Hook => "hook",
            Self::Markdown => "markdown",
        }
    }

    #[must_use]
    pub const fn is_machine_readable(self) -> bool {
        matches!(self, Self::Json | Self::Jsonl | Self::Compact | Self::Hook)
    }
}

/// Field profile controls the verbosity of JSON output.
///
/// - `Minimal`: IDs, status, version only — bare minimum for scripting
/// - `Summary`: + top-level metrics and counts
/// - `Standard`: + arrays with items, but without verbose details
/// - `Full`: everything including provenance, why, debug info
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FieldProfile {
    Minimal,
    Summary,
    #[default]
    Standard,
    Full,
}

impl FieldProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Summary => "summary",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }

    #[must_use]
    pub const fn include_arrays(self) -> bool {
        matches!(self, Self::Standard | Self::Full)
    }

    #[must_use]
    pub const fn include_summary_metrics(self) -> bool {
        !matches!(self, Self::Minimal)
    }

    #[must_use]
    pub const fn include_verbose_details(self) -> bool {
        matches!(self, Self::Full)
    }

    #[must_use]
    pub const fn include_provenance(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OutputContext {
    pub renderer: Renderer,
    pub field_profile: FieldProfile,
    pub is_tty: bool,
    pub color_enabled: bool,
}

impl OutputContext {
    #[must_use]
    pub fn detect() -> Self {
        Self::detect_with_hints(false, false, None)
    }

    #[must_use]
    pub fn detect_with_hints(
        json_flag: bool,
        robot_flag: bool,
        format_override: Option<Renderer>,
    ) -> Self {
        let is_tty = std::io::stdout().is_terminal();
        let no_color = env::var("NO_COLOR").is_ok();
        let ee_format = env::var("EE_FORMAT").ok();

        let renderer = if let Some(r) = format_override {
            r
        } else if json_flag || robot_flag {
            Renderer::Json
        } else if let Some(fmt) = ee_format {
            match fmt.to_lowercase().as_str() {
                "json" => Renderer::Json,
                "toon" => Renderer::Toon,
                "jsonl" => Renderer::Jsonl,
                "compact" => Renderer::Compact,
                "hook" => Renderer::Hook,
                _ => Renderer::Human,
            }
        } else {
            Renderer::Human
        };

        let color_enabled = is_tty && !no_color && !renderer.is_machine_readable();

        Self {
            renderer,
            field_profile: FieldProfile::Standard,
            is_tty,
            color_enabled,
        }
    }

    #[must_use]
    pub fn with_field_profile(mut self, profile: FieldProfile) -> Self {
        self.field_profile = profile;
        self
    }

    #[must_use]
    pub const fn is_machine_output(&self) -> bool {
        self.renderer.is_machine_readable()
    }
}

/// Severity level for degradation notices in the ee.response.v1 envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DegradationSeverity {
    Low,
    Medium,
    High,
}

impl DegradationSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A single degradation notice in the ee.response.v1 envelope.
///
/// Degradation notices tell consumers that the response is valid but
/// incomplete or limited in some way. The repair field suggests how to
/// resolve the degradation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Degradation {
    pub code: String,
    pub severity: DegradationSeverity,
    pub message: String,
    pub repair: String,
}

impl Degradation {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        severity: DegradationSeverity,
        message: impl Into<String>,
        repair: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
            repair: repair.into(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        let mut b = JsonBuilder::new();
        b.field_str("code", &self.code);
        b.field_str("severity", self.severity.as_str());
        b.field_str("message", &self.message);
        b.field_str("repair", &self.repair);
        b.finish()
    }
}

pub struct JsonBuilder {
    buffer: String,
    first: bool,
}

impl JsonBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: String::from("{"),
            first: true,
        }
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let mut buffer = String::with_capacity(capacity);
        buffer.push('{');
        Self {
            buffer,
            first: true,
        }
    }

    pub fn field_str(&mut self, key: &str, value: &str) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":\"");
        self.buffer.push_str(&escape_json_string(value));
        self.buffer.push('"');
        self
    }

    pub fn field_raw(&mut self, key: &str, raw_json: &str) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":");
        self.buffer.push_str(raw_json);
        self
    }

    pub fn field_bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.field_raw(key, if value { "true" } else { "false" })
    }

    pub fn field_u32(&mut self, key: &str, value: u32) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":");
        self.buffer.push_str(&value.to_string());
        self
    }

    pub fn field_object<F>(&mut self, key: &str, build: F) -> &mut Self
    where
        F: FnOnce(&mut JsonBuilder),
    {
        let mut nested = JsonBuilder::new();
        build(&mut nested);
        let nested_json = nested.finish();
        self.field_raw(key, &nested_json)
    }

    pub fn field_array_of_objects<T, F>(&mut self, key: &str, items: &[T], build: F) -> &mut Self
    where
        F: Fn(&mut JsonBuilder, &T),
    {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":[");
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.buffer.push(',');
            }
            let mut nested = JsonBuilder::new();
            build(&mut nested, item);
            self.buffer.push_str(&nested.finish());
        }
        self.buffer.push(']');
        self
    }

    fn separator(&mut self) {
        if self.first {
            self.first = false;
        } else {
            self.buffer.push(',');
        }
    }

    #[must_use]
    pub fn finish(mut self) -> String {
        self.buffer.push('}');
        self.buffer
    }
}

impl Default for JsonBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ResponseEnvelope {
    builder: JsonBuilder,
}

impl ResponseEnvelope {
    #[must_use]
    pub fn success() -> Self {
        let mut builder = JsonBuilder::with_capacity(256);
        builder.field_str("schema", RESPONSE_SCHEMA_V1);
        builder.field_bool("success", true);
        Self { builder }
    }

    #[must_use]
    pub fn failure() -> Self {
        let mut builder = JsonBuilder::with_capacity(256);
        builder.field_str("schema", RESPONSE_SCHEMA_V1);
        builder.field_bool("success", false);
        Self { builder }
    }

    pub fn data<F>(mut self, build: F) -> Self
    where
        F: FnOnce(&mut JsonBuilder),
    {
        self.builder.field_object("data", build);
        self
    }

    pub fn data_raw(mut self, raw_json: &str) -> Self {
        self.builder.field_raw("data", raw_json);
        self
    }

    pub fn degraded_array<T, F>(mut self, items: &[T], build: F) -> Self
    where
        F: Fn(&mut JsonBuilder, &T),
    {
        self.builder
            .field_array_of_objects("degraded", items, build);
        self
    }

    #[must_use]
    pub fn finish(self) -> String {
        self.builder.finish()
    }
}

/// Render a context response as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_context_response_json(response: &ContextResponse) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", response.schema);
    b.field_bool("success", response.success);
    b.field_object("data", |d| {
        d.field_str("command", response.data.command);
        d.field_object("request", |request| {
            request.field_str("query", &response.data.request.query);
            request.field_str("profile", response.data.request.profile.as_str());
            request.field_u32("maxTokens", response.data.request.budget.max_tokens());
            request.field_u32("candidatePool", response.data.request.candidate_pool);
            let sections = string_array_json(
                response
                    .data
                    .request
                    .sections
                    .iter()
                    .map(|section| section.as_str()),
            );
            request.field_raw("sections", &sections);
        });
        d.field_object("pack", |pack| {
            pack.field_str("query", &response.data.pack.query);
            pack.field_object("budget", |budget| {
                budget.field_u32("maxTokens", response.data.pack.budget.max_tokens());
                budget.field_u32("usedTokens", response.data.pack.used_tokens);
            });
            let quality_metrics = response.data.pack.quality_metrics();
            pack.field_object("quality", |quality| {
                build_pack_quality_metrics(quality, &quality_metrics);
            });
            pack.field_array_of_objects("items", &response.data.pack.items, |obj, item| {
                obj.field_u32("rank", item.rank);
                obj.field_str("memoryId", &item.memory_id.to_string());
                obj.field_str("section", item.section.as_str());
                obj.field_str("content", &item.content);
                obj.field_u32("estimatedTokens", item.estimated_tokens);
                obj.field_object("scores", |scores| {
                    scores.field_raw("relevance", &score_json(item.relevance.into_inner()));
                    scores.field_raw("utility", &score_json(item.utility.into_inner()));
                });
                let provenance = item.rendered_provenance();
                obj.field_array_of_objects("provenance", &provenance, build_rendered_provenance);
                obj.field_str("why", &item.why);
                if let Some(diversity_key) = &item.diversity_key {
                    obj.field_str("diversityKey", diversity_key);
                }
            });
            pack.field_array_of_objects("omitted", &response.data.pack.omitted, |obj, omission| {
                obj.field_str("memoryId", &omission.memory_id.to_string());
                obj.field_u32("estimatedTokens", omission.estimated_tokens);
                obj.field_str("reason", omission.reason.as_str());
            });
            let footer = response.data.pack.provenance_footer();
            pack.field_object("provenanceFooter", |obj| {
                obj.field_raw("memoryCount", &footer.memory_count.to_string());
                obj.field_raw("sourceCount", &footer.source_count.to_string());
                obj.field_raw(
                    "schemes",
                    &string_array_json(footer.schemes.iter().copied()),
                );
                obj.field_array_of_objects("entries", &footer.entries, build_item_provenance);
            });
        });
        d.field_array_of_objects("degraded", &response.data.degraded, |obj, degraded| {
            obj.field_str("code", &degraded.code);
            obj.field_str("severity", degraded.severity.as_str());
            obj.field_str("message", &degraded.message);
            if let Some(repair) = &degraded.repair {
                obj.field_str("repair", repair);
            }
        });
    });
    b.finish()
}

/// Render a context response as human-readable text.
#[must_use]
pub fn render_context_response_human(response: &ContextResponse) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "ee context \"{}\"\n\n",
        response.data.request.query
    ));
    output.push_str(&format!(
        "Profile: {} | Budget: {}/{} tokens\n\n",
        response.data.request.profile.as_str(),
        response.data.pack.used_tokens,
        response.data.pack.budget.max_tokens()
    ));

    if response.data.pack.items.is_empty() {
        output.push_str("No items in pack.\n");
    } else {
        output.push_str("Items:\n");
        for item in &response.data.pack.items {
            output.push_str(&format!(
                "  {}. [{}] {} ({}t)\n",
                item.rank,
                item.section.as_str(),
                item.memory_id,
                item.estimated_tokens
            ));
        }
    }

    if !response.data.degraded.is_empty() {
        output.push_str("\nDegraded:\n");
        for d in &response.data.degraded {
            output.push_str(&format!("  [{}] {}\n", d.severity.as_str(), d.message));
            if let Some(repair) = &d.repair {
                output.push_str(&format!("    Next: {repair}\n"));
            }
        }
    }

    output.push_str("\nNext:\n  ee context --json \"<query>\"\n");
    output
}

/// Render a context response as TOON.
#[must_use]
pub fn render_context_response_toon(response: &ContextResponse) -> String {
    render_toon_from_json(&render_context_response_json(response))
}

/// Render a context response as Markdown.
///
/// Produces a structured Markdown document suitable for direct inclusion
/// in agent context windows or documentation. Sections are organized by
/// pack section, with provenance and why explanations preserved.
#[must_use]
pub fn render_context_response_markdown(response: &ContextResponse) -> String {
    use std::collections::BTreeMap;

    let mut output = String::new();

    output.push_str(&format!(
        "# Context Pack: {}\n\n",
        response.data.request.query
    ));

    output.push_str(&format!(
        "**Profile:** {} | **Budget:** {}/{} tokens\n\n",
        response.data.request.profile.as_str(),
        response.data.pack.used_tokens,
        response.data.pack.budget.max_tokens()
    ));

    if response.data.pack.items.is_empty() {
        output.push_str("*No items in pack.*\n\n");
    } else {
        let mut by_section: BTreeMap<&str, Vec<&PackDraftItem>> = BTreeMap::new();
        for item in &response.data.pack.items {
            by_section
                .entry(item.section.as_str())
                .or_default()
                .push(item);
        }

        for (section, items) in by_section {
            output.push_str(&format!("## {}\n\n", section_display_name(section)));
            for item in items {
                output.push_str(&format!(
                    "### {}. {} ({} tokens)\n\n",
                    item.rank, item.memory_id, item.estimated_tokens
                ));

                if !item.content.is_empty() {
                    output.push_str("```\n");
                    output.push_str(&item.content);
                    if !item.content.ends_with('\n') {
                        output.push('\n');
                    }
                    output.push_str("```\n\n");
                }

                if !item.why.is_empty() {
                    output.push_str(&format!("**Why:** {}\n\n", item.why));
                }

                if !item.provenance.is_empty() {
                    output.push_str("**Provenance:**\n");
                    for prov in item.rendered_provenance() {
                        output.push_str(&format!("- {} ({})\n", prov.uri, prov.scheme));
                    }
                    output.push('\n');
                }
            }
        }
    }

    if !response.data.pack.omitted.is_empty() {
        output.push_str("## Omitted\n\n");
        for omission in &response.data.pack.omitted {
            output.push_str(&format!(
                "- {} ({} tokens) — {}\n",
                omission.memory_id, omission.estimated_tokens, omission.reason
            ));
        }
        output.push('\n');
    }

    if !response.data.degraded.is_empty() {
        output.push_str("## Degradations\n\n");
        for d in &response.data.degraded {
            output.push_str(&format!("- **[{}]** {}\n", d.severity.as_str(), d.message));
            if let Some(repair) = &d.repair {
                output.push_str(&format!("  - *Repair:* `{}`\n", repair));
            }
        }
        output.push('\n');
    }

    output.push_str("---\n\n");
    output.push_str(&format!(
        "*Generated by `ee context \"{}\" --format markdown`*\n",
        response.data.request.query
    ));

    output
}

fn section_display_name(section: &str) -> &str {
    match section {
        "core" => "Core",
        "supporting" => "Supporting",
        "procedural" => "Procedural",
        "background" => "Background",
        "example" => "Example",
        other => other,
    }
}

fn build_pack_quality_metrics(obj: &mut JsonBuilder, metrics: &PackQualityMetrics) {
    obj.field_raw("itemCount", &metrics.item_count.to_string());
    obj.field_raw("omittedCount", &metrics.omitted_count.to_string());
    obj.field_u32("usedTokens", metrics.used_tokens);
    obj.field_u32("maxTokens", metrics.max_tokens);
    obj.field_raw("budgetUtilization", &score_json(metrics.budget_utilization));
    obj.field_raw("averageRelevance", &score_json(metrics.average_relevance));
    obj.field_raw("averageUtility", &score_json(metrics.average_utility));
    obj.field_raw(
        "provenanceSourceCount",
        &metrics.provenance_source_count.to_string(),
    );
    obj.field_raw(
        "provenanceSourcesPerItem",
        &score_json(metrics.provenance_sources_per_item),
    );
    obj.field_bool("provenanceComplete", metrics.provenance_complete);
    obj.field_array_of_objects("sections", &metrics.sections, build_pack_section_metric);
    obj.field_object("omissions", |omissions| {
        build_pack_omission_metrics(omissions, &metrics.omissions);
    });
}

fn build_pack_section_metric(obj: &mut JsonBuilder, metric: &PackSectionMetric) {
    obj.field_str("section", metric.section.as_str());
    obj.field_raw("itemCount", &metric.item_count.to_string());
    obj.field_u32("usedTokens", metric.used_tokens);
}

fn build_pack_omission_metrics(obj: &mut JsonBuilder, metrics: &PackOmissionMetrics) {
    obj.field_raw(
        "tokenBudgetExceeded",
        &metrics.token_budget_exceeded.to_string(),
    );
    obj.field_raw(
        "redundantCandidates",
        &metrics.redundant_candidates.to_string(),
    );
}

fn build_rendered_provenance(obj: &mut JsonBuilder, source: &RenderedPackProvenance) {
    obj.field_str("uri", &source.uri);
    obj.field_str("scheme", source.scheme);
    obj.field_str("label", &source.label);
    if let Some(locator) = &source.locator {
        obj.field_str("locator", locator);
    }
    obj.field_str("note", &source.note);
}

fn build_item_provenance(obj: &mut JsonBuilder, entry: &PackItemProvenance) {
    obj.field_u32("rank", entry.rank);
    obj.field_str("memoryId", &entry.memory_id.to_string());
    obj.field_u32("sourceIndex", entry.source_index);
    obj.field_object("source", |source| {
        build_rendered_provenance(source, &entry.source);
    });
}

fn score_json(score: f32) -> String {
    format!("{score:.6}")
}

fn string_array_json<I, S>(values: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut output = String::from("[");
    for (index, value) in values.into_iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('"');
        output.push_str(&escape_json_string(value.as_ref()));
        output.push('"');
    }
    output.push(']');
    output
}

/// Render a status report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_status_json(report: &StatusReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "status");
        d.field_str("version", report.version);
        d.field_object("capabilities", |c| {
            c.field_str("runtime", report.capabilities.runtime.as_str());
            c.field_str("storage", report.capabilities.storage.as_str());
            c.field_str("search", report.capabilities.search.as_str());
        });
        d.field_object("runtime", |r| {
            r.field_str("engine", report.runtime.engine);
            r.field_str("profile", report.runtime.profile);
            r.field_raw("workerThreads", &report.runtime.worker_threads.to_string());
            r.field_str("asyncBoundary", report.runtime.async_boundary);
        });
        render_memory_health_json(d, &report.memory_health);
        d.field_array_of_objects("degraded", &report.degradations, |obj, deg| {
            obj.field_str("code", deg.code);
            obj.field_str("severity", deg.severity);
            obj.field_str("message", deg.message);
            obj.field_str("repair", deg.repair);
        });
    });
    b.finish()
}

fn render_memory_health_json(
    parent: &mut JsonBuilder,
    health: &crate::core::status::MemoryHealthReport,
) {
    parent.field_object("memoryHealth", |h| {
        h.field_str("status", health.status.as_str());
        h.field_u32("totalCount", health.total_count);
        h.field_u32("activeCount", health.active_count);
        h.field_u32("tombstonedCount", health.tombstoned_count);
        h.field_u32("staleCount", health.stale_count);
        match health.average_confidence {
            Some(c) => h.field_raw("averageConfidence", &format!("{c:.2}")),
            None => h.field_raw("averageConfidence", "null"),
        };
        match health.provenance_coverage {
            Some(c) => h.field_raw("provenanceCoverage", &format!("{c:.2}")),
            None => h.field_raw("provenanceCoverage", "null"),
        };
    });
}

/// Render a status report as JSON with optional timing metadata.
///
/// When `timing` is provided, adds a `meta` object with timing fields.
#[must_use]
pub fn render_status_json_with_meta(
    report: &StatusReport,
    timing: Option<&crate::models::DiagnosticTiming>,
) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "status");
        d.field_str("version", report.version);
        d.field_object("capabilities", |c| {
            c.field_str("runtime", report.capabilities.runtime.as_str());
            c.field_str("storage", report.capabilities.storage.as_str());
            c.field_str("search", report.capabilities.search.as_str());
        });
        d.field_object("runtime", |r| {
            r.field_str("engine", report.runtime.engine);
            r.field_str("profile", report.runtime.profile);
            r.field_raw("workerThreads", &report.runtime.worker_threads.to_string());
            r.field_str("asyncBoundary", report.runtime.async_boundary);
        });
        render_memory_health_json(d, &report.memory_health);
        d.field_array_of_objects("degraded", &report.degradations, |obj, deg| {
            obj.field_str("code", deg.code);
            obj.field_str("severity", deg.severity);
            obj.field_str("message", deg.message);
            obj.field_str("repair", deg.repair);
        });
    });
    if let Some(t) = timing {
        b.field_object("meta", |m| {
            m.field_object("timing", |tm| {
                tm.field_raw("elapsedMs", &format!("{:.3}", t.elapsed_ms));
                if !t.phases.is_empty() {
                    tm.field_array_of_objects("phases", &t.phases, |obj, phase| {
                        obj.field_str("name", phase.name);
                        obj.field_raw("durationMs", &format!("{:.3}", phase.duration_ms));
                    });
                }
            });
        });
    }
    b.finish()
}

/// Render a status report as human-readable text.
#[must_use]
pub fn render_status_human(report: &StatusReport) -> String {
    format!(
        "ee status\n\nstorage: {}\nsearch: {}\nruntime: {} ({} {})\n\nNext:\n  ee status --json\n",
        report.capabilities.storage.as_str(),
        report.capabilities.search.as_str(),
        report.capabilities.runtime.as_str(),
        report.runtime.engine,
        report.runtime.profile
    )
}

/// Render a status report as TOON (Terse Object Output Notation).
#[must_use]
pub fn render_status_toon(report: &StatusReport) -> String {
    render_toon_from_json(&render_status_json(report))
}

/// Render a doctor report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_doctor_json(report: &DoctorReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.overall_healthy);
    b.field_object("data", |d| {
        d.field_str("command", "doctor");
        d.field_str("version", report.version);
        d.field_bool("healthy", report.overall_healthy);
        d.field_array_of_objects("checks", &report.checks, |obj, check| {
            obj.field_str("name", check.name);
            obj.field_str("severity", check.severity.as_str());
            obj.field_str("message", &check.message);
            if let Some(code) = check.error_code {
                obj.field_str("errorCode", code.id);
            }
            if let Some(repair) = check.repair {
                obj.field_str("repair", repair);
            }
        });
    });
    b.finish()
}

/// Render a doctor report as human-readable text.
#[must_use]
pub fn render_doctor_human(report: &DoctorReport) -> String {
    let mut output = String::from("ee doctor\n\n");

    for check in &report.checks {
        let icon = match check.severity {
            crate::core::doctor::CheckSeverity::Ok => "✓",
            crate::core::doctor::CheckSeverity::Warning => "⚠",
            crate::core::doctor::CheckSeverity::Error => "✗",
        };
        output.push_str(&format!("{} {}: {}\n", icon, check.name, check.message));
        if let Some(repair) = check.repair {
            output.push_str(&format!("  repair: {}\n", repair));
        }
    }

    if report.overall_healthy {
        output.push_str("\nAll checks passed.\n");
    } else {
        output.push_str("\nSome checks failed. Run suggested repairs to fix issues.\n");
    }

    output
}

/// Render a doctor report as TOON.
#[must_use]
pub fn render_doctor_toon(report: &DoctorReport) -> String {
    render_toon_from_json(&render_doctor_json(report))
}

/// Render a fix plan as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_fix_plan_json(plan: &FixPlan) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "doctor");
        d.field_str("mode", "fix-plan");
        d.field_str("version", plan.version);
        d.field_raw("totalIssues", &plan.total_issues.to_string());
        d.field_raw("fixableIssues", &plan.fixable_issues.to_string());
        d.field_array_of_objects("steps", &plan.steps, |obj, step| {
            obj.field_raw("order", &step.order.to_string());
            obj.field_str("subsystem", step.subsystem);
            obj.field_str("severity", step.severity.as_str());
            obj.field_str("issue", &step.issue);
            if let Some(code) = step.error_code {
                obj.field_str("errorCode", code.id);
            }
            obj.field_str("command", step.command);
        });
    });
    b.finish()
}

/// Render a fix plan as human-readable text.
#[must_use]
pub fn render_fix_plan_human(plan: &FixPlan) -> String {
    let mut output = String::from("ee doctor --fix-plan\n\n");

    if plan.is_empty() {
        output.push_str("No issues to fix. All subsystems are healthy.\n");
        return output;
    }

    output.push_str(&format!(
        "Found {} issue(s), {} fixable:\n\n",
        plan.total_issues, plan.fixable_issues
    ));

    for step in &plan.steps {
        output.push_str(&format!(
            "{}. [{}] {}\n   Issue: {}\n   Fix:   {}\n\n",
            step.order,
            step.subsystem,
            step.severity.as_str().to_uppercase(),
            step.issue,
            step.command
        ));
    }

    if plan.fixable_issues > 0 {
        output.push_str("Run commands in order to resolve issues.\n");
    }

    output
}

/// Render a fix plan as TOON.
#[must_use]
pub fn render_fix_plan_toon(plan: &FixPlan) -> String {
    render_toon_from_json(&render_fix_plan_json(plan))
}

/// Render a quarantine report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_quarantine_json(report: &QuarantineReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "diag quarantine");
        d.field_str("version", report.version);
        d.field_object("summary", |s| {
            s.field_raw(
                "quarantinedCount",
                &report.summary.quarantined_count.to_string(),
            );
            s.field_raw("atRiskCount", &report.summary.at_risk_count.to_string());
            s.field_raw("blockedCount", &report.summary.blocked_count.to_string());
            s.field_raw("totalSources", &report.summary.total_sources.to_string());
            s.field_raw("healthyCount", &report.summary.healthy_count.to_string());
        });
        d.field_array_of_objects(
            "quarantinedSources",
            &report.quarantined_sources,
            build_quarantine_entry,
        );
        d.field_array_of_objects(
            "atRiskSources",
            &report.at_risk_sources,
            build_quarantine_entry,
        );
        d.field_array_of_objects(
            "blockedSources",
            &report.blocked_sources,
            build_quarantine_entry,
        );
    });
    b.finish()
}

fn build_quarantine_entry(obj: &mut JsonBuilder, entry: &QuarantineEntry) {
    obj.field_str("sourceId", &entry.source_id);
    obj.field_str("advisory", entry.advisory.as_str());
    obj.field_raw("effectiveTrust", &format!("{:.4}", entry.effective_trust));
    obj.field_raw("decayFactor", &format!("{:.4}", entry.decay_factor));
    obj.field_raw("negativeRate", &format!("{:.4}", entry.negative_rate));
    obj.field_raw("negativeCount", &entry.negative_count.to_string());
    obj.field_raw("totalImports", &entry.total_imports.to_string());
    obj.field_str("message", &entry.message);
    obj.field_bool("permitsImport", entry.permits_import);
    obj.field_bool("requiresValidation", entry.requires_validation);
}

/// Render a quarantine report as human-readable text.
#[must_use]
pub fn render_quarantine_human(report: &QuarantineReport) -> String {
    let mut output = format!("ee diag quarantine (v{})\n\n", report.version);

    if !report.has_issues() {
        output.push_str("No sources require attention.\n");
        output.push_str(&format!(
            "Tracked: {} sources, {} healthy\n\n",
            report.summary.total_sources, report.summary.healthy_count
        ));
        output.push_str("Next:\n  ee diag quarantine --json\n");
        return output;
    }

    output.push_str(&format!(
        "Summary: {} quarantined, {} at risk, {} blocked\n\n",
        report.summary.quarantined_count,
        report.summary.at_risk_count,
        report.summary.blocked_count
    ));

    if !report.blocked_sources.is_empty() {
        output.push_str("Blocked Sources:\n");
        for entry in &report.blocked_sources {
            output.push_str(&format!(
                "  ✗ {} (trust {:.2})\n    {}\n",
                entry.source_id, entry.effective_trust, entry.message
            ));
        }
        output.push('\n');
    }

    if !report.quarantined_sources.is_empty() {
        output.push_str("Quarantined Sources:\n");
        for entry in &report.quarantined_sources {
            output.push_str(&format!(
                "  ⚠ {} (trust {:.2}, decay {:.2})\n    {}\n",
                entry.source_id, entry.effective_trust, entry.decay_factor, entry.message
            ));
        }
        output.push('\n');
    }

    if !report.at_risk_sources.is_empty() {
        output.push_str("At-Risk Sources:\n");
        for entry in &report.at_risk_sources {
            output.push_str(&format!(
                "  ◐ {} (trust {:.2})\n    {}\n",
                entry.source_id, entry.effective_trust, entry.message
            ));
        }
        output.push('\n');
    }

    output.push_str("Next:\n  ee diag quarantine --json\n  ee import --validate-sources\n");
    output
}

/// Render a quarantine report as TOON.
#[must_use]
pub fn render_quarantine_toon(report: &QuarantineReport) -> String {
    render_toon_from_json(&render_quarantine_json(report))
}

/// Render a streams diagnostic report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_streams_json(report: &crate::core::streams::StreamsReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.is_healthy());
    b.field_object("data", |d| {
        d.field_str("command", "diag streams");
        d.field_str("version", report.version);
        d.field_bool("stdoutIsolated", report.stdout_isolated);
        d.field_bool("stderrReceivedProbe", report.stderr_received_probe);
        d.field_str("stderrProbeMessage", &report.stderr_probe_message);
        d.field_bool("healthy", report.is_healthy());
    });
    b.finish()
}

/// Render a streams diagnostic report as human-readable text.
#[must_use]
pub fn render_streams_human(report: &crate::core::streams::StreamsReport) -> String {
    let mut output = format!("ee diag streams (v{})\n\n", report.version);

    if report.is_healthy() {
        output.push_str("Stream separation: OK\n\n");
        output.push_str("  stdout: isolated for machine data\n");
        output.push_str("  stderr: received diagnostic probe\n\n");
        output.push_str("This confirms that stdout contains only machine-readable data\n");
        output
            .push_str("and stderr receives diagnostics, as required for agent-native operation.\n");
    } else {
        output.push_str("Stream separation: FAILED\n\n");
        if !report.stdout_isolated {
            output.push_str("  ✗ stdout is not isolated\n");
        }
        if !report.stderr_received_probe {
            output.push_str("  ✗ stderr did not receive probe\n");
        }
        output.push_str("\nNext:\n  Check for stderr redirection or write failures.\n");
    }

    output
}

/// Render a streams diagnostic report as TOON.
#[must_use]
pub fn render_streams_toon(report: &crate::core::streams::StreamsReport) -> String {
    render_toon_from_json(&render_streams_json(report))
}

/// Render a check report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_check_json(report: &CheckReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.posture.is_usable());
    b.field_object("data", |d| {
        d.field_str("command", "check");
        d.field_str("version", report.version);
        d.field_str("posture", report.posture.as_str());
        d.field_bool("workspaceInitialized", report.workspace_initialized);
        d.field_bool("databaseReady", report.database_ready);
        d.field_bool("searchReady", report.search_ready);
        d.field_bool("runtimeReady", report.runtime_ready);
        d.field_array_of_objects(
            "suggestedActions",
            &report.suggested_actions,
            |obj, action| {
                obj.field_raw("priority", &action.priority.to_string());
                obj.field_str("command", action.command);
                obj.field_str("reason", action.reason);
            },
        );
    });
    b.finish()
}

/// Render a check report as human-readable text.
#[must_use]
pub fn render_check_human(report: &CheckReport) -> String {
    let mut output = format!("ee check\n\nposture: {}\n\n", report.posture.as_str());

    output.push_str(&format!(
        "workspace: {}\ndatabase: {}\nsearch: {}\nruntime: {}\n",
        if report.workspace_initialized {
            "initialized"
        } else {
            "not initialized"
        },
        if report.database_ready {
            "ready"
        } else {
            "not ready"
        },
        if report.search_ready {
            "ready"
        } else {
            "not ready"
        },
        if report.runtime_ready {
            "ready"
        } else {
            "not ready"
        },
    ));

    if !report.suggested_actions.is_empty() {
        output.push_str("\nNext:\n");
        for action in &report.suggested_actions {
            output.push_str(&format!("  {} — {}\n", action.command, action.reason));
        }
    }

    output
}

/// Render a check report as TOON.
#[must_use]
pub fn render_check_toon(report: &CheckReport) -> String {
    render_toon_from_json(&render_check_json(report))
}

/// Render a health report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_health_json(report: &HealthReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.verdict.is_healthy());
    b.field_object("data", |d| {
        d.field_str("command", "health");
        d.field_str("version", report.version);
        d.field_str("verdict", report.verdict.as_str());
        d.field_object("subsystems", |s| {
            s.field_bool("runtime", report.runtime_ok);
            s.field_bool("storage", report.storage_ok);
            s.field_bool("search", report.search_ok);
        });
        d.field_object("summary", |s| {
            s.field_raw("issueCount", &report.issue_count().to_string());
            s.field_raw("highSeverity", &report.high_severity_count().to_string());
            s.field_raw(
                "mediumSeverity",
                &report.medium_severity_count().to_string(),
            );
        });
        d.field_array_of_objects("issues", &report.issues, |obj, issue| {
            obj.field_str("subsystem", issue.subsystem);
            obj.field_str("code", issue.code);
            obj.field_str("severity", issue.severity);
            obj.field_str("message", issue.message);
        });
    });
    b.finish()
}

/// Render a health report as human-readable text.
#[must_use]
pub fn render_health_human(report: &HealthReport) -> String {
    let mut output = format!(
        "ee health (v{})\n\nVerdict: {}\n\n",
        report.version,
        report.verdict.as_str().to_uppercase()
    );

    output.push_str("Subsystems:\n");
    output.push_str(&format!(
        "  runtime: {}\n  storage: {}\n  search: {}\n",
        if report.runtime_ok { "ok" } else { "not ok" },
        if report.storage_ok { "ok" } else { "not ok" },
        if report.search_ok { "ok" } else { "not ok" },
    ));

    if !report.issues.is_empty() {
        output.push_str(&format!("\nIssues ({}):\n", report.issue_count()));
        for issue in &report.issues {
            output.push_str(&format!(
                "  [{}] {} — {}\n",
                issue.severity, issue.subsystem, issue.message
            ));
        }
    }

    output.push_str("\nNext:\n  ee health --json\n  ee doctor\n");
    output
}

/// Render a health report as TOON.
#[must_use]
pub fn render_health_toon(report: &HealthReport) -> String {
    render_toon_from_json(&render_health_json(report))
}

/// Render a memory show report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_memory_show_json(report: &MemoryShowReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.found && report.error.is_none());
    b.field_object("data", |d| {
        d.field_str("command", "memory show");
        d.field_str("version", report.version);
        d.field_bool("found", report.found);
        d.field_bool("is_tombstoned", report.is_tombstoned);

        if let Some(ref details) = report.memory {
            d.field_object("memory", |m| {
                render_memory_fields(m, details);
            });
        }

        if let Some(ref err) = report.error {
            d.field_str("error", err);
        }
    });
    b.finish()
}

/// Render memory fields into a JSON builder.
fn render_memory_fields(b: &mut JsonBuilder, details: &MemoryDetails) {
    let mem = &details.memory;
    b.field_str("id", &mem.id);
    b.field_str("workspace_id", &mem.workspace_id);
    b.field_str("level", &mem.level);
    b.field_str("kind", &mem.kind);
    b.field_str("content", &mem.content);
    b.field_raw("confidence", &format!("{:.4}", mem.confidence));
    b.field_raw("utility", &format!("{:.4}", mem.utility));
    b.field_raw("importance", &format!("{:.4}", mem.importance));
    if let Some(ref uri) = mem.provenance_uri {
        b.field_str("provenance_uri", uri);
    }
    b.field_str("trust_class", &mem.trust_class);
    if let Some(ref sub) = mem.trust_subclass {
        b.field_str("trust_subclass", sub);
    }
    b.field_str("created_at", &mem.created_at);
    b.field_str("updated_at", &mem.updated_at);
    if let Some(ref ts) = mem.tombstoned_at {
        b.field_str("tombstoned_at", ts);
    }
    b.field_array_of_objects("tags", &details.tags, |obj, tag| {
        obj.field_str("name", tag);
    });
}

/// Render a memory show report as human-readable text.
#[must_use]
pub fn render_memory_show_human(report: &MemoryShowReport) -> String {
    if let Some(ref err) = report.error {
        return format!("error: {err}\n");
    }

    if !report.found {
        return "Memory not found.\n".to_string();
    }

    let details = match &report.memory {
        Some(d) => d,
        None => return "Memory not found.\n".to_string(),
    };

    let mem = &details.memory;
    let mut output = format!("Memory: {}\n\n", mem.id);
    output.push_str(&format!("  Level: {}\n", mem.level));
    output.push_str(&format!("  Kind: {}\n", mem.kind));
    output.push_str(&format!("  Content:\n    {}\n", mem.content));
    output.push_str(&format!(
        "  Scores: confidence={:.2}, utility={:.2}, importance={:.2}\n",
        mem.confidence, mem.utility, mem.importance
    ));
    output.push_str(&format!("  Trust: {}", mem.trust_class));
    if let Some(ref sub) = mem.trust_subclass {
        output.push_str(&format!(" ({})", sub));
    }
    output.push('\n');
    if let Some(ref uri) = mem.provenance_uri {
        output.push_str(&format!("  Provenance: {}\n", uri));
    }
    output.push_str(&format!("  Created: {}\n", mem.created_at));
    output.push_str(&format!("  Updated: {}\n", mem.updated_at));
    if let Some(ref ts) = mem.tombstoned_at {
        output.push_str(&format!("  Tombstoned: {}\n", ts));
    }
    if !details.tags.is_empty() {
        output.push_str(&format!("  Tags: {}\n", details.tags.join(", ")));
    }
    output
}

/// Render a memory show report as TOON.
#[must_use]
pub fn render_memory_show_toon(report: &MemoryShowReport) -> String {
    render_toon_from_json(&render_memory_show_json(report))
}

/// Render a memory list report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_memory_list_json(report: &MemoryListReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.error.is_none());
    b.field_object("data", |d| {
        d.field_str("command", "memory list");
        d.field_str("version", report.version);
        d.field_u32("total_count", report.total_count);
        d.field_bool("truncated", report.truncated);

        d.field_object("filter", |f| {
            if let Some(ref level) = report.filter.level {
                f.field_str("level", level);
            }
            if let Some(ref tag) = report.filter.tag {
                f.field_str("tag", tag);
            }
            f.field_bool("include_tombstoned", report.filter.include_tombstoned);
        });

        d.field_array_of_objects("memories", &report.memories, |obj, m| {
            obj.field_str("id", &m.id);
            obj.field_str("level", &m.level);
            obj.field_str("kind", &m.kind);
            obj.field_str("content_preview", &m.content_preview);
            obj.field_raw("confidence", &format!("{:.4}", m.confidence));
            if let Some(ref uri) = m.provenance_uri {
                obj.field_str("provenance_uri", uri);
            }
            obj.field_bool("is_tombstoned", m.is_tombstoned);
            obj.field_str("created_at", &m.created_at);
        });

        if let Some(ref err) = report.error {
            d.field_str("error", err);
        }
    });
    b.finish()
}

/// Render a memory list report as human-readable text.
#[must_use]
pub fn render_memory_list_human(report: &MemoryListReport) -> String {
    if let Some(ref err) = report.error {
        return format!("error: {err}\n");
    }

    let mut output = format!("Memories ({} total", report.total_count);
    if report.truncated {
        output.push_str(", showing first batch");
    }
    output.push_str(")\n\n");

    if report.memories.is_empty() {
        output.push_str("  No memories found.\n");
        return output;
    }

    for m in &report.memories {
        output.push_str(&format!("  {} [{}] {}\n", m.id, m.level, m.kind));
        output.push_str(&format!("    {}\n", m.content_preview));
        output.push_str(&format!(
            "    confidence={:.2}, created={}\n",
            m.confidence, m.created_at
        ));
        if m.is_tombstoned {
            output.push_str("    [TOMBSTONED]\n");
        }
        output.push('\n');
    }

    output.push_str("Next:\n  ee memory show <ID>\n");
    output
}

/// Render a memory list report as TOON.
#[must_use]
pub fn render_memory_list_toon(report: &MemoryListReport) -> String {
    render_toon_from_json(&render_memory_list_json(report))
}

/// Render a memory history report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_memory_history_json(report: &MemoryHistoryReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.error.is_none());
    b.field_object("data", |d| {
        d.field_str("command", "memory history");
        d.field_str("version", report.version);
        d.field_str("memory_id", &report.memory_id);
        d.field_bool("memory_exists", report.memory_exists);
        d.field_bool("is_tombstoned", report.is_tombstoned);
        d.field_u32("total_count", report.total_count);
        d.field_bool("truncated", report.truncated);

        d.field_array_of_objects("entries", &report.entries, |obj, e| {
            obj.field_str("audit_id", &e.audit_id);
            obj.field_str("timestamp", &e.timestamp);
            if let Some(ref actor) = e.actor {
                obj.field_str("actor", actor);
            }
            obj.field_str("action", &e.action);
            if let Some(ref details) = e.details {
                obj.field_raw("details", details);
            }
        });

        if let Some(ref err) = report.error {
            d.field_str("error", err);
        }
    });
    b.finish()
}

/// Render a memory history report as human-readable text.
#[must_use]
pub fn render_memory_history_human(report: &MemoryHistoryReport) -> String {
    if let Some(ref err) = report.error {
        return format!("error: {err}\n");
    }

    if !report.memory_exists {
        return format!("Memory not found: {}\n", report.memory_id);
    }

    let mut output = format!(
        "History for {} ({} entries",
        report.memory_id, report.total_count
    );
    if report.truncated {
        output.push_str(", showing first batch");
    }
    output.push_str(")\n");

    if report.is_tombstoned {
        output.push_str("  [TOMBSTONED]\n");
    }
    output.push('\n');

    if report.entries.is_empty() {
        output.push_str("  No history entries found.\n");
        return output;
    }

    for e in &report.entries {
        output.push_str(&format!("  {} [{}]\n", e.timestamp, e.action));
        if let Some(ref actor) = e.actor {
            output.push_str(&format!("    actor: {actor}\n"));
        }
        if let Some(ref details) = e.details {
            output.push_str(&format!("    details: {details}\n"));
        }
        output.push_str(&format!("    audit_id: {}\n\n", e.audit_id));
    }

    output
}

/// Render a memory history report as TOON.
#[must_use]
pub fn render_memory_history_toon(report: &MemoryHistoryReport) -> String {
    render_toon_from_json(&render_memory_history_json(report))
}

/// Render a capabilities report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_capabilities_json(report: &CapabilitiesReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "capabilities");
        d.field_str("version", report.version);
        d.field_array_of_objects("subsystems", &report.subsystems, |obj, sub| {
            obj.field_str("name", sub.name);
            obj.field_str("status", sub.status.as_str());
            obj.field_str("description", sub.description);
        });
        d.field_array_of_objects("features", &report.features, |obj, feat| {
            obj.field_str("name", feat.name);
            obj.field_bool("enabled", feat.enabled);
            obj.field_str("description", feat.description);
        });
        d.field_array_of_objects("commands", &report.commands, |obj, cmd| {
            obj.field_str("name", cmd.name);
            obj.field_bool("available", cmd.available);
            obj.field_str("description", cmd.description);
        });
        d.field_object("summary", |s| {
            s.field_raw(
                "readySubsystems",
                &report.ready_subsystem_count().to_string(),
            );
            s.field_raw("totalSubsystems", &report.subsystems.len().to_string());
            s.field_raw(
                "enabledFeatures",
                &report.enabled_feature_count().to_string(),
            );
            s.field_raw("totalFeatures", &report.features.len().to_string());
            s.field_raw(
                "availableCommands",
                &report.available_command_count().to_string(),
            );
            s.field_raw("totalCommands", &report.commands.len().to_string());
        });
    });
    b.finish()
}

/// Render a capabilities report as human-readable text.
#[must_use]
pub fn render_capabilities_human(report: &CapabilitiesReport) -> String {
    let mut output = format!("ee capabilities (v{})\n\n", report.version);

    output.push_str("Subsystems:\n");
    for sub in &report.subsystems {
        let icon = match sub.status {
            crate::models::CapabilityStatus::Ready => "✓",
            crate::models::CapabilityStatus::Pending => "◐",
            crate::models::CapabilityStatus::Degraded => "⚠",
            crate::models::CapabilityStatus::Unimplemented => "○",
        };
        output.push_str(&format!("  {} {} — {}\n", icon, sub.name, sub.description));
    }

    output.push_str("\nFeatures:\n");
    for feat in &report.features {
        let icon = if feat.enabled { "✓" } else { "○" };
        output.push_str(&format!(
            "  {} {} — {}\n",
            icon, feat.name, feat.description
        ));
    }

    output.push_str("\nCommands:\n");
    for cmd in &report.commands {
        let icon = if cmd.available { "✓" } else { "○" };
        output.push_str(&format!("  {} {} — {}\n", icon, cmd.name, cmd.description));
    }

    output.push_str(&format!(
        "\nSummary: {}/{} subsystems ready, {}/{} features enabled, {}/{} commands available\n",
        report.ready_subsystem_count(),
        report.subsystems.len(),
        report.enabled_feature_count(),
        report.features.len(),
        report.available_command_count(),
        report.commands.len()
    ));

    output.push_str("\nNext:\n  ee capabilities --json\n");
    output
}

/// Render a capabilities report as TOON.
#[must_use]
pub fn render_capabilities_toon(report: &CapabilitiesReport) -> String {
    render_toon_from_json(&render_capabilities_json(report))
}

/// Render evaluation run result as JSON (ee.response.v1 envelope).
///
/// This stub version is used when no report is available.
#[must_use]
pub fn render_eval_run_json(scenario_id: Option<&str>) -> String {
    render_eval_report_json(&EvaluationReport::new(), scenario_id)
}

/// Render evaluation report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_eval_report_json(report: &EvaluationReport, scenario_id: Option<&str>) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.status.is_success());
    b.field_object("data", |d| {
        d.field_str("command", "eval run");
        if let Some(id) = scenario_id {
            d.field_str("scenarioId", id);
        }
        d.field_str("status", report.status.as_str());
        d.field_raw("scenariosRun", &report.scenarios_run.to_string());
        d.field_raw("scenariosPassed", &report.scenarios_passed.to_string());
        d.field_raw("scenariosFailed", &report.scenarios_failed.to_string());
        d.field_raw("elapsedMs", &format!("{:.2}", report.elapsed_ms));
        if let Some(ref dir) = report.fixture_dir {
            d.field_str("fixtureDir", dir);
        }
        if report.status == EvaluationStatus::NoScenarios {
            d.field_str(
                "message",
                "No evaluation scenarios configured. Add fixtures to tests/fixtures/eval/.",
            );
        }
        d.field_array_of_objects("results", &report.results, render_scenario_result_json);
    });
    b.finish()
}

fn render_scenario_result_json(obj: &mut JsonBuilder, result: &ScenarioValidationResult) {
    obj.field_str("scenarioId", &result.scenario_id);
    obj.field_bool("passed", result.passed);
    obj.field_raw("stepsPassed", &result.steps_passed.to_string());
    obj.field_raw("stepsTotal", &result.steps_total.to_string());
    obj.field_array_of_objects("failures", &result.failures, |f, failure| {
        f.field_raw("step", &failure.step.to_string());
        f.field_str("kind", failure.kind.as_str());
        f.field_str("message", &failure.message);
    });
}

/// Render evaluation run result as human-readable text.
///
/// This stub version is used when no report is available.
#[must_use]
pub fn render_eval_run_human(scenario_id: Option<&str>) -> String {
    render_eval_report_human(&EvaluationReport::new(), scenario_id)
}

/// Render evaluation report as human-readable text.
#[must_use]
pub fn render_eval_report_human(report: &EvaluationReport, scenario_id: Option<&str>) -> String {
    let mut output = String::from("ee eval run\n\n");

    if let Some(id) = scenario_id {
        output.push_str(&format!("Scenario: {id}\n\n"));
    }

    let status_display = match report.status {
        EvaluationStatus::NoScenarios => "no scenarios available",
        EvaluationStatus::AllPassed => "all passed",
        EvaluationStatus::SomeFailed => "some failed",
        EvaluationStatus::AllFailed => "all failed",
    };
    output.push_str(&format!("Status: {status_display}\n"));
    output.push_str(&format!(
        "Results: {} run, {} passed, {} failed\n",
        report.scenarios_run, report.scenarios_passed, report.scenarios_failed
    ));
    output.push_str(&format!("Elapsed: {:.1}ms\n", report.elapsed_ms));

    if let Some(ref dir) = report.fixture_dir {
        output.push_str(&format!("Fixtures: {dir}\n"));
    }

    if report.status == EvaluationStatus::NoScenarios {
        output.push_str("\nNo evaluation scenarios configured.\n");
        output.push_str("Add fixtures to tests/fixtures/eval/ to define scenarios.\n");
    } else {
        output.push('\n');
        for result in &report.results {
            let icon = if result.passed { "[PASS]" } else { "[FAIL]" };
            output.push_str(&format!(
                "{icon} {}: {}/{} steps\n",
                result.scenario_id, result.steps_passed, result.steps_total
            ));
            for failure in &result.failures {
                output.push_str(&format!(
                    "  - Step {}: {} - {}\n",
                    failure.step,
                    failure.kind.as_str(),
                    failure.message
                ));
            }
        }
    }

    output
}

/// Render evaluation run result as TOON.
///
/// This stub version is used when no report is available.
#[must_use]
pub fn render_eval_run_toon(scenario_id: Option<&str>) -> String {
    render_eval_report_toon(&EvaluationReport::new(), scenario_id)
}

/// Render evaluation report as TOON.
#[must_use]
pub fn render_eval_report_toon(report: &EvaluationReport, scenario_id: Option<&str>) -> String {
    render_toon_from_json(&render_eval_report_json(report, scenario_id))
}

/// Render evaluation scenario list as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_eval_list_json() -> String {
    let mut b = JsonBuilder::with_capacity(256);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "eval list");
        d.field_raw("scenarios", "[]");
        d.field_str(
            "message",
            "No evaluation scenarios configured. Add fixtures to tests/fixtures/eval/.",
        );
    });
    b.finish()
}

/// Render evaluation scenario list as human-readable text.
#[must_use]
pub fn render_eval_list_human() -> String {
    let mut output = String::from("ee eval list\n\n");
    output.push_str("No evaluation scenarios configured.\n");
    output.push_str("Add fixtures to tests/fixtures/eval/ to define scenarios.\n");
    output
}

/// Render evaluation scenario list as TOON.
#[must_use]
pub fn render_eval_list_toon() -> String {
    render_toon_from_json(&render_eval_list_json())
}

/// Public schema entry for the schema registry.
#[derive(Clone, Debug)]
pub struct SchemaEntry {
    pub id: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub category: &'static str,
}

/// All public schemas exposed by ee.
pub const fn public_schemas() -> &'static [SchemaEntry] {
    &[
        SchemaEntry {
            id: "ee.response.v1",
            version: "1",
            description: "Success response envelope for all ee commands",
            category: "envelope",
        },
        SchemaEntry {
            id: "ee.error.v1",
            version: "1",
            description: "Error response envelope with code, message, and repair",
            category: "envelope",
        },
        SchemaEntry {
            id: "ee.certificate.v1",
            version: "1",
            description: "Certificate schemas for pack, curation, tail-risk, privacy-budget, and lifecycle",
            category: "domain",
        },
        SchemaEntry {
            id: "ee.executable_id_schemas.v1",
            version: "1",
            description: "Executable claim/evidence/policy/trace/demo ID schemas",
            category: "id",
        },
    ]
}

/// Render the schema list as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_schema_list_json() -> String {
    let schemas = public_schemas();
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "schema list");
        d.field_array_of_objects("schemas", schemas, |obj, entry| {
            obj.field_str("id", entry.id);
            obj.field_str("version", entry.version);
            obj.field_str("description", entry.description);
            obj.field_str("category", entry.category);
        });
    });
    b.finish()
}

/// Render the schema list as human-readable text.
#[must_use]
pub fn render_schema_list_human() -> String {
    let schemas = public_schemas();
    let mut output = String::from("ee schema list\n\nAvailable schemas:\n\n");
    for entry in schemas {
        output.push_str(&format!("  {} (v{})\n", entry.id, entry.version));
        output.push_str(&format!("    {}\n\n", entry.description));
    }
    output.push_str(
        "Use `ee schema export <SCHEMA_ID>` to export a schema's JSON Schema definition.\n",
    );
    output
}

/// Render the schema list as TOON.
#[must_use]
pub fn render_schema_list_toon() -> String {
    render_toon_from_json(&render_schema_list_json())
}

/// Render a schema export as JSON (full JSON Schema definition).
#[must_use]
pub fn render_schema_export_json(schema_id: Option<&str>) -> String {
    match schema_id {
        Some(id) => render_single_schema_export(id),
        None => render_all_schemas_export(),
    }
}

fn render_single_schema_export(schema_id: &str) -> String {
    match schema_id {
        "ee.response.v1" => response_schema_definition(),
        "ee.error.v1" => error_schema_definition(),
        "ee.certificate.v1" => certificate_schema_definition(),
        "ee.executable_id_schemas.v1" => crate::models::executable_id_schema_catalog_json(),
        _ => {
            let mut b = JsonBuilder::with_capacity(256);
            b.field_str("schema", ERROR_SCHEMA_V1);
            b.field_object("error", |e| {
                e.field_str("code", "schema_not_found");
                e.field_str("message", &format!("Schema '{}' not found", schema_id));
                e.field_str("repair", "ee schema list");
            });
            b.finish()
        }
    }
}

fn render_all_schemas_export() -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "schema export");
        d.field_raw(
            "schemas",
            &format!(
                "[{},{},{},{}]",
                response_schema_definition(),
                error_schema_definition(),
                certificate_schema_definition(),
                crate::models::executable_id_schema_catalog_json()
            ),
        );
    });
    b.finish()
}

fn response_schema_definition() -> String {
    r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"ee.response.v1","type":"object","required":["schema","success","data"],"properties":{"schema":{"const":"ee.response.v1"},"success":{"type":"boolean"},"data":{"type":"object"},"degraded":{"type":"array","items":{"type":"object","required":["code","severity","message","repair"],"properties":{"code":{"type":"string"},"severity":{"type":"string","enum":["low","medium","high"]},"message":{"type":"string"},"repair":{"type":"string"}}}}}}"#.to_string()
}

fn error_schema_definition() -> String {
    r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"ee.error.v1","type":"object","required":["schema","error"],"properties":{"schema":{"const":"ee.error.v1"},"error":{"type":"object","required":["code","message"],"properties":{"code":{"type":"string"},"message":{"type":"string"},"repair":{"type":"string"}}}}}"#.to_string()
}

fn certificate_schema_definition() -> String {
    r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"ee.certificate.v1","type":"object","required":["kind","status"],"properties":{"kind":{"type":"string","enum":["pack","curation","tail_risk","privacy_budget","lifecycle"]},"status":{"type":"string","enum":["pending","active","revoked","expired"]}}}"#.to_string()
}

/// Render a schema export as human-readable text.
#[must_use]
pub fn render_schema_export_human(schema_id: Option<&str>) -> String {
    let json = render_schema_export_json(schema_id);
    if json.contains("\"error\"") {
        String::from("error: Schema not found\n\nRun `ee schema list` to see available schemas.\n")
    } else {
        format!("ee schema export\n\n{}\n", json)
    }
}

/// Render a schema export as TOON.
#[must_use]
pub fn render_schema_export_toon(schema_id: Option<&str>) -> String {
    render_toon_from_json(&render_schema_export_json(schema_id))
}

fn render_toon_from_json(json: &str) -> String {
    toon::json_to_toon(json).unwrap_or_else(|error| {
        let message = escape_toon_quoted_string(&format!("TOON encoding failed: {error}"));
        format!(
            "schema: {ERROR_SCHEMA_V1}\nerror:\n  code: toon_encoding_failed\n  message: \"{message}\"\n"
        )
    })
}

fn escape_toon_quoted_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Legacy placeholder for backwards compatibility during transition.
#[must_use]
pub fn status_response_json() -> String {
    render_status_json(&StatusReport::gather())
}

/// Legacy placeholder for backwards compatibility during transition.
#[must_use]
pub fn human_status() -> String {
    render_status_human(&StatusReport::gather())
}

#[must_use]
pub fn help_text() -> &'static str {
    "ee - durable memory substrate for coding agents\n\nUsage:\n  ee status [--json]\n  ee --version\n  ee --help\n"
}

#[must_use]
pub fn schema_json() -> String {
    format!(
        "{{\"schema\":\"{}\",\"success\":true,\"data\":{{\"command\":\"schema\",\"schemas\":{{\"response\":\"{}\",\"error\":\"{}\"}}}}}}",
        RESPONSE_SCHEMA_V1, RESPONSE_SCHEMA_V1, ERROR_SCHEMA_V1
    )
}

#[must_use]
pub fn help_json() -> String {
    let mut b = JsonBuilder::with_capacity(4096);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "help");
        d.field_str("binary", "ee");
        d.field_str("version", env!("CARGO_PKG_VERSION"));
        d.field_str("usage", "ee [OPTIONS] [COMMAND]");
        d.field_str(
            "description",
            "Durable, local-first, explainable memory for coding agents.",
        );

        d.field_array_of_objects("globalOptions", GLOBAL_OPTIONS, |obj, opt| {
            obj.field_str("name", opt.name);
            obj.field_str("short", opt.short);
            obj.field_str("description", opt.description);
            obj.field_str("type", opt.opt_type);
        });

        d.field_array_of_objects("commands", COMMAND_MANIFEST, |obj, cmd| {
            obj.field_str("name", cmd.name);
            obj.field_str("description", cmd.description);
            obj.field_bool("available", cmd.available);
            if !cmd.subcommands.is_empty() {
                obj.field_array_of_objects("subcommands", cmd.subcommands, |sub, sc| {
                    sub.field_str("name", sc.name);
                    sub.field_str("description", sc.description);
                });
            }
            if !cmd.args.is_empty() {
                obj.field_array_of_objects("args", cmd.args, |arg, a| {
                    arg.field_str("name", a.name);
                    arg.field_str("description", a.description);
                    arg.field_bool("required", a.required);
                    if let Some(def) = a.default {
                        arg.field_str("default", def);
                    }
                });
            }
        });
    });
    b.finish()
}

struct GlobalOption {
    name: &'static str,
    short: &'static str,
    description: &'static str,
    opt_type: &'static str,
}

const GLOBAL_OPTIONS: &[GlobalOption] = &[
    GlobalOption {
        name: "--json",
        short: "-j",
        description: "Emit JSON output",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--workspace",
        short: "",
        description: "Workspace root to operate on",
        opt_type: "path",
    },
    GlobalOption {
        name: "--no-color",
        short: "",
        description: "Disable colored diagnostics",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--robot",
        short: "",
        description: "Use agent-oriented output defaults",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--format",
        short: "",
        description: "Select output renderer (human|json|toon|jsonl|compact|hook)",
        opt_type: "enum",
    },
    GlobalOption {
        name: "--fields",
        short: "",
        description: "Control output verbosity (minimal|summary|standard|full)",
        opt_type: "enum",
    },
    GlobalOption {
        name: "--schema",
        short: "",
        description: "Print JSON schema for response envelope",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--help-json",
        short: "",
        description: "Print JSON-formatted help",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--agent-docs",
        short: "",
        description: "Print agent-oriented documentation",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--meta",
        short: "",
        description: "Include additional metadata in response",
        opt_type: "flag",
    },
];

struct CommandArg {
    name: &'static str,
    description: &'static str,
    required: bool,
    default: Option<&'static str>,
}

struct SubcommandEntry {
    name: &'static str,
    description: &'static str,
}

struct CommandEntry {
    name: &'static str,
    description: &'static str,
    available: bool,
    subcommands: &'static [SubcommandEntry],
    args: &'static [CommandArg],
}

const COMMAND_MANIFEST: &[CommandEntry] = &[
    CommandEntry {
        name: "agent-docs",
        description: "Agent-oriented documentation for ee commands, contracts, and usage",
        available: true,
        subcommands: &[],
        args: &[CommandArg {
            name: "TOPIC",
            description: "Documentation topic (guide, commands, contracts, schemas, paths, env, exit-codes, fields, errors, formats, examples)",
            required: false,
            default: None,
        }],
    },
    CommandEntry {
        name: "capabilities",
        description: "Report feature availability, commands, and subsystem status",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "check",
        description: "Quick posture summary: ready, degraded, or needs attention",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "diag",
        description: "Run diagnostic commands for trust, quarantine, and streams",
        available: true,
        subcommands: &[SubcommandEntry {
            name: "quarantine",
            description: "Report quarantine status for import sources",
        }],
        args: &[],
    },
    CommandEntry {
        name: "doctor",
        description: "Run health checks on workspace and subsystems",
        available: true,
        subcommands: &[],
        args: &[CommandArg {
            name: "--fix-plan",
            description: "Output structured repair plan",
            required: false,
            default: None,
        }],
    },
    CommandEntry {
        name: "eval",
        description: "Run evaluation scenarios against fixtures",
        available: true,
        subcommands: &[
            SubcommandEntry {
                name: "run",
                description: "Run one or more evaluation scenarios",
            },
            SubcommandEntry {
                name: "list",
                description: "List available evaluation scenarios",
            },
        ],
        args: &[],
    },
    CommandEntry {
        name: "health",
        description: "Quick health check with overall verdict",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "help",
        description: "Print command help",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "import",
        description: "Import memories and evidence from external sources",
        available: true,
        subcommands: &[SubcommandEntry {
            name: "cass",
            description: "Import from coding_agent_session_search",
        }],
        args: &[],
    },
    CommandEntry {
        name: "index",
        description: "Manage search indexes",
        available: true,
        subcommands: &[
            SubcommandEntry {
                name: "rebuild",
                description: "Rebuild the search index",
            },
            SubcommandEntry {
                name: "status",
                description: "Inspect index health and generation",
            },
        ],
        args: &[],
    },
    CommandEntry {
        name: "remember",
        description: "Store a new memory",
        available: true,
        subcommands: &[],
        args: &[
            CommandArg {
                name: "CONTENT",
                description: "Memory content to store",
                required: true,
                default: None,
            },
            CommandArg {
                name: "--level",
                description: "Memory level",
                required: false,
                default: Some("episodic"),
            },
            CommandArg {
                name: "--kind",
                description: "Memory kind",
                required: false,
                default: Some("fact"),
            },
            CommandArg {
                name: "--tags",
                description: "Tags (comma-separated)",
                required: false,
                default: None,
            },
            CommandArg {
                name: "--confidence",
                description: "Confidence score (0.0-1.0)",
                required: false,
                default: Some("0.8"),
            },
            CommandArg {
                name: "--source",
                description: "Source provenance URI",
                required: false,
                default: None,
            },
            CommandArg {
                name: "--dry-run",
                description: "Perform dry run without storing",
                required: false,
                default: None,
            },
        ],
    },
    CommandEntry {
        name: "schema",
        description: "List or export public response schemas",
        available: true,
        subcommands: &[
            SubcommandEntry {
                name: "list",
                description: "List all available public schemas",
            },
            SubcommandEntry {
                name: "export",
                description: "Export schema JSON definition",
            },
        ],
        args: &[],
    },
    CommandEntry {
        name: "search",
        description: "Search indexed memories and sessions",
        available: true,
        subcommands: &[],
        args: &[
            CommandArg {
                name: "QUERY",
                description: "Query string to search for",
                required: true,
                default: None,
            },
            CommandArg {
                name: "--limit",
                description: "Maximum results",
                required: false,
                default: Some("10"),
            },
            CommandArg {
                name: "--database",
                description: "Database path",
                required: false,
                default: None,
            },
            CommandArg {
                name: "--index-dir",
                description: "Index directory path",
                required: false,
                default: None,
            },
        ],
    },
    CommandEntry {
        name: "status",
        description: "Report workspace and subsystem readiness",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "version",
        description: "Print the ee version",
        available: true,
        subcommands: &[],
        args: &[],
    },
];

#[must_use]
pub fn render_introspect_json() -> String {
    let mut b = JsonBuilder::with_capacity(8192);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "introspect");
        d.field_str("version", env!("CARGO_PKG_VERSION"));

        d.field_object("commands", |c| {
            for cmd in COMMAND_MANIFEST {
                c.field_object(cmd.name, |obj| {
                    obj.field_str("description", cmd.description);
                    obj.field_bool("available", cmd.available);
                    if !cmd.subcommands.is_empty() {
                        obj.field_array_of_objects("subcommands", cmd.subcommands, |sub, sc| {
                            sub.field_str("name", sc.name);
                            sub.field_str("description", sc.description);
                        });
                    }
                    if !cmd.args.is_empty() {
                        obj.field_raw("argCount", &cmd.args.len().to_string());
                    }
                });
            }
        });

        d.field_object("schemas", |s| {
            for schema in public_schemas() {
                s.field_object(schema.id, |obj| {
                    obj.field_str("version", schema.version);
                    obj.field_str("description", schema.description);
                    obj.field_str("category", schema.category);
                });
            }
        });

        d.field_object("errorCodes", |e| {
            for code in ERROR_CODES {
                e.field_object(code.code, |obj| {
                    obj.field_str("message", code.message);
                    obj.field_str("repair", code.repair);
                    obj.field_str("category", code.category);
                });
            }
        });

        d.field_object("globalOptions", |g| {
            for opt in GLOBAL_OPTIONS {
                g.field_object(opt.name, |obj| {
                    if !opt.short.is_empty() {
                        obj.field_str("short", opt.short);
                    }
                    obj.field_str("description", opt.description);
                    obj.field_str("type", opt.opt_type);
                });
            }
        });
    });
    b.finish()
}

#[must_use]
pub fn render_introspect_human() -> String {
    let mut output = format!("ee introspect (v{})\n\n", env!("CARGO_PKG_VERSION"));

    output.push_str("Commands:\n");
    for cmd in COMMAND_MANIFEST {
        let status = if cmd.available { "✓" } else { "○" };
        output.push_str(&format!(
            "  {} {} — {}\n",
            status, cmd.name, cmd.description
        ));
    }

    output.push_str("\nSchemas:\n");
    for schema in public_schemas() {
        output.push_str(&format!(
            "  {} (v{}) — {}\n",
            schema.id, schema.version, schema.description
        ));
    }

    output.push_str("\nError Codes:\n");
    for code in ERROR_CODES {
        output.push_str(&format!("  {} — {}\n", code.code, code.message));
    }

    output.push_str("\nNext:\n  ee introspect --json\n");
    output
}

#[must_use]
pub fn render_introspect_toon() -> String {
    render_toon_from_json(&render_introspect_json())
}

struct ErrorCodeEntry {
    code: &'static str,
    message: &'static str,
    repair: &'static str,
    category: &'static str,
}

const ERROR_CODES: &[ErrorCodeEntry] = &[
    ErrorCodeEntry {
        code: "usage",
        message: "Invalid command usage",
        repair: "ee --help",
        category: "cli",
    },
    ErrorCodeEntry {
        code: "config",
        message: "Configuration error",
        repair: "ee doctor",
        category: "config",
    },
    ErrorCodeEntry {
        code: "storage",
        message: "Storage operation failed",
        repair: "ee doctor --fix-plan",
        category: "storage",
    },
    ErrorCodeEntry {
        code: "search_index",
        message: "Search index error",
        repair: "ee index rebuild",
        category: "search",
    },
    ErrorCodeEntry {
        code: "import",
        message: "Import operation failed",
        repair: "ee import cass --dry-run",
        category: "import",
    },
    ErrorCodeEntry {
        code: "degraded",
        message: "Required capability is degraded",
        repair: "ee status --json",
        category: "degraded",
    },
    ErrorCodeEntry {
        code: "policy",
        message: "Operation denied by policy",
        repair: "ee capabilities --json",
        category: "policy",
    },
    ErrorCodeEntry {
        code: "migration",
        message: "Migration required",
        repair: "ee doctor --fix-plan",
        category: "storage",
    },
];

#[must_use]
pub fn agent_docs() -> String {
    format!(
        "{{\"schema\":\"{}\",\"success\":true,\"data\":{{\"command\":\"agent-docs\",\"description\":\"Durable, local-first, explainable memory for coding agents.\",\"primaryWorkflow\":\"ee context \\\"<task>\\\" --workspace . --max-tokens 4000 --json\",\"coreCommands\":[\"init\",\"remember\",\"search\",\"context\",\"why\",\"status\"]}}}}",
        RESPONSE_SCHEMA_V1
    )
}

use crate::core::agent_docs::{
    AgentDocsReport, AgentDocsTopic, CONTRACTS, DEFAULT_PATHS, ENV_VARS, EXAMPLES, EXIT_CODES,
    FIELD_LEVELS, GUIDE_SECTIONS, OUTPUT_FORMATS,
};

#[must_use]
pub fn render_agent_docs_json(report: &AgentDocsReport) -> String {
    let mut b = JsonBuilder::with_capacity(4096);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "agent-docs");
        d.field_str("version", report.version);

        if let Some(topic) = report.topic {
            d.field_str("topic", topic.as_str());
            render_agent_docs_topic_json(d, topic);
        } else {
            d.field_str("topic", "overview");
            d.field_str(
                "description",
                "Durable, local-first, explainable memory for coding agents.",
            );
            d.field_str(
                "primaryWorkflow",
                "ee context \"<task>\" --workspace . --max-tokens 4000 --json",
            );
            d.field_array_of_objects("topics", AgentDocsTopic::all(), |obj, topic| {
                obj.field_str("name", topic.as_str());
                obj.field_str("description", topic.description());
            });
        }
    });
    b.finish()
}

fn render_agent_docs_topic_json(d: &mut JsonBuilder, topic: AgentDocsTopic) {
    match topic {
        AgentDocsTopic::Guide => {
            d.field_array_of_objects("sections", GUIDE_SECTIONS, |obj, section| {
                obj.field_str("title", section.title);
                obj.field_str("content", section.content);
            });
        }
        AgentDocsTopic::Commands => {
            d.field_array_of_objects("commands", COMMAND_MANIFEST, |obj, cmd| {
                obj.field_str("name", cmd.name);
                obj.field_str("description", cmd.description);
                obj.field_bool("available", cmd.available);
                if !cmd.subcommands.is_empty() {
                    obj.field_array_of_objects("subcommands", cmd.subcommands, |sub, sc| {
                        sub.field_str("name", sc.name);
                        sub.field_str("description", sc.description);
                    });
                }
                if !cmd.args.is_empty() {
                    obj.field_array_of_objects("args", cmd.args, |arg, a| {
                        arg.field_str("name", a.name);
                        arg.field_str("description", a.description);
                        arg.field_bool("required", a.required);
                        if let Some(def) = a.default {
                            arg.field_str("default", def);
                        }
                    });
                }
            });
        }
        AgentDocsTopic::Contracts => {
            d.field_array_of_objects("contracts", CONTRACTS, |obj, contract| {
                obj.field_str("name", contract.name);
                obj.field_str("schema", contract.schema);
                obj.field_str("description", contract.description);
                obj.field_str("stability", contract.stability);
            });
        }
        AgentDocsTopic::Schemas => {
            let schemas = public_schemas();
            d.field_array_of_objects("schemas", schemas, |obj, schema| {
                obj.field_str("id", schema.id);
                obj.field_str("version", schema.version);
                obj.field_str("description", schema.description);
                obj.field_str("category", schema.category);
            });
        }
        AgentDocsTopic::Paths => {
            d.field_array_of_objects("paths", DEFAULT_PATHS, |obj, path| {
                obj.field_str("name", path.name);
                obj.field_str("default", path.default);
                obj.field_str("description", path.description);
                if let Some(env) = path.env_override {
                    obj.field_str("envOverride", env);
                }
            });
        }
        AgentDocsTopic::Env => {
            d.field_array_of_objects("envVars", ENV_VARS, |obj, var| {
                obj.field_str("name", var.name);
                obj.field_str("description", var.description);
                obj.field_str("category", var.category);
                if let Some(def) = var.default {
                    obj.field_str("default", def);
                }
            });
        }
        AgentDocsTopic::ExitCodes => {
            d.field_array_of_objects("exitCodes", EXIT_CODES, |obj, code| {
                obj.field_raw("code", &code.code.to_string());
                obj.field_str("name", code.name);
                obj.field_str("description", code.description);
            });
        }
        AgentDocsTopic::Fields => {
            d.field_array_of_objects("fieldLevels", FIELD_LEVELS, |obj, level| {
                obj.field_str("name", level.name);
                obj.field_str("flag", level.flag);
                obj.field_str("includes", level.includes);
                obj.field_str("useCase", level.use_case);
            });
        }
        AgentDocsTopic::Errors => {
            d.field_array_of_objects("errorCodes", ERROR_CODES, |obj, code| {
                obj.field_str("code", code.code);
                obj.field_str("message", code.message);
                obj.field_str("repair", code.repair);
                obj.field_str("category", code.category);
            });
        }
        AgentDocsTopic::Formats => {
            d.field_array_of_objects("formats", OUTPUT_FORMATS, |obj, fmt| {
                obj.field_str("name", fmt.name);
                obj.field_str("flag", fmt.flag);
                obj.field_str("description", fmt.description);
                obj.field_bool("machineReadable", fmt.machine_readable);
            });
        }
        AgentDocsTopic::Examples => {
            d.field_array_of_objects("examples", EXAMPLES, |obj, example| {
                obj.field_str("title", example.title);
                obj.field_str("description", example.description);
                obj.field_str("command", example.command);
                obj.field_str("category", example.category);
            });
        }
    }
}

#[must_use]
pub fn render_agent_docs_human(report: &AgentDocsReport) -> String {
    let mut output = String::with_capacity(2048);
    output.push_str("ee agent-docs");
    if let Some(topic) = report.topic {
        output.push(' ');
        output.push_str(topic.as_str());
    }
    output.push('\n');
    output.push_str(&"-".repeat(40));
    output.push('\n');

    if let Some(topic) = report.topic {
        render_agent_docs_topic_human(&mut output, topic);
    } else {
        output.push_str("\nDurable, local-first, explainable memory for coding agents.\n\n");
        output.push_str(
            "Primary workflow:\n  ee context \"<task>\" --workspace . --max-tokens 4000 --json\n\n",
        );
        output.push_str("Available topics:\n");
        for t in AgentDocsTopic::all() {
            output.push_str(&format!("  {:12} {}\n", t.as_str(), t.description()));
        }
        output.push_str("\nRun `ee agent-docs <topic>` for details.\n");
    }

    output
}

fn render_agent_docs_topic_human(output: &mut String, topic: AgentDocsTopic) {
    match topic {
        AgentDocsTopic::Guide => {
            for section in GUIDE_SECTIONS {
                output.push_str(&format!("\n{}:\n  {}\n", section.title, section.content));
            }
        }
        AgentDocsTopic::Commands => {
            output.push_str("\nAvailable commands:\n");
            for cmd in COMMAND_MANIFEST {
                let status = if cmd.available { "" } else { " (unavailable)" };
                output.push_str(&format!(
                    "  {:16} {}{}\n",
                    cmd.name, cmd.description, status
                ));
                for sub in cmd.subcommands {
                    output.push_str(&format!("    {:14} {}\n", sub.name, sub.description));
                }
            }
        }
        AgentDocsTopic::Contracts => {
            output.push_str("\nStable output contracts:\n");
            for contract in CONTRACTS {
                output.push_str(&format!(
                    "  {:12} {} ({})\n    {}\n",
                    contract.name, contract.schema, contract.stability, contract.description
                ));
            }
        }
        AgentDocsTopic::Schemas => {
            output.push_str("\nPublic schemas:\n");
            for schema in public_schemas() {
                output.push_str(&format!(
                    "  {:30} v{} [{}]\n    {}\n",
                    schema.id, schema.version, schema.category, schema.description
                ));
            }
        }
        AgentDocsTopic::Paths => {
            output.push_str("\nDefault paths:\n");
            for path in DEFAULT_PATHS {
                output.push_str(&format!("  {:14} {}\n", path.name, path.default));
                output.push_str(&format!("    {}\n", path.description));
                if let Some(env) = path.env_override {
                    output.push_str(&format!("    Override: {}\n", env));
                }
            }
        }
        AgentDocsTopic::Env => {
            output.push_str("\nEnvironment variables:\n");
            for var in ENV_VARS {
                let def = var
                    .default
                    .map_or(String::new(), |d| format!(" (default: {})", d));
                output.push_str(&format!(
                    "  {:20}{}\n    {}\n",
                    var.name, def, var.description
                ));
            }
        }
        AgentDocsTopic::ExitCodes => {
            output.push_str("\nExit codes:\n");
            for code in EXIT_CODES {
                output.push_str(&format!(
                    "  {:3} {:16} {}\n",
                    code.code, code.name, code.description
                ));
            }
        }
        AgentDocsTopic::Fields => {
            output.push_str("\nField profile levels:\n");
            for level in FIELD_LEVELS {
                output.push_str(&format!("  {:10} {}\n", level.name, level.flag));
                output.push_str(&format!("    Includes: {}\n", level.includes));
                output.push_str(&format!("    Use case: {}\n", level.use_case));
            }
        }
        AgentDocsTopic::Errors => {
            output.push_str("\nError codes:\n");
            for code in ERROR_CODES {
                output.push_str(&format!("  {:16} [{}]\n", code.code, code.category));
                output.push_str(&format!("    {}\n", code.message));
                output.push_str(&format!("    Repair: {}\n", code.repair));
            }
        }
        AgentDocsTopic::Formats => {
            output.push_str("\nOutput formats:\n");
            for fmt in OUTPUT_FORMATS {
                let machine = if fmt.machine_readable {
                    " [machine]"
                } else {
                    ""
                };
                output.push_str(&format!("  {:10}{}\n", fmt.name, machine));
                output.push_str(&format!("    Flag: {}\n", fmt.flag));
                output.push_str(&format!("    {}\n", fmt.description));
            }
        }
        AgentDocsTopic::Examples => {
            output.push_str("\nCommon examples:\n");
            for example in EXAMPLES {
                output.push_str(&format!("\n  {} [{}]\n", example.title, example.category));
                output.push_str(&format!("    {}\n", example.description));
                output.push_str(&format!("    $ {}\n", example.command));
            }
        }
    }
}

#[must_use]
pub fn render_agent_docs_toon(report: &AgentDocsReport) -> String {
    render_toon_from_json(&render_agent_docs_json(report))
}

#[must_use]
pub fn error_response_json(error: &DomainError) -> String {
    let code = error.code();
    let message = escape_json_string(&error.message());
    match error.repair() {
        Some(repair) => {
            let repair = escape_json_string(repair);
            format!(
                "{{\"schema\":\"{schema}\",\"error\":{{\"code\":\"{code}\",\"message\":\"{message}\",\"repair\":\"{repair}\"}}}}",
                schema = ERROR_SCHEMA_V1
            )
        }
        None => {
            format!(
                "{{\"schema\":\"{schema}\",\"error\":{{\"code\":\"{code}\",\"message\":\"{message}\"}}}}",
                schema = ERROR_SCHEMA_V1
            )
        }
    }
}

pub fn escape_json_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => {
                result.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => result.push(c),
        }
    }
    result
}

// ============================================================================
// Field Profile Filtered Renderers (EE-037)
//
// These functions respect the `FieldProfile` setting to control output
// verbosity. Each level progressively includes more fields:
// - minimal: command, version, status only
// - summary: + top-level metrics and summary counts
// - standard: + arrays with items (default behavior)
// - full: + verbose details like provenance, why, debug info
// ============================================================================

/// Render a status report as JSON with field filtering.
#[must_use]
pub fn render_status_json_filtered(report: &StatusReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "status");
        d.field_str("version", report.version);

        if profile.include_summary_metrics() {
            d.field_object("capabilities", |c| {
                c.field_str("runtime", report.capabilities.runtime.as_str());
                c.field_str("storage", report.capabilities.storage.as_str());
                c.field_str("search", report.capabilities.search.as_str());
            });
        }

        if profile.include_arrays() {
            d.field_object("runtime", |r| {
                r.field_str("engine", report.runtime.engine);
                r.field_str("profile", report.runtime.profile);
                r.field_raw("workerThreads", &report.runtime.worker_threads.to_string());
                r.field_str("asyncBoundary", report.runtime.async_boundary);
            });
            render_memory_health_json(d, &report.memory_health);
            d.field_array_of_objects("degraded", &report.degradations, |obj, deg| {
                obj.field_str("code", deg.code);
                obj.field_str("severity", deg.severity);
                obj.field_str("message", deg.message);
                if profile.include_verbose_details() {
                    obj.field_str("repair", deg.repair);
                }
            });
        }
    });
    b.finish()
}

/// Render a capabilities report as JSON with field filtering.
#[must_use]
pub fn render_capabilities_json_filtered(
    report: &CapabilitiesReport,
    profile: FieldProfile,
) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "capabilities");
        d.field_str("version", report.version);

        if profile.include_arrays() {
            d.field_array_of_objects("subsystems", &report.subsystems, |obj, sub| {
                obj.field_str("name", sub.name);
                obj.field_str("status", sub.status.as_str());
                if profile.include_verbose_details() {
                    obj.field_str("description", sub.description);
                }
            });
            d.field_array_of_objects("features", &report.features, |obj, feat| {
                obj.field_str("name", feat.name);
                obj.field_bool("enabled", feat.enabled);
                if profile.include_verbose_details() {
                    obj.field_str("description", feat.description);
                }
            });
            d.field_array_of_objects("commands", &report.commands, |obj, cmd| {
                obj.field_str("name", cmd.name);
                obj.field_bool("available", cmd.available);
                if profile.include_verbose_details() {
                    obj.field_str("description", cmd.description);
                }
            });
        }

        if profile.include_summary_metrics() {
            d.field_object("summary", |s| {
                s.field_raw(
                    "readySubsystems",
                    &report.ready_subsystem_count().to_string(),
                );
                s.field_raw("totalSubsystems", &report.subsystems.len().to_string());
                s.field_raw(
                    "enabledFeatures",
                    &report.enabled_feature_count().to_string(),
                );
                s.field_raw("totalFeatures", &report.features.len().to_string());
                s.field_raw(
                    "availableCommands",
                    &report.available_command_count().to_string(),
                );
                s.field_raw("totalCommands", &report.commands.len().to_string());
            });
        }
    });
    b.finish()
}

/// Render a doctor report as JSON with field filtering.
#[must_use]
pub fn render_doctor_json_filtered(report: &DoctorReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.overall_healthy);
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "doctor");
        d.field_str("version", report.version);
        d.field_bool("healthy", report.overall_healthy);

        if profile.include_arrays() {
            d.field_array_of_objects("checks", &report.checks, |obj, check| {
                obj.field_str("name", check.name);
                obj.field_str("severity", check.severity.as_str());
                if profile.include_summary_metrics() {
                    obj.field_str("message", &check.message);
                }
                if profile.include_verbose_details() {
                    if let Some(code) = check.error_code {
                        obj.field_str("errorCode", code.id);
                    }
                    if let Some(repair) = check.repair {
                        obj.field_str("repair", repair);
                    }
                }
            });
        }
    });
    b.finish()
}

/// Render a health report as JSON with field filtering.
#[must_use]
pub fn render_health_json_filtered(report: &HealthReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.verdict.is_healthy());
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "health");
        d.field_str("version", report.version);
        d.field_str("verdict", report.verdict.as_str());

        if profile.include_summary_metrics() {
            d.field_object("subsystems", |s| {
                s.field_bool("runtime", report.runtime_ok);
                s.field_bool("storage", report.storage_ok);
                s.field_bool("search", report.search_ok);
            });
            d.field_object("summary", |s| {
                s.field_raw("issueCount", &report.issue_count().to_string());
                s.field_raw("highSeverity", &report.high_severity_count().to_string());
                s.field_raw(
                    "mediumSeverity",
                    &report.medium_severity_count().to_string(),
                );
            });
        }

        if profile.include_arrays() {
            d.field_array_of_objects("issues", &report.issues, |obj, issue| {
                obj.field_str("subsystem", issue.subsystem);
                obj.field_str("code", issue.code);
                obj.field_str("severity", issue.severity);
                if profile.include_verbose_details() {
                    obj.field_str("message", issue.message);
                }
            });
        }
    });
    b.finish()
}

/// Render a check report as JSON with field filtering.
#[must_use]
pub fn render_check_json_filtered(report: &CheckReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.posture.is_usable());
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "check");
        d.field_str("version", report.version);
        d.field_str("posture", report.posture.as_str());

        if profile.include_summary_metrics() {
            d.field_bool("workspaceInitialized", report.workspace_initialized);
            d.field_bool("databaseReady", report.database_ready);
            d.field_bool("searchReady", report.search_ready);
            d.field_bool("runtimeReady", report.runtime_ready);
        }

        if profile.include_arrays() {
            d.field_array_of_objects(
                "suggestedActions",
                &report.suggested_actions,
                |obj, action| {
                    obj.field_raw("priority", &action.priority.to_string());
                    obj.field_str("command", action.command);
                    if profile.include_verbose_details() {
                        obj.field_str("reason", action.reason);
                    }
                },
            );
        }
    });
    b.finish()
}

/// Render a quarantine report as JSON with field filtering.
#[must_use]
pub fn render_quarantine_json_filtered(report: &QuarantineReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "diag quarantine");
        d.field_str("version", report.version);

        if profile.include_summary_metrics() {
            d.field_object("summary", |s| {
                s.field_raw(
                    "quarantinedCount",
                    &report.summary.quarantined_count.to_string(),
                );
                s.field_raw("atRiskCount", &report.summary.at_risk_count.to_string());
                s.field_raw("blockedCount", &report.summary.blocked_count.to_string());
                s.field_raw("totalSources", &report.summary.total_sources.to_string());
                s.field_raw("healthyCount", &report.summary.healthy_count.to_string());
            });
        }

        if profile.include_arrays() {
            let build_entry = |obj: &mut JsonBuilder, entry: &QuarantineEntry| {
                obj.field_str("sourceId", &entry.source_id);
                obj.field_str("advisory", entry.advisory.as_str());
                obj.field_raw("effectiveTrust", &format!("{:.4}", entry.effective_trust));
                if profile.include_verbose_details() {
                    obj.field_raw("decayFactor", &format!("{:.4}", entry.decay_factor));
                    obj.field_raw("negativeRate", &format!("{:.4}", entry.negative_rate));
                    obj.field_raw("negativeCount", &entry.negative_count.to_string());
                    obj.field_raw("totalImports", &entry.total_imports.to_string());
                    obj.field_str("message", &entry.message);
                    obj.field_bool("permitsImport", entry.permits_import);
                    obj.field_bool("requiresValidation", entry.requires_validation);
                }
            };
            d.field_array_of_objects(
                "quarantinedSources",
                &report.quarantined_sources,
                build_entry,
            );
            d.field_array_of_objects("atRiskSources", &report.at_risk_sources, build_entry);
            d.field_array_of_objects("blockedSources", &report.blocked_sources, build_entry);
        }
    });
    b.finish()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use uuid::Uuid;

    use super::{
        Degradation, DegradationSeverity, JsonBuilder, OutputContext, Renderer, ResponseEnvelope,
        error_response_json, escape_json_string, help_text, human_status, render_agent_docs_json,
        render_agent_docs_toon, render_context_response_json, render_context_response_toon,
        render_doctor_json, render_doctor_toon, render_health_json, render_health_toon,
        render_status_json, render_status_toon, status_response_json,
    };
    use crate::core::agent_docs::AgentDocsReport;
    use crate::core::doctor::DoctorReport;
    use crate::core::health::HealthReport;
    use crate::core::status::StatusReport;
    use crate::models::{DomainError, MemoryId, ProvenanceUri, UnitScore};
    use crate::pack::{
        ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
        PackSection, TokenBudget, assemble_draft,
    };

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
        ensure(
            haystack.contains(needle),
            format!("{context}: expected output to contain {needle:?}, got {haystack:?}"),
        )
    }

    fn ensure_starts_with(haystack: &str, prefix: &str, context: &str) -> TestResult {
        ensure(
            haystack.starts_with(prefix),
            format!("{context}: expected output to start with {prefix:?}, got {haystack:?}"),
        )
    }

    fn memory_id(seed: u128) -> MemoryId {
        MemoryId::from_uuid(Uuid::from_u128(seed))
    }

    fn score(value: f32) -> Result<UnitScore, String> {
        UnitScore::parse(value).map_err(|error| format!("test score rejected: {error:?}"))
    }

    fn pack_provenance(uri: &str) -> Result<PackProvenance, String> {
        let uri = ProvenanceUri::from_str(uri)
            .map_err(|error| format!("test provenance URI rejected: {error:?}"))?;
        PackProvenance::new(uri, "source evidence")
            .map_err(|error| format!("test provenance rejected: {error:?}"))
    }

    fn context_response_fixture() -> Result<ContextResponse, String> {
        let request = ContextRequest::from_query("prepare release")
            .map_err(|error| format!("request rejected: {error:?}"))?;
        let budget =
            TokenBudget::new(100).map_err(|error| format!("budget rejected: {error:?}"))?;
        let candidate = PackCandidate::new(PackCandidateInput {
            memory_id: memory_id(42),
            section: PackSection::ProceduralRules,
            content: "Run cargo fmt --check before release.".to_string(),
            estimated_tokens: 10,
            relevance: score(0.8)?,
            utility: score(0.6)?,
            provenance: vec![pack_provenance("file://AGENTS.md#L42")?],
            why: "selected because release checks match the task".to_string(),
        })
        .map_err(|error| format!("candidate rejected: {error:?}"))?;
        let draft = assemble_draft(&request.query, budget, vec![candidate])
            .map_err(|error| format!("draft rejected: {error:?}"))?;
        ContextResponse::new(request, draft, Vec::new())
            .map_err(|error| format!("response rejected: {error:?}"))
    }

    #[test]
    fn status_json_has_stable_schema_and_degradation_codes() -> TestResult {
        let json = status_response_json();
        ensure_starts_with(&json, "{\"schema\":\"ee.response.v1\"", "status schema")?;
        ensure_contains(&json, "\"success\":true", "status success flag")?;
        ensure_contains(&json, "\"runtime\":\"ready\"", "status runtime capability")?;
        ensure_contains(&json, "\"engine\":\"asupersync\"", "status runtime engine")?;
        ensure_contains(
            &json,
            "\"profile\":\"current_thread\"",
            "status runtime profile",
        )?;
        ensure_contains(
            &json,
            "\"storage_not_implemented\"",
            "status storage degradation",
        )?;
        ensure_contains(
            &json,
            "\"search_not_implemented\"",
            "status search degradation",
        )
    }

    #[test]
    fn human_status_is_not_json() -> TestResult {
        let status = human_status();
        ensure_starts_with(&status, "ee status", "human status heading")?;
        ensure(!status.starts_with('{'), "human status must not be JSON")
    }

    #[test]
    fn help_mentions_supported_skeleton_commands() -> TestResult {
        let help = help_text();
        ensure_contains(help, "ee status [--json]", "help status command")?;
        ensure_contains(help, "ee --version", "help version command")
    }

    #[test]
    fn error_json_has_stable_schema_and_code() -> TestResult {
        let error = DomainError::Usage {
            message: "unrecognized subcommand 'foo'".to_string(),
            repair: Some("ee --help".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "error schema")?;
        ensure_contains(&json, "\"code\":\"usage\"", "error code")?;
        ensure_contains(
            &json,
            "\"message\":\"unrecognized subcommand 'foo'\"",
            "error message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee --help\"", "error repair")
    }

    #[test]
    fn error_json_without_repair_omits_field() -> TestResult {
        let error = DomainError::Storage {
            message: "Database locked".to_string(),
            repair: None,
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "error schema")?;
        ensure_contains(&json, "\"code\":\"storage\"", "error code")?;
        ensure(!json.contains("repair"), "repair field should be absent")
    }

    #[test]
    fn escape_json_handles_special_chars() -> TestResult {
        let escaped = escape_json_string("line1\nline2\ttab\"quote\\backslash");
        ensure_contains(&escaped, "\\n", "newline escape")?;
        ensure_contains(&escaped, "\\t", "tab escape")?;
        ensure_contains(&escaped, "\\\"", "quote escape")?;
        ensure_contains(&escaped, "\\\\", "backslash escape")
    }

    #[test]
    fn json_builder_constructs_simple_object() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_str("name", "test");
        b.field_bool("active", true);
        b.field_u32("count", 42);
        let json = b.finish();
        ensure_contains(&json, "\"name\":\"test\"", "string field")?;
        ensure_contains(&json, "\"active\":true", "bool field")?;
        ensure_contains(&json, "\"count\":42", "u32 field")?;
        ensure(
            json.starts_with('{') && json.ends_with('}'),
            "valid JSON object",
        )
    }

    #[test]
    fn json_builder_escapes_string_values() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_str("message", "line1\nline2");
        let json = b.finish();
        ensure_contains(&json, "\"message\":\"line1\\nline2\"", "escaped newline")
    }

    #[test]
    fn json_builder_supports_nested_objects() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_str("schema", "test.v1");
        b.field_object("data", |obj| {
            obj.field_str("inner", "value");
        });
        let json = b.finish();
        ensure_contains(&json, "\"schema\":\"test.v1\"", "outer field")?;
        ensure_contains(&json, "\"data\":{\"inner\":\"value\"}", "nested object")
    }

    #[test]
    fn json_builder_supports_array_of_objects() -> TestResult {
        let items = vec![("a", 1u32), ("b", 2u32)];
        let mut b = JsonBuilder::new();
        b.field_array_of_objects("items", &items, |obj, (name, val)| {
            obj.field_str("name", name);
            obj.field_u32("value", *val);
        });
        let json = b.finish();
        ensure_contains(&json, "\"items\":[", "array start")?;
        ensure_contains(&json, "{\"name\":\"a\",\"value\":1}", "first item")?;
        ensure_contains(&json, "{\"name\":\"b\",\"value\":2}", "second item")
    }

    #[test]
    fn json_builder_raw_field_allows_prebuilt_json() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_raw("config", "[1,2,3]");
        let json = b.finish();
        ensure_contains(&json, "\"config\":[1,2,3]", "raw json array")
    }

    #[test]
    fn renderer_wire_names_are_stable() -> TestResult {
        ensure_equal(&Renderer::Human.as_str(), &"human", "human")?;
        ensure_equal(&Renderer::Json.as_str(), &"json", "json")?;
        ensure_equal(&Renderer::Toon.as_str(), &"toon", "toon")?;
        ensure_equal(&Renderer::Jsonl.as_str(), &"jsonl", "jsonl")?;
        ensure_equal(&Renderer::Compact.as_str(), &"compact", "compact")?;
        ensure_equal(&Renderer::Hook.as_str(), &"hook", "hook")
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        ctx: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn renderer_machine_readable_classification() -> TestResult {
        ensure(
            !Renderer::Human.is_machine_readable(),
            "human is not machine",
        )?;
        ensure(!Renderer::Toon.is_machine_readable(), "toon is not machine")?;
        ensure(Renderer::Json.is_machine_readable(), "json is machine")?;
        ensure(Renderer::Jsonl.is_machine_readable(), "jsonl is machine")?;
        ensure(
            Renderer::Compact.is_machine_readable(),
            "compact is machine",
        )?;
        ensure(Renderer::Hook.is_machine_readable(), "hook is machine")
    }

    #[test]
    fn output_context_json_flag_forces_json() -> TestResult {
        let ctx = OutputContext::detect_with_hints(true, false, None);
        ensure_equal(&ctx.renderer, &Renderer::Json, "json flag")
    }

    #[test]
    fn output_context_robot_flag_forces_json() -> TestResult {
        let ctx = OutputContext::detect_with_hints(false, true, None);
        ensure_equal(&ctx.renderer, &Renderer::Json, "robot flag")
    }

    #[test]
    fn output_context_format_override_takes_precedence() -> TestResult {
        let ctx = OutputContext::detect_with_hints(true, true, Some(Renderer::Toon));
        ensure_equal(&ctx.renderer, &Renderer::Toon, "format override")
    }

    #[test]
    fn response_envelope_success_has_stable_schema() -> TestResult {
        let json = ResponseEnvelope::success()
            .data(|d| {
                d.field_str("command", "test");
            })
            .finish();
        ensure_starts_with(&json, "{\"schema\":\"ee.response.v1\"", "schema")?;
        ensure_contains(&json, "\"success\":true", "success flag")?;
        ensure_contains(&json, "\"data\":{\"command\":\"test\"}", "data object")
    }

    #[test]
    fn response_envelope_failure_has_success_false() -> TestResult {
        let json = ResponseEnvelope::failure()
            .data_raw("{\"error\":\"something\"}")
            .finish();
        ensure_contains(&json, "\"success\":false", "failure flag")?;
        ensure_contains(&json, "\"data\":{\"error\":\"something\"}", "data raw")
    }

    #[test]
    fn response_envelope_degraded_array() -> TestResult {
        let degradations = vec![("code1", "message1")];
        let json = ResponseEnvelope::success()
            .data(|d| {
                d.field_str("status", "ok");
            })
            .degraded_array(&degradations, |obj, (code, msg)| {
                obj.field_str("code", code);
                obj.field_str("message", msg);
            })
            .finish();
        ensure_contains(&json, "\"degraded\":[{", "degraded array start")?;
        ensure_contains(&json, "\"code\":\"code1\"", "degradation code")
    }

    #[test]
    fn context_response_json_renders_provenance() -> TestResult {
        let response = context_response_fixture()?;
        let json = render_context_response_json(&response);

        ensure_starts_with(&json, "{\"schema\":\"ee.response.v1\"", "schema")?;
        ensure_contains(&json, "\"command\":\"context\"", "command")?;
        ensure_contains(
            &json,
            "\"provenance\":[{\"uri\":\"file://AGENTS.md#L42\",\"scheme\":\"file\",\"label\":\"AGENTS.md:L42\",\"locator\":\"L42\",\"note\":\"source evidence\"}]",
            "item provenance",
        )?;
        ensure_contains(
            &json,
            "\"provenanceFooter\":{\"memoryCount\":1,\"sourceCount\":1,\"schemes\":[\"file\"],\"entries\":[",
            "provenance footer",
        )?;
        ensure_contains(&json, "\"relevance\":0.800000", "stable relevance")
    }

    #[test]
    fn context_response_json_renders_pack_quality() -> TestResult {
        let response = context_response_fixture()?;
        let json = render_context_response_json(&response);

        ensure_contains(
            &json,
            "\"quality\":{\"itemCount\":1,\"omittedCount\":0,\"usedTokens\":10,\"maxTokens\":100,\"budgetUtilization\":0.100000",
            "quality metric header",
        )?;
        ensure_contains(
            &json,
            "\"averageRelevance\":0.800000,\"averageUtility\":0.600000",
            "quality score averages",
        )?;
        ensure_contains(
            &json,
            "\"provenanceSourceCount\":1,\"provenanceSourcesPerItem\":1.000000,\"provenanceComplete\":true",
            "quality provenance density",
        )?;
        ensure_contains(
            &json,
            "\"sections\":[{\"section\":\"procedural_rules\",\"itemCount\":1,\"usedTokens\":10},{\"section\":\"decisions\",\"itemCount\":0,\"usedTokens\":0}",
            "quality section metrics",
        )?;
        ensure_contains(
            &json,
            "\"omissions\":{\"tokenBudgetExceeded\":0,\"redundantCandidates\":0}",
            "quality omission metrics",
        )
    }

    #[test]
    fn degradation_severity_strings_are_stable() -> TestResult {
        ensure_equal(&DegradationSeverity::Low.as_str(), &"low", "low")?;
        ensure_equal(&DegradationSeverity::Medium.as_str(), &"medium", "medium")?;
        ensure_equal(&DegradationSeverity::High.as_str(), &"high", "high")
    }

    #[test]
    fn degradation_to_json_has_stable_structure() -> TestResult {
        let d = Degradation::new(
            "storage_stale",
            DegradationSeverity::Medium,
            "Storage index is stale.",
            "ee index rebuild",
        );
        let json = d.to_json();
        ensure_contains(&json, "\"code\":\"storage_stale\"", "code field")?;
        ensure_contains(&json, "\"severity\":\"medium\"", "severity field")?;
        ensure_contains(
            &json,
            "\"message\":\"Storage index is stale.\"",
            "message field",
        )?;
        ensure_contains(&json, "\"repair\":\"ee index rebuild\"", "repair field")
    }

    // ========================================================================
    // Error JSON Schema Tests (EE-015)
    //
    // These tests verify the ee.error.v1 JSON schema contract for all
    // DomainError variants. Each error type must produce valid JSON with:
    // - schema: "ee.error.v1"
    // - error.code: stable string matching the error variant
    // - error.message: human-readable description
    // - error.repair: optional remediation command (present when provided)
    // ========================================================================

    #[test]
    fn error_schema_usage_has_stable_structure() -> TestResult {
        let error = DomainError::Usage {
            message: "Unknown command 'xyz'.".to_string(),
            repair: Some("ee --help".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"usage\"", "code")?;
        ensure_contains(&json, "\"message\":\"Unknown command 'xyz'.\"", "message")?;
        ensure_contains(&json, "\"repair\":\"ee --help\"", "repair")
    }

    #[test]
    fn error_schema_configuration_has_stable_structure() -> TestResult {
        let error = DomainError::Configuration {
            message: "Invalid config file format.".to_string(),
            repair: Some("ee config validate".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"configuration\"", "code")?;
        ensure_contains(
            &json,
            "\"message\":\"Invalid config file format.\"",
            "message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee config validate\"", "repair")
    }

    #[test]
    fn error_schema_storage_has_stable_structure() -> TestResult {
        let error = DomainError::Storage {
            message: "Database file corrupted.".to_string(),
            repair: Some("ee db repair".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"storage\"", "code")?;
        ensure_contains(&json, "\"message\":\"Database file corrupted.\"", "message")?;
        ensure_contains(&json, "\"repair\":\"ee db repair\"", "repair")
    }

    #[test]
    fn error_schema_search_index_has_stable_structure() -> TestResult {
        let error = DomainError::SearchIndex {
            message: "Index is stale (generation 9, database generation 12).".to_string(),
            repair: Some("ee index rebuild".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"search_index\"", "code")?;
        ensure_contains(&json, "generation 9", "message contains details")?;
        ensure_contains(&json, "\"repair\":\"ee index rebuild\"", "repair")
    }

    #[test]
    fn error_schema_import_has_stable_structure() -> TestResult {
        let error = DomainError::Import {
            message: "CASS session file not found.".to_string(),
            repair: Some("ee import --dry-run".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"import\"", "code")?;
        ensure_contains(
            &json,
            "\"message\":\"CASS session file not found.\"",
            "message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee import --dry-run\"", "repair")
    }

    #[test]
    fn error_schema_unsatisfied_degraded_mode_has_stable_structure() -> TestResult {
        let error = DomainError::UnsatisfiedDegradedMode {
            message: "Semantic search unavailable and --require-semantic was set.".to_string(),
            repair: Some("ee search --lexical-only".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"unsatisfied_degraded_mode\"", "code")?;
        ensure_contains(&json, "--require-semantic", "message contains flag")?;
        ensure_contains(&json, "\"repair\":\"ee search --lexical-only\"", "repair")
    }

    #[test]
    fn error_schema_policy_denied_has_stable_structure() -> TestResult {
        let error = DomainError::PolicyDenied {
            message: "Redaction policy prevents this operation.".to_string(),
            repair: Some("ee policy review".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"policy_denied\"", "code")?;
        ensure_contains(
            &json,
            "\"message\":\"Redaction policy prevents this operation.\"",
            "message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee policy review\"", "repair")
    }

    #[test]
    fn error_schema_migration_required_has_stable_structure() -> TestResult {
        let error = DomainError::MigrationRequired {
            message: "Database schema version 3 requires migration to version 5.".to_string(),
            repair: Some("ee db migrate".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"migration_required\"", "code")?;
        ensure_contains(&json, "version 3", "message contains version")?;
        ensure_contains(&json, "\"repair\":\"ee db migrate\"", "repair")
    }

    #[test]
    fn error_schema_all_codes_are_covered() -> TestResult {
        // Ensure we have tests for all 8 error types
        let codes = [
            "usage",
            "configuration",
            "storage",
            "search_index",
            "import",
            "unsatisfied_degraded_mode",
            "policy_denied",
            "migration_required",
        ];

        // Verify each code produces valid JSON
        let errors = [
            DomainError::Usage {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Configuration {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Storage {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::SearchIndex {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Import {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::UnsatisfiedDegradedMode {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::PolicyDenied {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::MigrationRequired {
                message: "test".to_string(),
                repair: None,
            },
        ];

        for (error, expected_code) in errors.iter().zip(codes.iter()) {
            let json = error_response_json(error);
            ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", expected_code)?;
            ensure_contains(
                &json,
                &format!("\"code\":\"{expected_code}\""),
                expected_code,
            )?;
        }
        Ok(())
    }

    #[test]
    fn error_schema_without_repair_omits_field() -> TestResult {
        // Verify that when repair is None, the field is absent (not null)
        for error in [
            DomainError::Usage {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Storage {
                message: "test".to_string(),
                repair: None,
            },
        ] {
            let json = error_response_json(&error);
            ensure(
                !json.contains("repair"),
                format!("{}: repair field should be absent when None", error.code()),
            )?;
        }
        Ok(())
    }

    // ========================================================================
    // TOON Output Tests (EE-036)
    //
    // TOON is rendered from the canonical JSON envelope through /dp/toon_rust.
    // These tests prove the public renderer is valid TOON and semantically
    // equivalent to the JSON status output.
    // ========================================================================

    #[test]
    fn toon_status_has_required_fields() -> TestResult {
        let report = StatusReport::gather();
        let toon = render_status_toon(&report);
        ensure_contains(&toon, "schema: ee.response.v1", "toon schema")?;
        ensure_contains(&toon, "success: true", "toon success")?;
        ensure_contains(&toon, "command: status", "toon command")?;
        ensure_contains(&toon, "capabilities:", "toon capabilities section")?;
        ensure_contains(&toon, "runtime:", "toon runtime section")?;
        ensure_contains(&toon, "engine: asupersync", "toon engine")
    }

    #[test]
    fn toon_status_has_degradation_details() -> TestResult {
        let report = StatusReport::gather();
        let toon = render_status_toon(&report);
        ensure_contains(
            &toon,
            "degraded[2]{code,severity,message,repair}:",
            "degradation section",
        )?;
        ensure_contains(&toon, "storage_not_implemented", "storage degradation code")?;
        ensure_contains(&toon, "search_not_implemented", "search degradation code")
    }

    #[test]
    fn json_toon_parity_status_decodes_to_same_json() -> TestResult {
        let report = StatusReport::gather();
        let json = render_status_json(&report);
        let toon = render_status_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("status JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("status TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches status JSON")
    }

    #[test]
    fn json_toon_parity_health_decodes_to_same_json() -> TestResult {
        let report = HealthReport::gather();
        let json = render_health_json(&report);
        let toon = render_health_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("health JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("health TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches health JSON")
    }

    #[test]
    fn json_toon_parity_doctor_decodes_to_same_json() -> TestResult {
        let report = DoctorReport::gather();
        let json = render_doctor_json(&report);
        let toon = render_doctor_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("doctor JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("doctor TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches doctor JSON")
    }

    #[test]
    fn json_toon_parity_agent_docs_decodes_to_same_json() -> TestResult {
        let report = AgentDocsReport::gather(None);
        let json = render_agent_docs_json(&report);
        let toon = render_agent_docs_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("agent-docs JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("agent-docs TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches agent-docs JSON")
    }

    #[test]
    fn json_toon_parity_context_decodes_to_same_json() -> TestResult {
        let response = context_response_fixture()?;
        let json = render_context_response_json(&response);
        let toon = render_context_response_toon(&response);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("context JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("context TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches context JSON")
    }

    #[test]
    fn invalid_json_to_toon_returns_stable_error() -> TestResult {
        let toon = super::render_toon_from_json("{not valid json");
        ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
        ensure_contains(&toon, "code: toon_encoding_failed", "error code")
    }

    const TOON_STATUS_GOLDEN: &str = include_str!("../../tests/fixtures/golden/toon/status.golden");

    #[test]
    fn toon_status_matches_golden() -> TestResult {
        let report = StatusReport::gather();
        let actual = render_status_toon(&report);

        // Normalize both for comparison (trim trailing whitespace)
        let actual_lines: Vec<&str> = actual.lines().collect();
        let golden_lines: Vec<&str> = TOON_STATUS_GOLDEN.lines().collect();

        if actual_lines.len() != golden_lines.len() {
            return Err(format!(
                "line count mismatch: actual={} golden={}",
                actual_lines.len(),
                golden_lines.len()
            ));
        }

        for (i, (actual_line, golden_line)) in
            actual_lines.iter().zip(golden_lines.iter()).enumerate()
        {
            if actual_line.trim_end() != golden_line.trim_end() {
                return Err(format!(
                    "line {} mismatch:\n  actual:  {:?}\n  golden:  {:?}",
                    i + 1,
                    actual_line,
                    golden_line
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn golden_error_fixtures_are_valid_json() -> TestResult {
        let fixtures = [
            include_str!("../../tests/fixtures/golden/error/usage.golden"),
            include_str!("../../tests/fixtures/golden/error/configuration.golden"),
            include_str!("../../tests/fixtures/golden/error/storage.golden"),
            include_str!("../../tests/fixtures/golden/error/search_index.golden"),
            include_str!("../../tests/fixtures/golden/error/import.golden"),
            include_str!("../../tests/fixtures/golden/error/policy_denied.golden"),
            include_str!("../../tests/fixtures/golden/error/migration_required.golden"),
            include_str!("../../tests/fixtures/golden/error/unsatisfied_degraded_mode.golden"),
            include_str!("../../tests/fixtures/golden/error/no_repair.golden"),
        ];

        for (i, fixture) in fixtures.iter().enumerate() {
            let value: serde_json::Value = serde_json::from_str(fixture)
                .map_err(|e| format!("error fixture {} is not valid JSON: {e}", i))?;
            if value.get("schema") != Some(&serde_json::Value::String("ee.error.v1".to_string())) {
                return Err(format!("error fixture {} missing schema", i));
            }
        }
        Ok(())
    }

    #[test]
    fn golden_status_fixtures_are_valid_json() -> TestResult {
        let fixtures = [
            include_str!("../../tests/fixtures/golden/status/status_healthy.golden"),
            include_str!("../../tests/fixtures/golden/status/status_degraded.golden"),
        ];

        for (i, fixture) in fixtures.iter().enumerate() {
            let value: serde_json::Value = serde_json::from_str(fixture)
                .map_err(|e| format!("status fixture {} is not valid JSON: {e}", i))?;
            if value.get("schema") != Some(&serde_json::Value::String("ee.response.v1".to_string()))
            {
                return Err(format!("status fixture {} missing schema", i));
            }
        }
        Ok(())
    }

    #[test]
    fn golden_version_fixture_is_valid_json() -> TestResult {
        let fixture = include_str!("../../tests/fixtures/golden/version/version.golden");
        let value: serde_json::Value = serde_json::from_str(fixture)
            .map_err(|e| format!("version fixture is not valid JSON: {e}"))?;
        if value.get("schema") != Some(&serde_json::Value::String("ee.response.v1".to_string())) {
            return Err("version fixture missing schema".to_string());
        }
        Ok(())
    }

    #[test]
    fn golden_human_fixtures_have_expected_structure() -> TestResult {
        let error_fixture =
            include_str!("../../tests/fixtures/golden/human/error_with_repair.golden");
        ensure_starts_with(error_fixture, "error:", "human error starts with 'error:'")?;
        ensure_contains(error_fixture, "Next:", "human error has Next section")?;

        let success_fixture =
            include_str!("../../tests/fixtures/golden/human/success_with_summary.golden");
        ensure_contains(success_fixture, "Next:", "human success has Next section")?;
        ensure(
            !success_fixture.starts_with('{'),
            "human output is not JSON",
        )
    }

    // ========================================================================
    // Field Profile Tests (EE-037)
    //
    // These tests verify the --fields filtering behavior for JSON output.
    // Each profile level progressively includes more fields.
    // ========================================================================

    #[test]
    fn field_profile_as_str_is_stable() -> TestResult {
        use super::FieldProfile;
        ensure_equal(&FieldProfile::Minimal.as_str(), &"minimal", "minimal")?;
        ensure_equal(&FieldProfile::Summary.as_str(), &"summary", "summary")?;
        ensure_equal(&FieldProfile::Standard.as_str(), &"standard", "standard")?;
        ensure_equal(&FieldProfile::Full.as_str(), &"full", "full")
    }

    #[test]
    fn field_profile_inclusion_rules() -> TestResult {
        use super::FieldProfile;

        // Minimal: no arrays, no summary metrics, no verbose
        ensure(!FieldProfile::Minimal.include_arrays(), "minimal no arrays")?;
        ensure(
            !FieldProfile::Minimal.include_summary_metrics(),
            "minimal no summary",
        )?;
        ensure(
            !FieldProfile::Minimal.include_verbose_details(),
            "minimal no verbose",
        )?;

        // Summary: no arrays, has summary metrics, no verbose
        ensure(!FieldProfile::Summary.include_arrays(), "summary no arrays")?;
        ensure(
            FieldProfile::Summary.include_summary_metrics(),
            "summary has summary",
        )?;
        ensure(
            !FieldProfile::Summary.include_verbose_details(),
            "summary no verbose",
        )?;

        // Standard: has arrays, has summary metrics, no verbose
        ensure(
            FieldProfile::Standard.include_arrays(),
            "standard has arrays",
        )?;
        ensure(
            FieldProfile::Standard.include_summary_metrics(),
            "standard has summary",
        )?;
        ensure(
            !FieldProfile::Standard.include_verbose_details(),
            "standard no verbose",
        )?;

        // Full: has everything
        ensure(FieldProfile::Full.include_arrays(), "full has arrays")?;
        ensure(
            FieldProfile::Full.include_summary_metrics(),
            "full has summary",
        )?;
        ensure(
            FieldProfile::Full.include_verbose_details(),
            "full has verbose",
        )
    }

    #[test]
    fn render_status_json_filtered_minimal_has_only_essentials() -> TestResult {
        use super::{FieldProfile, render_status_json_filtered};
        let report = StatusReport::gather();
        let json = render_status_json_filtered(&report, FieldProfile::Minimal);

        ensure_contains(&json, "\"schema\":\"ee.response.v1\"", "schema")?;
        ensure_contains(&json, "\"success\":true", "success")?;
        ensure_contains(&json, "\"fields\":\"minimal\"", "fields indicator")?;
        ensure_contains(&json, "\"command\":\"status\"", "command")?;
        ensure_contains(&json, "\"version\":", "version")?;
        // Minimal should NOT have capabilities, runtime, or degraded
        ensure(!json.contains("\"capabilities\":"), "no capabilities")?;
        ensure(!json.contains("\"runtime\":"), "no runtime")?;
        ensure(!json.contains("\"degraded\":"), "no degraded")
    }

    #[test]
    fn render_status_json_filtered_summary_adds_capabilities() -> TestResult {
        use super::{FieldProfile, render_status_json_filtered};
        let report = StatusReport::gather();
        let json = render_status_json_filtered(&report, FieldProfile::Summary);

        ensure_contains(&json, "\"fields\":\"summary\"", "fields indicator")?;
        ensure_contains(&json, "\"capabilities\":", "has capabilities")?;
        // Summary should NOT have runtime or degraded arrays
        ensure(!json.contains("\"runtime\":"), "no runtime")?;
        ensure(!json.contains("\"degraded\":"), "no degraded")
    }

    #[test]
    fn render_status_json_filtered_standard_adds_arrays() -> TestResult {
        use super::{FieldProfile, render_status_json_filtered};
        let report = StatusReport::gather();
        let json = render_status_json_filtered(&report, FieldProfile::Standard);

        ensure_contains(&json, "\"fields\":\"standard\"", "fields indicator")?;
        ensure_contains(&json, "\"capabilities\":", "has capabilities")?;
        ensure_contains(&json, "\"runtime\":", "has runtime")?;
        ensure_contains(&json, "\"degraded\":", "has degraded")?;
        // Standard should NOT have repair in degraded items
        ensure(!json.contains("\"repair\":"), "no repair in degraded")
    }

    #[test]
    fn render_status_json_filtered_full_includes_verbose() -> TestResult {
        use super::{FieldProfile, render_status_json_filtered};
        let report = StatusReport::gather();
        let json = render_status_json_filtered(&report, FieldProfile::Full);

        ensure_contains(&json, "\"fields\":\"full\"", "fields indicator")?;
        ensure_contains(&json, "\"capabilities\":", "has capabilities")?;
        ensure_contains(&json, "\"runtime\":", "has runtime")?;
        ensure_contains(&json, "\"degraded\":", "has degraded")?;
        ensure_contains(&json, "\"repair\":", "has repair in degraded")
    }

    #[test]
    fn render_capabilities_json_filtered_minimal_only_essentials() -> TestResult {
        use super::{FieldProfile, render_capabilities_json_filtered};
        use crate::core::capabilities::CapabilitiesReport;
        let report = CapabilitiesReport::gather();
        let json = render_capabilities_json_filtered(&report, FieldProfile::Minimal);

        ensure_contains(&json, "\"command\":\"capabilities\"", "command")?;
        ensure_contains(&json, "\"version\":", "version")?;
        ensure_contains(&json, "\"fields\":\"minimal\"", "fields")?;
        // Minimal: no arrays, no summary
        ensure(!json.contains("\"subsystems\":"), "no subsystems")?;
        ensure(!json.contains("\"features\":"), "no features")?;
        ensure(!json.contains("\"commands\":"), "no commands")?;
        ensure(!json.contains("\"summary\":"), "no summary")
    }

    #[test]
    fn render_capabilities_json_filtered_full_has_descriptions() -> TestResult {
        use super::{FieldProfile, render_capabilities_json_filtered};
        use crate::core::capabilities::CapabilitiesReport;
        let report = CapabilitiesReport::gather();
        let json = render_capabilities_json_filtered(&report, FieldProfile::Full);

        ensure_contains(&json, "\"subsystems\":", "has subsystems")?;
        ensure_contains(&json, "\"description\":", "has descriptions")?;
        ensure_contains(&json, "\"summary\":", "has summary")
    }

    // ========================================================================
    // Evaluation Report Renderer Tests (EE-255)
    // ========================================================================

    #[test]
    fn render_eval_report_json_empty_report() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::EvaluationReport;

        let report = EvaluationReport::new();
        let json = render_eval_report_json(&report, None);

        ensure_contains(&json, "\"schema\":\"ee.response.v1\"", "schema")?;
        ensure_contains(&json, "\"success\":true", "success")?;
        ensure_contains(&json, "\"command\":\"eval run\"", "command")?;
        ensure_contains(&json, "\"status\":\"no_scenarios\"", "status")?;
        ensure_contains(&json, "\"scenariosRun\":0", "scenariosRun")?;
        ensure_contains(&json, "\"results\":[]", "empty results")
    }

    #[test]
    fn render_eval_report_json_with_scenario_id() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::EvaluationReport;

        let report = EvaluationReport::new();
        let json = render_eval_report_json(&report, Some("test_scenario"));

        ensure_contains(&json, "\"scenarioId\":\"test_scenario\"", "scenarioId")
    }

    #[test]
    fn render_eval_report_json_all_passed() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::{EvaluationReport, EvaluationStatus, ScenarioValidationResult};

        let mut report = EvaluationReport::new();
        report.add_result(ScenarioValidationResult {
            scenario_id: "scenario_1".to_string(),
            passed: true,
            steps_passed: 3,
            steps_total: 3,
            failures: vec![],
        });
        report.add_result(ScenarioValidationResult {
            scenario_id: "scenario_2".to_string(),
            passed: true,
            steps_passed: 2,
            steps_total: 2,
            failures: vec![],
        });
        report.finalize();

        ensure(report.status, EvaluationStatus::AllPassed, "status")?;

        let json = render_eval_report_json(&report, None);
        ensure_contains(&json, "\"success\":true", "success")?;
        ensure_contains(&json, "\"status\":\"all_passed\"", "status")?;
        ensure_contains(&json, "\"scenariosRun\":2", "scenariosRun")?;
        ensure_contains(&json, "\"scenariosPassed\":2", "scenariosPassed")?;
        ensure_contains(&json, "\"scenariosFailed\":0", "scenariosFailed")?;
        ensure_contains(&json, "\"scenarioId\":\"scenario_1\"", "result 1")?;
        ensure_contains(&json, "\"scenarioId\":\"scenario_2\"", "result 2")
    }

    #[test]
    fn render_eval_report_json_some_failed() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::{
            EvaluationReport, EvaluationStatus, ScenarioValidationResult, ValidationFailure,
            ValidationFailureKind,
        };

        let mut report = EvaluationReport::new();
        report.add_result(ScenarioValidationResult {
            scenario_id: "passing".to_string(),
            passed: true,
            steps_passed: 2,
            steps_total: 2,
            failures: vec![],
        });
        report.add_result(ScenarioValidationResult {
            scenario_id: "failing".to_string(),
            passed: false,
            steps_passed: 1,
            steps_total: 2,
            failures: vec![ValidationFailure {
                step: 2,
                kind: ValidationFailureKind::GoldenMismatch,
                message: "Output differs from golden".to_string(),
            }],
        });
        report.finalize();

        ensure(report.status, EvaluationStatus::SomeFailed, "status")?;

        let json = render_eval_report_json(&report, None);
        ensure_contains(&json, "\"success\":false", "not success")?;
        ensure_contains(&json, "\"status\":\"some_failed\"", "status")?;
        ensure_contains(&json, "\"scenariosPassed\":1", "scenariosPassed")?;
        ensure_contains(&json, "\"scenariosFailed\":1", "scenariosFailed")?;
        ensure_contains(&json, "\"kind\":\"golden_mismatch\"", "failure kind")?;
        ensure_contains(
            &json,
            "\"message\":\"Output differs from golden\"",
            "failure msg",
        )
    }

    #[test]
    fn render_eval_report_human_empty_report() -> TestResult {
        use super::render_eval_report_human;
        use crate::eval::EvaluationReport;

        let report = EvaluationReport::new();
        let human = render_eval_report_human(&report, None);

        ensure_contains(&human, "ee eval run", "header")?;
        ensure_contains(&human, "Status: no scenarios available", "status")?;
        ensure_contains(&human, "Results: 0 run, 0 passed, 0 failed", "results")?;
        ensure_contains(&human, "No evaluation scenarios configured", "message")
    }

    #[test]
    fn render_eval_report_human_with_results() -> TestResult {
        use super::render_eval_report_human;
        use crate::eval::{
            EvaluationReport, ScenarioValidationResult, ValidationFailure, ValidationFailureKind,
        };

        let mut report = EvaluationReport::new();
        report.add_result(ScenarioValidationResult {
            scenario_id: "test_scenario".to_string(),
            passed: false,
            steps_passed: 2,
            steps_total: 3,
            failures: vec![ValidationFailure {
                step: 3,
                kind: ValidationFailureKind::ExitCodeMismatch,
                message: "Expected 0, got 1".to_string(),
            }],
        });
        report.finalize();

        let human = render_eval_report_human(&report, None);

        ensure_contains(&human, "[FAIL] test_scenario: 2/3 steps", "scenario result")?;
        ensure_contains(&human, "Step 3: exit_code_mismatch", "failure step")?;
        ensure_contains(&human, "Expected 0, got 1", "failure message")
    }

    #[test]
    fn render_eval_report_toon_produces_valid_toon() -> TestResult {
        use super::render_eval_report_toon;
        use crate::eval::{EvaluationReport, ScenarioValidationResult};

        let mut report = EvaluationReport::new();
        report.add_result(ScenarioValidationResult {
            scenario_id: "test".to_string(),
            passed: true,
            steps_passed: 1,
            steps_total: 1,
            failures: vec![],
        });
        report.finalize();

        let toon = render_eval_report_toon(&report, None);

        ensure_contains(&toon, "ee.response.v1", "schema")?;
        ensure_contains(&toon, "all_passed", "status")?;
        ensure_contains(&toon, "test", "scenario id")
    }

    #[test]
    fn evaluation_status_strings_are_stable() -> TestResult {
        use crate::eval::EvaluationStatus;

        ensure(
            EvaluationStatus::NoScenarios.as_str(),
            "no_scenarios",
            "no_scenarios",
        )?;
        ensure(
            EvaluationStatus::AllPassed.as_str(),
            "all_passed",
            "all_passed",
        )?;
        ensure(
            EvaluationStatus::SomeFailed.as_str(),
            "some_failed",
            "some_failed",
        )?;
        ensure(
            EvaluationStatus::AllFailed.as_str(),
            "all_failed",
            "all_failed",
        )
    }

    #[test]
    fn evaluation_status_is_success() -> TestResult {
        use crate::eval::EvaluationStatus;

        ensure(
            EvaluationStatus::NoScenarios.is_success(),
            true,
            "no_scenarios is success",
        )?;
        ensure(
            EvaluationStatus::AllPassed.is_success(),
            true,
            "all_passed is success",
        )?;
        ensure(
            EvaluationStatus::SomeFailed.is_success(),
            false,
            "some_failed not success",
        )?;
        ensure(
            EvaluationStatus::AllFailed.is_success(),
            false,
            "all_failed not success",
        )
    }

    #[test]
    fn render_eval_report_with_elapsed_and_fixture_dir() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::EvaluationReport;

        let report = EvaluationReport::new()
            .with_elapsed_ms(42.5)
            .with_fixture_dir("tests/fixtures/eval/");
        let json = render_eval_report_json(&report, None);

        ensure_contains(&json, "\"elapsedMs\":42.50", "elapsedMs")?;
        ensure_contains(
            &json,
            "\"fixtureDir\":\"tests/fixtures/eval/\"",
            "fixtureDir",
        )
    }
}
