//! ADR 0065 workspace primer — a deterministic, cached, token-budgeted
//! workspace charter assembled from the highest-value durable memory.
//! bd-39tzu.2.
//!
//! Primer is KNOWLEDGE; `ee swarm brief` is coordination; `ee orient` is
//! posture. Assembly is a fixed quota mix over four sections (rules /
//! warnings / decisions / loadBearing) computed from persisted state only:
//! the load-bearing section reads persisted graph-snapshot centrality rows
//! and is honestly omitted (`primer_graph_unavailable`) when they are
//! missing — never recomputed inline (latency contract). Output is
//! byte-identical for identical `(workspace_id, db_generation, config_hash,
//! budget, format)` keys, which is also the `primer_cache` cache key.
//! Nothing here reads the wall clock: ordering uses stored timestamps and
//! ids only, so a cache hit and a cold assembly produce identical bytes.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::db::{DbConnection, GraphSnapshotStatus, GraphSnapshotType, MemoryLinkRelation};
use crate::output::jsonl_export::contains_secret_pattern;

/// Response payload schema carried under `ee.response.v2` `data.primer`.
pub const PRIMER_SCHEMA_V1: &str = "ee.primer.v1";

/// No cache row for the key; the primer was assembled fresh (info).
pub const PRIMER_CACHE_COLD_CODE: &str = "primer_cache_cold";

/// Persisted centrality rows missing or unusable; `loadBearing` omitted and
/// the rules authority factor falls back to neutral (info).
pub const PRIMER_GRAPH_UNAVAILABLE_CODE: &str = "primer_graph_unavailable";

/// Proportional shrink hit a section floor (info).
pub const PRIMER_BUDGET_FLOOR_CODE: &str = "primer_budget_floor";

/// Default token budget when `[primer] default_tokens` is unset.
pub const PRIMER_DEFAULT_BUDGET_TOKENS: u32 = 600;

/// Fixed quota shares per ADR 0065 §1 (rules / warnings / decisions /
/// loadBearing). The floor order is the same order: earlier sections are
/// protected at the expense of later ones.
pub const PRIMER_SECTION_SHARES: [(&str, f32); 4] = [
    ("rules", 0.40),
    ("warnings", 0.25),
    ("decisions", 0.20),
    ("loadBearing", 0.15),
];

/// Bounded candidate scan per section (defensive ceiling).
pub const PRIMER_SECTION_CANDIDATE_CAP: usize = 512;

/// Single-line rendering budget per item (chars), mirroring the existing
/// preview discipline.
pub const PRIMER_LINE_MAX_CHARS: usize = 200;

/// Output format. Markdown renders a token-tight prepend block with compact
/// provenance suffixes; JSON carries the structured report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimerFormat {
    Markdown,
    Json,
}

impl PrimerFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }

    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "markdown" => Some(Self::Markdown),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Resolved assembly settings. `config_hash` binds the cache key to the
/// config inputs that shape output (budget default, privacy posture);
/// compute it with [`primer_config_hash`].
#[derive(Clone, Debug, PartialEq)]
pub struct PrimerSettings {
    pub budget_tokens: u32,
    pub format: PrimerFormat,
    pub config_hash: String,
    /// Skip memories whose bodies trip the secret detector (workspace
    /// `[privacy]` posture). Skips are counted, never silent.
    pub redact_secrets: bool,
    /// `[memory] include_global && participate` (bd-1bfwa.3 slice C); when
    /// false the primer never consults the user-global store.
    pub global_lane_enabled: bool,
}

/// One candidate memory row (joined fields the selectors need).
#[derive(Clone, Debug, PartialEq)]
pub struct PrimerCandidate {
    pub memory_id: String,
    pub level: String,
    pub kind: String,
    pub content: String,
    pub confidence: f32,
    pub utility: f32,
    /// Severity proxy for the warnings section.
    pub importance: f32,
    /// Stored timestamp used for recency ordering (never the wall clock).
    pub updated_at: String,
    pub provenance_uri: Option<String>,
    /// True when another memory supersedes this one (decisions section
    /// excludes superseded chain links).
    pub superseded: bool,
    /// True when the row came from the user-global store (bd-1bfwa.3).
    /// Global rows compete in the same sections as workspace rows; the lane
    /// is surfaced through the item's provenance `source_type`.
    pub global_lane: bool,
}

/// One persisted centrality row (from the latest valid graph snapshot).
#[derive(Clone, Debug, PartialEq)]
pub struct PrimerCentralityRow {
    pub memory_id: String,
    pub authority: f64,
    pub betweenness: f64,
}

/// One provenance pointer on a primer item.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrimerProvenanceRef {
    pub uri: String,
    pub source_type: String,
}

/// One rendered primer item (`ee.primer.v1` `sections[].items[]`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PrimerItem {
    pub memory_id: String,
    /// Rendered single line; provenance-suffixed in markdown form.
    pub line: String,
    pub level: String,
    pub kind: String,
    pub confidence: f32,
    pub provenance: Vec<PrimerProvenanceRef>,
}

/// One primer section in fixed order.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PrimerSection {
    pub name: String,
    pub items: Vec<PrimerItem>,
}

/// Skip accounting (`meta.skipped`).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrimerSkipped {
    pub redaction: u32,
    pub budget_floor: u32,
}

/// `ee.primer.v1` `meta`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrimerMeta {
    pub tokens_used: u32,
    pub skipped: PrimerSkipped,
    pub floors_engaged: Vec<String>,
}

/// One `degraded[]` entry the primer pipeline may emit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrimerDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
}

/// Deterministic primer result (`ee.primer.v1`). `cache_hit` is set at
/// response time and is intentionally excluded from the cached bytes so a
/// hit and a cold assembly stay byte-identical everywhere else.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PrimerReport {
    pub schema: String,
    pub budget_tokens: u32,
    pub format: String,
    #[serde(default)]
    pub cache_hit: bool,
    pub db_generation: i64,
    pub config_hash: String,
    pub sections: Vec<PrimerSection>,
    pub degraded: Vec<PrimerDegradation>,
    pub meta: PrimerMeta,
    /// Rendered markdown when `format == markdown`; `None` for JSON.
    pub rendered_markdown: Option<String>,
}

/// Compute the cache-key config hash from the config inputs that shape
/// primer output. Reuses the derived-asset dependency hashing so the hash
/// vocabulary stays consistent across caches.
#[must_use]
pub fn primer_config_hash(default_tokens: u32, redact_secrets: bool) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PRIMER_SCHEMA_V1.as_bytes());
    hasher.update(b"\0primer.default_tokens\0");
    hasher.update(default_tokens.to_string().as_bytes());
    hasher.update(b"\0privacy.redact_secrets\0");
    hasher.update(if redact_secrets { b"true" } else { b"false" });
    format!(
        "blake3:{}",
        hasher
            .finalize()
            .to_hex()
            .chars()
            .take(16)
            .collect::<String>()
    )
}

/// Resolve assembly settings from workspace config: `[primer]
/// default_tokens` (default 600) and the `[privacy] redact_secrets`
/// posture (default on). `budget_override` wins over config.
#[must_use]
pub fn primer_settings_from_workspace(
    workspace_path: &std::path::Path,
    format: PrimerFormat,
    budget_override: Option<u32>,
) -> PrimerSettings {
    let config = crate::config::workspace_config(workspace_path);
    let default_tokens = config
        .as_ref()
        .and_then(|config| config.primer.default_tokens)
        .and_then(|tokens| u32::try_from(tokens).ok())
        .unwrap_or(PRIMER_DEFAULT_BUDGET_TOKENS);
    let redact_secrets = config
        .as_ref()
        .and_then(|config| config.privacy.redact_secrets)
        .unwrap_or(true);
    let global_lane_enabled = config
        .as_ref()
        .map(|config| {
            config.memory.include_global.unwrap_or(true)
                && config.memory.participate.unwrap_or(true)
        })
        .unwrap_or(true);
    PrimerSettings {
        budget_tokens: budget_override.unwrap_or(default_tokens),
        format,
        config_hash: primer_config_hash(default_tokens, redact_secrets),
        redact_secrets,
        global_lane_enabled,
    }
}

/// Deterministic single-line rendering of a memory body.
#[must_use]
pub fn primer_line(content: &str) -> String {
    let single_line = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= PRIMER_LINE_MAX_CHARS {
        return single_line;
    }
    let mut line: String = single_line
        .chars()
        .take(PRIMER_LINE_MAX_CHARS - 1)
        .collect();
    line.push('…');
    line
}

/// Compact provenance suffix: `mem_` plus the first eight id characters
/// (ADR 0065 §4 short memory-id form).
#[must_use]
pub fn primer_short_memory_ref(memory_id: &str) -> String {
    let suffix: String = memory_id
        .strip_prefix("mem_")
        .unwrap_or(memory_id)
        .chars()
        .take(8)
        .collect();
    format!("mem_{suffix}")
}

fn item_token_cost(line: &str) -> u32 {
    crate::pack::estimate_tokens_default(line)
}

struct RankedSection {
    name: &'static str,
    candidates: Vec<PrimerItem>,
}

fn render_item(candidate: &PrimerCandidate, markdown: bool) -> PrimerItem {
    let body = primer_line(&candidate.content);
    let line = if markdown {
        format!("{body} [{}]", primer_short_memory_ref(&candidate.memory_id))
    } else {
        body
    };
    PrimerItem {
        memory_id: candidate.memory_id.clone(),
        line,
        level: candidate.level.clone(),
        kind: candidate.kind.clone(),
        confidence: candidate.confidence,
        provenance: candidate
            .provenance_uri
            .as_ref()
            .map(|uri| {
                vec![PrimerProvenanceRef {
                    uri: uri.clone(),
                    source_type: if candidate.global_lane {
                        "global_store".to_owned()
                    } else {
                        "memory_provenance".to_owned()
                    },
                }]
            })
            .unwrap_or_default(),
    }
}

/// Assemble a primer from pre-fetched candidates and persisted centrality
/// rows (ADR 0065 §§1–4). Pure and wall-clock-free: identical inputs
/// produce a byte-identical report.
#[must_use]
pub fn assemble_primer(
    candidates: &[PrimerCandidate],
    centrality: Option<&[PrimerCentralityRow]>,
    settings: &PrimerSettings,
    db_generation: i64,
) -> PrimerReport {
    let mut degraded = Vec::new();
    let mut skipped = PrimerSkipped::default();
    let markdown = settings.format == PrimerFormat::Markdown;

    // Redaction gate first: a memory whose body would require redaction
    // above the workspace [privacy] defaults is skipped with a counted skip
    // reason rather than leaked or mangled (ADR 0065 §4).
    let admitted: Vec<&PrimerCandidate> = candidates
        .iter()
        .filter(|candidate| {
            if settings.redact_secrets && contains_secret_pattern(&candidate.content) {
                skipped.redaction += 1;
                false
            } else {
                true
            }
        })
        .collect();

    let authority_of = |memory_id: &str| -> f64 {
        centrality
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.memory_id == memory_id)
                    .map(|row| row.authority)
            })
            .unwrap_or(0.0)
    };

    if centrality.is_none() {
        degraded.push(PrimerDegradation {
            code: PRIMER_GRAPH_UNAVAILABLE_CODE.to_owned(),
            severity: "info".to_owned(),
            message: "persisted centrality rows are missing or unusable; loadBearing section omitted and rules use a neutral authority factor".to_owned(),
            repair: Some("ee graph centrality-refresh --workspace .".to_owned()),
        });
    }

    // Section candidate ranking (ADR §1), deduplicated across sections in
    // priority order so the small budget is never spent twice on one memory.
    let mut used: BTreeSet<&str> = BTreeSet::new();

    let mut rules: Vec<&PrimerCandidate> = admitted
        .iter()
        .copied()
        .filter(|candidate| candidate.level == "procedural")
        .collect();
    rules.sort_by(|left, right| {
        let left_score = f64::from(left.confidence)
            * f64::from(left.utility)
            * (1.0 + authority_of(&left.memory_id));
        let right_score = f64::from(right.confidence)
            * f64::from(right.utility)
            * (1.0 + authority_of(&right.memory_id));
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    rules.truncate(PRIMER_SECTION_CANDIDATE_CAP);
    for candidate in &rules {
        used.insert(candidate.memory_id.as_str());
    }

    let mut warnings: Vec<&PrimerCandidate> = admitted
        .iter()
        .copied()
        .filter(|candidate| !used.contains(candidate.memory_id.as_str()))
        .filter(|candidate| matches!(candidate.kind.as_str(), "failure" | "anti-pattern" | "risk"))
        .collect();
    warnings.sort_by(|left, right| {
        right
            .importance
            .partial_cmp(&left.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    warnings.truncate(PRIMER_SECTION_CANDIDATE_CAP);
    for candidate in &warnings {
        used.insert(candidate.memory_id.as_str());
    }

    let mut decisions: Vec<&PrimerCandidate> = admitted
        .iter()
        .copied()
        .filter(|candidate| !used.contains(candidate.memory_id.as_str()))
        .filter(|candidate| candidate.kind == "decision" && !candidate.superseded)
        .collect();
    decisions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    decisions.truncate(PRIMER_SECTION_CANDIDATE_CAP);
    for candidate in &decisions {
        used.insert(candidate.memory_id.as_str());
    }

    let mut load_bearing: Vec<&PrimerCandidate> = Vec::new();
    if let Some(rows) = centrality {
        let mut ranked_rows: Vec<&PrimerCentralityRow> = rows.iter().collect();
        ranked_rows.sort_by(|left, right| {
            right
                .authority
                .partial_cmp(&left.authority)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    right
                        .betweenness
                        .partial_cmp(&left.betweenness)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| left.memory_id.cmp(&right.memory_id))
        });
        for row in ranked_rows.into_iter().take(PRIMER_SECTION_CANDIDATE_CAP) {
            if used.contains(row.memory_id.as_str()) {
                continue;
            }
            if let Some(candidate) = admitted
                .iter()
                .copied()
                .find(|candidate| candidate.memory_id == row.memory_id)
            {
                used.insert(candidate.memory_id.as_str());
                load_bearing.push(candidate);
            }
        }
    }

    let ranked_sections = [
        RankedSection {
            name: "rules",
            candidates: rules.iter().map(|c| render_item(c, markdown)).collect(),
        },
        RankedSection {
            name: "warnings",
            candidates: warnings.iter().map(|c| render_item(c, markdown)).collect(),
        },
        RankedSection {
            name: "decisions",
            candidates: decisions.iter().map(|c| render_item(c, markdown)).collect(),
        },
        RankedSection {
            name: "loadBearing",
            candidates: load_bearing
                .iter()
                .map(|c| render_item(c, markdown))
                .collect(),
        },
    ];

    // Budget mechanics (ADR §2): fixed quota shares, greedy fill in ranked
    // order, then the rules floor — rules never drop to zero while any
    // exist; floor order rules > warnings > decisions > loadBearing.
    let budget = settings.budget_tokens;
    let mut tokens_used: u32 = 0;
    let mut floors_engaged: Vec<String> = Vec::new();
    let mut sections: Vec<PrimerSection> = Vec::new();
    for ranked in &ranked_sections {
        let share = PRIMER_SECTION_SHARES
            .iter()
            .find(|(name, _)| *name == ranked.name)
            .map(|(_, share)| *share)
            .unwrap_or(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let quota = (f64::from(budget) * f64::from(share)).floor() as u32;
        let mut kept = Vec::new();
        let mut spent: u32 = 0;
        for item in &ranked.candidates {
            let cost = item_token_cost(&item.line);
            if spent + cost <= quota {
                spent += cost;
                kept.push(item.clone());
            }
        }
        tokens_used += spent;
        sections.push(PrimerSection {
            name: ranked.name.to_owned(),
            items: kept,
        });
    }

    // Rules floor: if rules ended empty while candidates exist, evict from
    // the lowest-priority non-empty section until the top rule fits in the
    // total budget.
    if sections[0].items.is_empty() && !ranked_sections[0].candidates.is_empty() {
        let top_rule = ranked_sections[0].candidates[0].clone();
        let top_cost = item_token_cost(&top_rule.line);
        for victim_index in (1..sections.len()).rev() {
            while tokens_used.saturating_add(top_cost) > budget {
                let Some(evicted) = sections[victim_index].items.pop() else {
                    break;
                };
                tokens_used = tokens_used.saturating_sub(item_token_cost(&evicted.line));
                skipped.budget_floor += 1;
            }
            if tokens_used.saturating_add(top_cost) <= budget {
                break;
            }
        }
        if tokens_used.saturating_add(top_cost) <= budget {
            tokens_used += top_cost;
            sections[0].items.push(top_rule);
            floors_engaged.push("rules".to_owned());
            degraded.push(PrimerDegradation {
                code: PRIMER_BUDGET_FLOOR_CODE.to_owned(),
                severity: "info".to_owned(),
                message: "budget too tight for proportional quotas; the rules floor evicted lower-priority items".to_owned(),
                repair: None,
            });
        }
    }

    let rendered_markdown = markdown.then(|| render_markdown(&sections));

    PrimerReport {
        schema: PRIMER_SCHEMA_V1.to_owned(),
        budget_tokens: budget,
        format: settings.format.as_str().to_owned(),
        cache_hit: false,
        db_generation,
        config_hash: settings.config_hash.clone(),
        sections,
        degraded,
        meta: PrimerMeta {
            tokens_used,
            skipped,
            floors_engaged,
        },
        rendered_markdown,
    }
}

fn render_markdown(sections: &[PrimerSection]) -> String {
    let mut out = String::from("# Workspace Primer\n");
    for section in sections {
        if section.items.is_empty() {
            continue;
        }
        let heading = match section.name.as_str() {
            "rules" => "Rules",
            "warnings" => "Warnings",
            "decisions" => "Decisions",
            "loadBearing" => "Load-Bearing",
            other => other,
        };
        out.push_str(&format!("\n## {heading}\n"));
        for item in &section.items {
            out.push_str(&format!("- {}\n", item.line));
        }
    }
    out
}

/// Fetch candidates + persisted centrality, assemble (or serve from the
/// `primer_cache` derived table), and persist the cold result. The cache key
/// is `(workspace_id, db_generation, config_hash, budget, format)`; any DB
/// generation advance invalidates. `refresh` forces re-assembly (still
/// deterministic ⇒ still byte-identical to a cold assembly).
pub fn run_primer(
    connection: &DbConnection,
    workspace_id: &str,
    settings: &PrimerSettings,
    refresh: bool,
) -> crate::db::Result<PrimerReport> {
    run_primer_with_persistence(connection, workspace_id, settings, refresh, true)
}

/// [`run_primer`] with an explicit persistence switch: `persist = false`
/// assembles without writing `primer_cache` rows (the `--no-persist`
/// read-only variant; still served from an existing cache row unless
/// `refresh` is set).
pub fn run_primer_with_persistence(
    connection: &DbConnection,
    workspace_id: &str,
    settings: &PrimerSettings,
    refresh: bool,
    persist: bool,
) -> crate::db::Result<PrimerReport> {
    // Opt-in-by-presence plus the `[memory]` config gate (bd-1bfwa.3
    // slice C): a resolvable env root with an existing store database
    // includes the global lane unless include_global/participate turned it
    // off; anything else (no HOME, no store) is lane-off, matching the
    // search seam's posture.
    let global_paths = if settings.global_lane_enabled {
        crate::core::global_store::default_global_store_paths_from_env().ok()
    } else {
        None
    };
    run_primer_with_global_lane(
        connection,
        workspace_id,
        settings,
        refresh,
        persist,
        global_paths.as_ref(),
    )
}

/// [`run_primer_with_persistence`] with explicit global-store paths so
/// tests can point the lane at a temp root (bd-1bfwa.3). `None` disables
/// the global lane outright.
pub fn run_primer_with_global_lane(
    connection: &DbConnection,
    workspace_id: &str,
    settings: &PrimerSettings,
    refresh: bool,
    persist: bool,
    global_paths: Option<&crate::core::global_store::GlobalStorePaths>,
) -> crate::db::Result<PrimerReport> {
    let db_generation = i64::try_from(
        connection
            .get_workspace_generation(workspace_id)?
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX);

    // The global store changes without bumping the workspace generation, so
    // the lane's content fingerprint must be part of the cache key or a
    // cached primer would silently omit fresh global rows. When the lane is
    // off the key is byte-identical to the pre-lane form.
    let (global_rows, global_lane_degraded) = load_global_lane_rows(global_paths);
    let cache_config_hash = if global_rows.is_empty() && global_lane_degraded.is_none() {
        settings.config_hash.clone()
    } else {
        format!(
            "{}+global:{}",
            settings.config_hash,
            global_store_lane_hash(&global_rows)
        )
    };

    if !refresh
        && let Some(cached) = connection.get_primer_cache(
            workspace_id,
            db_generation,
            &cache_config_hash,
            settings.budget_tokens,
            settings.format.as_str(),
        )?
        && let Ok(mut report) = serde_json::from_str::<PrimerReport>(&cached)
    {
        report.cache_hit = true;
        return Ok(report);
    }

    // Candidate fetch: non-tombstoned memories; decision-supersession via
    // explicit links (a decision is superseded when another memory points a
    // `supersedes` link at it).
    let memories = connection.list_memories(workspace_id, None, false)?;
    let mut candidates = Vec::with_capacity(memories.len());
    for memory in &memories {
        let superseded = if memory.kind == "decision" {
            connection
                .list_memory_links_for_memory(&memory.id, Some(MemoryLinkRelation::Supersedes))?
                .iter()
                .any(|link| link.dst_memory_id == memory.id)
        } else {
            false
        };
        candidates.push(PrimerCandidate {
            memory_id: memory.id.clone(),
            level: memory.level.clone(),
            kind: memory.kind.clone(),
            content: memory.content.clone(),
            confidence: memory.confidence,
            utility: memory.utility,
            importance: memory.importance,
            updated_at: memory.updated_at.clone(),
            provenance_uri: memory.provenance_uri.clone(),
            superseded,
            global_lane: false,
        });
    }
    merge_global_candidates(&mut candidates, &global_rows);

    let centrality_rows = load_persisted_centrality(connection, workspace_id)?;

    let mut report = assemble_primer(
        &candidates,
        centrality_rows.as_deref(),
        settings,
        db_generation,
    );
    if let Some(entry) = global_lane_degraded {
        report.degraded.push(entry);
    }
    report.degraded.push(PrimerDegradation {
        code: PRIMER_CACHE_COLD_CODE.to_owned(),
        severity: "info".to_owned(),
        message: "no primer cache row for this generation/config/budget/format; assembled fresh"
            .to_owned(),
        repair: None,
    });

    // Persist with cache_hit=false bytes so a later hit only flips the flag.
    let serialized = serde_json::to_string(&PrimerReport {
        cache_hit: false,
        ..report.clone()
    })
    .unwrap_or_default();
    if persist && !serialized.is_empty() {
        connection.put_primer_cache(
            workspace_id,
            db_generation,
            &cache_config_hash,
            settings.budget_tokens,
            settings.format.as_str(),
            &serialized,
            report.meta.tokens_used,
        )?;
    }
    Ok(report)
}

/// Read persisted centrality rows from the latest VALID memory-links graph
/// snapshot. Returns `None` (and the caller degrades) when the snapshot is
/// missing or non-valid. Deliberately ignores time-based expiry on this
/// path: assembly must be wall-clock-free so cache hits stay byte-identical.
/// Read the global-store rows for the primer lane (bd-1bfwa.3).
///
/// `None` paths or an absent store database mean lane-off (opt-in by
/// presence, mirroring the search seam). A present-but-unreadable store is
/// surfaced as a low-severity degraded entry instead of a silent skip.
fn load_global_lane_rows(
    global_paths: Option<&crate::core::global_store::GlobalStorePaths>,
) -> (Vec<crate::db::StoredMemory>, Option<PrimerDegradation>) {
    let Some(paths) = global_paths else {
        return (Vec::new(), None);
    };
    let inclusion = crate::core::global_store::resolve_global_inclusion(
        &crate::core::global_store::GlobalInclusionInput {
            store_present: paths.database_path.exists(),
            // The separate-store implementation has not yet grown a
            // repository participation row; default participation preserves
            // the opt-in-by-presence behavior (same note as the search seam).
            participating: true,
            config_enabled: true,
            no_global_flag: false,
        },
    );
    if !inclusion.included {
        return (Vec::new(), None);
    }
    match crate::core::global_store::read_global_store_memories(paths, false) {
        Ok(rows) => (rows, None),
        Err(error) => (
            Vec::new(),
            Some(PrimerDegradation {
                code: "scope_metadata_unavailable".to_owned(),
                severity: "low".to_owned(),
                message: format!(
                    "global store present but unreadable for the primer lane: {error}"
                ),
                repair: Some("ee doctor --json".to_owned()),
            }),
        ),
    }
}

/// Deterministic fingerprint of the global lane's content for the primer
/// cache key: the lane changes without bumping the workspace generation.
fn global_store_lane_hash(rows: &[crate::db::StoredMemory]) -> String {
    let mut keyed: Vec<(&str, &str, bool)> = rows
        .iter()
        .map(|row| {
            (
                row.id.as_str(),
                row.updated_at.as_str(),
                row.tombstoned_at.is_some(),
            )
        })
        .collect();
    keyed.sort_unstable();
    let mut hasher = blake3::Hasher::new();
    for (id, updated_at, tombstoned) in keyed {
        hasher.update(id.as_bytes());
        hasher.update(b"\0");
        hasher.update(updated_at.as_bytes());
        hasher.update(b"\0");
        hasher.update(if tombstoned { b"t" } else { b"-" });
        hasher.update(b"\0");
    }
    hasher
        .finalize()
        .to_hex()
        .chars()
        .take(16)
        .collect::<String>()
}

/// Merge global-store rows into the candidate pool (bd-1bfwa.3).
///
/// Exact-content twins resolve workspace-wins (the same rule
/// `promote_global` uses for duplicate detection), tombstoned rows are
/// excluded, and rows without their own provenance get their canonical
/// global-store address so the lane label always has a real URI. Global
/// rows carry no local supersession links.
fn merge_global_candidates(
    candidates: &mut Vec<PrimerCandidate>,
    global_rows: &[crate::db::StoredMemory],
) {
    if global_rows.is_empty() {
        return;
    }
    let workspace_content: BTreeSet<String> = candidates
        .iter()
        .map(|candidate| candidate.content.clone())
        .collect();
    let mut merged: Vec<PrimerCandidate> = global_rows
        .iter()
        .filter(|row| row.tombstoned_at.is_none())
        .filter(|row| !workspace_content.contains(&row.content))
        .map(|row| PrimerCandidate {
            memory_id: row.id.clone(),
            level: row.level.clone(),
            kind: row.kind.clone(),
            content: row.content.clone(),
            confidence: row.confidence,
            utility: row.utility,
            importance: row.importance,
            updated_at: row.updated_at.clone(),
            provenance_uri: row
                .provenance_uri
                .clone()
                .or_else(|| Some(format!("ee-mem://{}/{}", row.workspace_id, row.id))),
            superseded: false,
            global_lane: true,
        })
        .collect();
    merged.sort_by(|left, right| left.memory_id.cmp(&right.memory_id));
    candidates.extend(merged);
}

fn load_persisted_centrality(
    connection: &DbConnection,
    workspace_id: &str,
) -> crate::db::Result<Option<Vec<PrimerCentralityRow>>> {
    let Some(snapshot) =
        connection.get_latest_graph_snapshot(workspace_id, GraphSnapshotType::MemoryLinks)?
    else {
        return Ok(None);
    };
    if snapshot.status != GraphSnapshotStatus::Valid {
        return Ok(None);
    }
    let Ok(metrics) = serde_json::from_str::<serde_json::Value>(&snapshot.metrics_json) else {
        return Ok(None);
    };
    let nodes = metrics
        .pointer("/graph/nodes")
        .or_else(|| metrics.pointer("/nodes"))
        .and_then(serde_json::Value::as_array);
    let Some(nodes) = nodes else {
        return Ok(None);
    };
    let mut rows = Vec::with_capacity(nodes.len());
    for node in nodes {
        let Some(memory_id) = node
            .pointer("/memoryId")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        rows.push(PrimerCentralityRow {
            memory_id: memory_id.to_owned(),
            authority: node
                .pointer("/authority")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0),
            betweenness: node
                .pointer("/betweenness")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0),
        });
    }
    Ok(Some(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: u32, level: &str, kind: &str, content: &str) -> PrimerCandidate {
        PrimerCandidate {
            memory_id: format!("mem_{id:026}"),
            level: level.to_owned(),
            kind: kind.to_owned(),
            content: content.to_owned(),
            confidence: 0.8,
            utility: 0.7,
            importance: 0.6,
            updated_at: format!("2026-06-{:02}T00:00:00Z", (id % 27) + 1),
            provenance_uri: Some(format!("test://prov/{id}")),
            superseded: false,
            global_lane: false,
        }
    }

    fn global_row(id: u32, content: &str, provenance: Option<&str>) -> crate::db::StoredMemory {
        crate::db::StoredMemory {
            id: format!("mem_g{id:025}"),
            workspace_id: "ws_global".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.8,
            importance: 0.5,
            provenance_uri: provenance.map(str::to_owned),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "v1".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-06-01T00:00:00Z".to_owned(),
            updated_at: "2026-06-02T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    fn settings(budget: u32) -> PrimerSettings {
        PrimerSettings {
            global_lane_enabled: true,
            budget_tokens: budget,
            format: PrimerFormat::Markdown,
            config_hash: primer_config_hash(budget, true),
            redact_secrets: true,
        }
    }

    fn corpus() -> Vec<PrimerCandidate> {
        let mut out = Vec::new();
        for index in 0..12 {
            out.push(candidate(
                index,
                "procedural",
                "rule",
                &format!("Rule {index:02}: always run the verify script before pushing changes."),
            ));
        }
        for index in 12..20 {
            out.push(candidate(
                index,
                "episodic",
                "failure",
                &format!(
                    "Failure {index:02}: release broke when goldens were regenerated locally."
                ),
            ));
        }
        for index in 20..26 {
            out.push(candidate(
                index,
                "semantic",
                "decision",
                &format!("Decision {index:02}: keep the runtime on asupersync, never tokio."),
            ));
        }
        for index in 26..30 {
            out.push(candidate(
                index,
                "semantic",
                "fact",
                &format!("Fact {index:02}: the db layer is fsqlite through sqlmodel."),
            ));
        }
        out
    }

    fn centrality_for(corpus: &[PrimerCandidate]) -> Vec<PrimerCentralityRow> {
        corpus
            .iter()
            .map(|candidate| PrimerCentralityRow {
                memory_id: candidate.memory_id.clone(),
                authority: 0.5,
                betweenness: 0.1,
            })
            .collect()
    }

    #[test]
    fn quota_math_under_tight_typical_and_huge_budgets() {
        let corpus = corpus();
        let rows = centrality_for(&corpus);
        let typical = assemble_primer(&corpus, Some(&rows), &settings(600), 7);
        // Typical budget fills every section under its quota share.
        assert!(!typical.sections[0].items.is_empty(), "rules populated");
        assert!(!typical.sections[1].items.is_empty(), "warnings populated");
        assert!(typical.meta.tokens_used <= 600);

        let huge = assemble_primer(&corpus, Some(&rows), &settings(100_000), 7);
        // Huge budget admits every candidate exactly once (dedup holds).
        let total: usize = huge.sections.iter().map(|s| s.items.len()).sum();
        let mut ids: Vec<&str> = huge
            .sections
            .iter()
            .flat_map(|s| s.items.iter().map(|i| i.memory_id.as_str()))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "no memory appears twice");

        let tight = assemble_primer(&corpus, Some(&rows), &settings(120), 7);
        assert!(tight.meta.tokens_used <= 120);
        // Tight budgets keep a strict per-section subset of typical.
        for (tight_section, typical_section) in tight.sections.iter().zip(&typical.sections) {
            assert_eq!(
                tight_section.items[..],
                typical_section.items[..tight_section.items.len()],
                "budget sweep yields a selection subset for {}",
                tight_section.name
            );
        }
    }

    #[test]
    fn rules_floor_engages_under_starvation_budget() {
        let corpus = corpus();
        let rows = centrality_for(&corpus);
        // A budget so small the 40% rules quota cannot hold one line.
        let report = assemble_primer(&corpus, Some(&rows), &settings(30), 7);
        assert!(
            !report.sections[0].items.is_empty(),
            "rules never drop to zero while any exist"
        );
        assert_eq!(report.meta.floors_engaged, vec!["rules".to_owned()]);
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == PRIMER_BUDGET_FLOOR_CODE)
        );
        assert!(report.meta.tokens_used <= 30);
    }

    #[test]
    fn deterministic_output_and_tie_breaks() {
        let mut corpus = corpus();
        let rows = centrality_for(&corpus);
        let first = assemble_primer(&corpus, Some(&rows), &settings(600), 7);
        corpus.reverse();
        let second = assemble_primer(&corpus, Some(&rows), &settings(600), 7);
        assert_eq!(first, second, "input order never changes output");
        // Equal-score rules order by ascending memory id.
        let rule_ids: Vec<&str> = first.sections[0]
            .items
            .iter()
            .map(|item| item.memory_id.as_str())
            .collect();
        let mut sorted = rule_ids.clone();
        sorted.sort_unstable();
        assert_eq!(rule_ids, sorted);
    }

    #[test]
    fn graph_absent_path_omits_load_bearing_and_degrades() {
        let corpus = corpus();
        let report = assemble_primer(&corpus, None, &settings(600), 7);
        assert!(report.sections[3].items.is_empty(), "loadBearing omitted");
        assert_eq!(report.degraded.len(), 1);
        assert_eq!(report.degraded[0].code, PRIMER_GRAPH_UNAVAILABLE_CODE);
        assert_eq!(report.degraded[0].severity, "info");
    }

    #[test]
    fn redaction_skips_are_counted_never_leaked() {
        let mut corpus = corpus();
        corpus.push(candidate(
            90,
            "procedural",
            "rule",
            "Use key -----BEGIN PRIVATE KEY----- abc123 to deploy.",
        ));
        let report = assemble_primer(&corpus, None, &settings(100_000), 7);
        assert_eq!(report.meta.skipped.redaction, 1);
        assert!(
            !report
                .sections
                .iter()
                .flat_map(|s| &s.items)
                .any(|item| item.line.contains("PRIVATE KEY")),
            "secret content never renders"
        );
        // With redaction disabled the same candidate is admitted.
        let mut open_settings = settings(100_000);
        open_settings.redact_secrets = false;
        let open = assemble_primer(&corpus, None, &open_settings, 7);
        assert_eq!(open.meta.skipped.redaction, 0);
    }

    #[test]
    fn superseded_decisions_are_excluded() {
        let mut corpus = corpus();
        for candidate in &mut corpus {
            if candidate.kind == "decision" && candidate.memory_id.ends_with("21") {
                candidate.superseded = true;
            }
        }
        let report = assemble_primer(&corpus, None, &settings(100_000), 7);
        assert!(
            !report.sections[2]
                .items
                .iter()
                .any(|item| item.memory_id.ends_with("21")),
            "superseded decision chain links never render"
        );
    }

    #[test]
    fn markdown_rendering_is_provenance_suffixed_and_stable() {
        let corpus = corpus();
        let report = assemble_primer(&corpus, None, &settings(600), 7);
        let rendered = report.rendered_markdown.as_deref().expect("markdown");
        assert!(rendered.starts_with("# Workspace Primer\n"));
        assert!(rendered.contains("## Rules"));
        assert!(
            rendered
                .lines()
                .filter(|l| l.starts_with("- "))
                .all(|line| { line.trim_end().ends_with(']') && line.contains("[mem_") })
        );
        // Short ref shape.
        assert_eq!(
            primer_short_memory_ref("mem_01HQ3K5ZABCDEFGH"),
            "mem_01HQ3K5Z"
        );
    }

    // --- DB-backed cache tests -------------------------------------------

    fn wrapper_test_db() -> (crate::db::DbConnection, String) {
        let connection = crate::db::DbConnection::open_memory().expect("open in-memory db");
        connection.migrate().expect("migrate");
        let workspace_id = format!("wsp_{:026}", 9);
        connection
            .insert_workspace(
                &workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/primer-test".to_owned(),
                    name: Some("primer-test".to_owned()),
                },
            )
            .expect("insert workspace");
        (connection, workspace_id)
    }

    fn insert_rule(connection: &crate::db::DbConnection, workspace_id: &str, id: u32) {
        connection
            .insert_memory(
                &format!("mem_{id:026}"),
                &crate::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!("Primer cache rule {id:02}: run verify before pushing."),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("test://primer".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec!["primer-test".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert memory");
    }

    #[test]
    fn cache_hit_is_byte_identical_and_generation_invalidates() {
        let (connection, workspace_id) = wrapper_test_db();
        insert_rule(&connection, &workspace_id, 1);
        let settings = settings(600);

        let cold =
            run_primer_with_global_lane(&connection, &workspace_id, &settings, false, true, None)
                .expect("cold run");
        assert!(!cold.cache_hit);
        assert!(
            cold.degraded
                .iter()
                .any(|entry| entry.code == PRIMER_CACHE_COLD_CODE)
        );

        let warm =
            run_primer_with_global_lane(&connection, &workspace_id, &settings, false, true, None)
                .expect("warm run");
        assert!(warm.cache_hit);
        // Byte identity everywhere except the response-time cache flag.
        let mut warm_normalized = warm.clone();
        warm_normalized.cache_hit = false;
        assert_eq!(warm_normalized, cold);
        assert_eq!(warm.rendered_markdown, cold.rendered_markdown);

        // --refresh forces re-assembly and still matches the cold bytes.
        let refreshed =
            run_primer_with_global_lane(&connection, &workspace_id, &settings, true, true, None)
                .expect("refresh run");
        assert!(!refreshed.cache_hit);
        assert_eq!(refreshed.sections, cold.sections);

        // A DB generation advance invalidates: the next run is cold again
        // and sees the new memory.
        insert_rule(&connection, &workspace_id, 2);
        let after_write =
            run_primer_with_global_lane(&connection, &workspace_id, &settings, false, true, None)
                .expect("post-write run");
        assert!(!after_write.cache_hit);
        assert!(after_write.db_generation > cold.db_generation);
        assert!(
            after_write.sections[0]
                .items
                .iter()
                .any(|item| item.memory_id.ends_with("02")),
            "new memory appears after invalidation"
        );
    }

    // ===== global-lane tests (bd-1bfwa.3 slice B) =====

    #[test]
    fn merge_global_candidates_dedupes_workspace_wins_and_labels_lane() {
        let mut candidates = vec![candidate(1, "procedural", "rule", "shared rule content")];
        let rows = vec![
            global_row(1, "shared rule content", Some("ee-mem://ws_a/mem_1")),
            global_row(2, "unique global rule", None),
        ];
        merge_global_candidates(&mut candidates, &rows);

        assert_eq!(
            candidates.len(),
            2,
            "exact-content twin resolves workspace-wins"
        );
        let merged = &candidates[1];
        assert!(merged.global_lane);
        assert!(!merged.superseded);
        assert_eq!(merged.content, "unique global rule");
        assert_eq!(
            merged.provenance_uri.as_deref(),
            Some(format!("ee-mem://ws_global/{}", merged.memory_id).as_str()),
            "rows without provenance get their canonical global-store address"
        );
    }

    /// bd-1bfwa.4 precedence-determinism property: for arbitrary
    /// overlapping workspace/global pools, the workspace row always wins an
    /// exact-content twin, tombstoned rows never enter, and the merged
    /// output is byte-identical regardless of global-row insertion order.
    #[test]
    fn merge_global_candidates_precedence_is_order_independent() {
        use proptest::prelude::*;
        let mut runner =
            proptest::test_runner::TestRunner::new(proptest::test_runner::Config::with_cases(64));
        let strategy = (
            proptest::collection::vec(0u32..12, 0..6),
            proptest::collection::vec((0u32..12, proptest::bool::ANY), 0..8),
            proptest::collection::vec(proptest::num::usize::ANY, 0..8),
        );
        runner
            .run(&strategy, |(workspace_ids, global_specs, shuffle_seed)| {
                let base: Vec<PrimerCandidate> = workspace_ids
                    .iter()
                    .map(|id| candidate(*id, "procedural", "rule", &format!("content {id}")))
                    .collect();
                // Ids are unique in a real store (primary key); dedupe the
                // generated specs the same way (last spec wins).
                let unique_specs: std::collections::BTreeMap<u32, bool> =
                    global_specs.iter().copied().collect();
                let mut rows: Vec<crate::db::StoredMemory> = unique_specs
                    .iter()
                    .map(|(id, tombstoned)| {
                        let mut row = global_row(*id, &format!("content {id}"), None);
                        if *tombstoned {
                            row.tombstoned_at = Some("2026-06-03T00:00:00Z".to_owned());
                        }
                        row
                    })
                    .collect();

                let mut in_given_order = base.clone();
                merge_global_candidates(&mut in_given_order, &rows);

                // Deterministic shuffle from the seed vector.
                for (index, seed) in shuffle_seed.iter().enumerate() {
                    let row_count = rows.len();
                    if row_count > 1 {
                        let swap = seed % row_count;
                        rows.swap(index % row_count, swap);
                    }
                }
                let mut in_shuffled_order = base.clone();
                merge_global_candidates(&mut in_shuffled_order, &rows);

                prop_assert_eq!(
                    &in_given_order,
                    &in_shuffled_order,
                    "merge output must not depend on global-row order"
                );

                let workspace_content: std::collections::BTreeSet<&str> =
                    base.iter().map(|c| c.content.as_str()).collect();
                for merged in &in_given_order {
                    if merged.global_lane {
                        prop_assert!(
                            !workspace_content.contains(merged.content.as_str()),
                            "workspace twin must win over the global row"
                        );
                    }
                }
                let lane_count = in_given_order.iter().filter(|c| c.global_lane).count();
                let expected: std::collections::BTreeSet<u32> = global_specs
                    .iter()
                    .filter(|(id, tombstoned)| !*tombstoned && !workspace_ids.contains(id))
                    .map(|(id, _)| *id)
                    .collect();
                prop_assert_eq!(
                    lane_count,
                    expected.len(),
                    "every live, non-twin global row enters exactly once"
                );
                Ok(())
            })
            .expect("precedence property holds");
    }

    #[test]
    fn merge_global_candidates_skips_tombstoned_rows() {
        let mut candidates = vec![candidate(1, "procedural", "rule", "workspace rule")];
        let mut tombstoned = global_row(3, "dead global rule", None);
        tombstoned.tombstoned_at = Some("2026-06-03T00:00:00Z".to_owned());
        merge_global_candidates(&mut candidates, &[tombstoned]);
        assert_eq!(
            candidates.len(),
            1,
            "tombstoned global rows never enter the pool"
        );
    }

    #[test]
    fn global_lane_items_carry_global_store_provenance_source() {
        let mut lane_candidate = candidate(7, "procedural", "rule", "global content");
        lane_candidate.global_lane = true;
        let item = render_item(&lane_candidate, false);
        assert_eq!(item.provenance.len(), 1);
        assert_eq!(item.provenance[0].source_type, "global_store");

        let workspace_item = render_item(&candidate(8, "procedural", "rule", "local"), false);
        assert_eq!(
            workspace_item.provenance[0].source_type,
            "memory_provenance"
        );
    }

    #[test]
    fn global_store_lane_hash_is_order_independent_and_content_bound() {
        let row_a = global_row(1, "a", None);
        let row_b = global_row(2, "b", None);
        let forward = global_store_lane_hash(&[row_a.clone(), row_b.clone()]);
        let reversed = global_store_lane_hash(&[row_b.clone(), row_a.clone()]);
        assert_eq!(forward, reversed, "hash must not depend on row order");

        let mut touched = row_b;
        touched.updated_at = "2026-06-09T00:00:00Z".to_owned();
        let shifted = global_store_lane_hash(&[row_a, touched]);
        assert_ne!(forward, shifted, "updated_at changes must change the hash");
    }

    #[test]
    fn global_rules_compete_in_the_rules_section() {
        let mut candidates = vec![candidate(1, "procedural", "rule", "workspace rule text")];
        merge_global_candidates(
            &mut candidates,
            &[global_row(
                2,
                "global rule text",
                Some("ee-mem://ws_a/mem_2"),
            )],
        );
        let report = assemble_primer(&candidates, None, &settings(600), 1);
        let rules = &report.sections[0];
        assert_eq!(rules.name, "rules");
        assert_eq!(rules.items.len(), 2, "global rule competes inside rules");
        assert!(
            rules.items.iter().any(|item| item
                .provenance
                .first()
                .is_some_and(|p| p.source_type == "global_store")),
            "the lane label survives assembly"
        );
    }
}
