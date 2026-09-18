use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Disposition {
    MustFix,
    Allowed,
}

#[derive(Clone, Copy, Debug)]
struct InventoryRule {
    id: &'static str,
    file: &'static str,
    /// Enclosing function this rule is scoped to, or `None` for file scope.
    ///
    /// bd-apvhh, ruled 2026-09-17: the key is (file, enclosing function,
    /// matched construct) WITH A COUNT, not a per-occurrence identity. Four
    /// identical `return Ok(Vec::new());` in one function are ONE finding with
    /// multiplicity four; separating them would require position, and position
    /// is the thing that drifts — 167 of 177 anchors measured stale for
    /// bd-d8trk, and bd-blj5n is a test red ~4 months over hardcoded
    /// file:line.
    ///
    /// The function narrows the group. It does NOT replace the construct: a
    /// rule still matches on its fragment, so two different kinds of silent
    /// fallback in one function cannot share a slot. Multiplicity within the
    /// group is carried by the match-count ledger, which fails in BOTH
    /// directions.
    function: Option<&'static str>,
    fragment: &'static str,
    disposition: Disposition,
    follow_up: Option<&'static str>,
    reason: &'static str,
}

#[derive(Clone, Copy, Debug)]
struct ManualFinding {
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    follow_up: &'static str,
    reason: &'static str,
}

#[derive(Clone, Debug)]
struct SourceFinding {
    file: String,
    line: usize,
    text: String,
    context: String,
    /// Enclosing function, resolved by brace depth. `None` only at module
    /// scope.
    function: Option<String>,
}

const FOLLOW_UP_BEADS: &[&str] = &[
    "eidetic_engine_cli-sos5.2",
    "eidetic_engine_cli-sos5.3",
    "eidetic_engine_cli-sos5.4",
    "eidetic_engine_cli-sos5.7",
    "eidetic_engine_cli-ogy9",
    // bd-1jpg7: the no-echo ingest guard is disabled whenever
    // apply_join_first_sync_events cannot read its own origin node id.
    //
    // Before adding another id here, read bd-epvc1: eight must_fix rules
    // already name a follow-up bead while owning ZERO findings, so they watch
    // nothing. A must_fix that is shadowed into silence by an earlier fragment
    // is worse than no rule, because the bead makes it look tracked. The rule
    // that names this bead was checked against the match-count ledger and owns
    // its site.
    "bd-1jpg7",
    // bd-zjcx6: the mesh responder swallows discovery-list LOAD errors, so a
    // denylist that exists but cannot be read becomes an empty denylist -- and
    // in auto_admit and service_tag the denylist is the only per-requester
    // exclusion there is.
    "bd-zjcx6",
];

const INVENTORY_RULES: &[InventoryRule] = &[
    // bd-apvhh: the first two function-scoped group rules (ruled 2026-09-17).
    // Both cover findings that landed after the unclassified baseline was
    // recorded at 8662ae0cb and that had main red.
    allowed_in(
        "NSF-CLI-CONTEXT-DELTA-EVIDENCE-PROJECTION",
        "src/cli/context_delta_evidence.rs",
        "from_ledger",
        ".unwrap_or_default()",
        "Optional projection fields of a verified prior-pack evidence item. The          default of a serde_json::Value is Value::Null, so absence is PRESERVED          rather than replaced by a fabricated value -- the distinction that          separates a benign default from a silent fallback. The REQUIRED field          does not take this path: entityRevision uses ok_or_else and errors out          when missing or non-canonical. Nine occurrences in this one function,          carried as multiplicity in the match-count ledger rather than as nine          positional keys; two of them are the identical line `.unwrap_or_default(),`          and nothing but position could separate those.",
    ),
    allowed_in(
        "NSF-CORE-ASK-CANDIDATES-ZERO-LIMIT",
        "src/core/ask_candidates.rs",
        "select_candidates",
        "return Ok(Vec::new());",
        "A caller asking for zero candidates gets zero. This early return sits          AFTER the explicit error returns for InvalidConfidence and          AmbiguousSource, so it cannot mask either: an invalid request errors          before reaching it. An empty result for limit == 0 is the honest answer,          not a swallowed failure.",
    ),
    // bd-apvhh burn-down, tranche 1 (2026-09-17): the sixteen files that had
    // NO rule at all and exactly one unclassified finding each. Sequenced first
    // per the ruling -- a file with no rule is where a group key has the least
    // prior art to lean on, so each of these was read in source rather than
    // matched by shape. Every one is function-scoped; none is file-scoped.
    //
    // All sixteen came out ALLOWED, which is a result worth distrusting on its
    // face, so: the population is biased benign by construction. A file whose
    // whole body contains exactly one high-risk line is usually a contained
    // helper. The files with ten and seven findings are where a real fallback
    // is likelier, and they are not in this tranche.
    allowed_in(
        "NSF-CACHE-PACK-L2-MISSING-CACHE-DIR",
        "src/cache/pack_l2.rs",
        "entry_candidates",
        "Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),",
        "A cache root that was never created holds no entries. The arm matches NotFound ONLY; every other io error falls through to the next arm and becomes PackL2CacheError::Io with the path and the operation, so a permission failure or a corrupt directory is never read as an empty cache.",
    ),
    allowed_in(
        "NSF-CORE-CONTENTION-P99-UNREACHABLE-DEFAULT",
        "src/core/contention.rs",
        "build_contention_report",
        "inputs.lock_wait_ms_p99.unwrap_or_default()",
        "UNREACHABLE, and that is the whole justification. This sits in the write-owner-unavailable branch, where status is WriteOwnerStatus::default() -- queue_depth 0 and max_wait_ms 0. classify_write_lock can only return >= Warm from that status via p99_ms.is_some_and(..), because WRITE_QUEUE_WARM is 1 and the max_wait_ms test is `> 0`. So inside `if posture >= Warm` the Option is necessarily Some and the formatted message cannot print a fabricated `0 ms`. FRAGILE ON PURPOSE: a nonzero field in WriteOwnerStatus::default(), or WRITE_QUEUE_WARM becoming 0, makes the default reachable and turns this into a fabricated measurement.",
    ),
    allowed_in(
        "NSF-CORE-HEALTH-DEBT-COUNTS-DECLARED-DEGRADED",
        "src/core/health.rs",
        "health_scorecard_evidence",
        ".map(|report| &report.summary.class_counts)",
        "An absent memory-debt report yields empty class counts, and those counts ARE scored (stale_anchor_count feeds the weighted score and is rendered as staleAnchors=N), so on its own this would be a missing input flattering the result. It is honest only because the caller declares the absence: the Err arm of run_memory_debt_doctor pushes HealthScorecardDegradation `health_scorecard_debt_unavailable` at severity warning, carrying the error text and a repair command, and None is produced nowhere else. DELETE THAT PUSH AND THIS BECOMES A SILENT FALLBACK.",
    ),
    allowed_in(
        "NSF-CORE-MEMORY-DRIFT-ROLLBACK-DETAIL",
        "src/core/memory_drift.rs",
        "build_memory_drift_report_with_connection",
        ".map(|error| format!(\"; rollback error: {error}\"))",
        "The default is the empty SUFFIX appended when the rollback succeeded, not an erased value. `.err()` is Some only when rollback itself failed, so an empty string means there was no second failure to report -- which is what the surrounding message should then say. The commit failure that triggered this path is still returned as DomainError::Storage regardless.",
    ),
    allowed_in(
        "NSF-CORE-ORIENT-UNTAGGED-MEMORY",
        "src/core/orient.rs",
        "orient_fast_relevant_content",
        "tags_by_memory.get(&hit.doc_id).cloned().unwrap_or_default()",
        "A map miss means the memory has no tags, not that tags failed to load. tags_by_memory comes from get_memory_tags_batch over exactly the hit ids, its error is propagated as orient_fast_relevant_unavailable before this line runs, and the underlying SELECT over memory_tags returns rows only for memories that have tags -- so an untagged memory is absent from the map by construction. Empty preserves that absence.",
    ),
    allowed_in(
        "NSF-CORE-RETRIEVAL-AFFINITY-AS-OF-UNREACHABLE",
        "src/core/retrieval_affinity.rs",
        "materialize_retrieval_affinity_snapshot",
        ".map(|(_, _, _, last_event_at)| last_event_at.as_str())",
        "UNREACHABLE: `if edges.is_empty() { return Ok(AffinityMaterialization::Cold); }` sits directly above, so the iterator is non-empty and `.max()` is always Some. The empty-edge case has its own honest outcome (Cold) rather than an empty as_of timestamp.",
    ),
    allowed_in(
        "NSF-CORE-SESSION-BUDGET-MISSING-LEDGER",
        "src/core/session_budget.rs",
        "load_ledger_rows_with_max_bytes",
        "Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),",
        "A ledger file that was never written has no rows. NotFound ONLY; the following arm turns every other io error into SessionBudgetRecordError::io with the path, so an unreadable ledger is not reported as an empty one.",
    ),
    allowed_in(
        "NSF-CORE-TAG-BACKFILL-UNTAGGED-MEMORY",
        "src/core/tag_backfill.rs",
        "collect_candidates",
        "let existing_tags = tags_by_memory.get(&memory.id).cloned().unwrap_or_default();",
        "Same construction as the orient rule: ids are taken from the exact `stored` population being mapped, get_memory_tags_batch's error is already converted to DomainError::Storage with a repair command, and the batch returns keys only for memories that have tags. A miss is a genuinely untagged memory, which is precisely the population a backfill is looking for.",
    ),
    allowed_in(
        "NSF-CORE-UNSAFE-CLAIM-NO-CANDIDATES",
        "src/core/unsafe_claim_planner.rs",
        "recommend_unsafe_claim_alternates",
        ".map(|candidate| candidate.next_command_actions.clone())",
        "No candidates means no next-command actions to offer, and an empty action list is the honest rendering of that. The empty case is not left implicit elsewhere either: recommended_action_for(&candidates) decides the recommended action over the same slice.",
    ),
    allowed_in(
        "NSF-GRAPH-EMPTY-WORKSPACE-FILTER",
        "src/graph/mod.rs",
        "typed_memory_graph_edges",
        "if workspace_filter.is_some_and(BTreeSet::is_empty) {",
        "An explicitly EMPTY workspace filter selects no workspaces, so zero edges is the correct answer rather than a swallowed query failure. is_some_and is load-bearing: None means no filter at all and does NOT take this path, it falls through to the real query whose errors propagate as GraphError.",
    ),
    allowed_in(
        "NSF-MESH-BOOTSTRAP-CAPABILITY-REJECTED",
        "src/mesh/bootstrap_envelope.rs",
        "decode_envelope",
        "let capability_token = probe.capability.as_str().unwrap_or_default();",
        "FAILS CLOSED, which is the property that matters on a trust boundary. A capability field that is not a JSON string becomes \"\", which equals neither BootstrapCapability::Hello.token() nor Join.token(), so the very next branch returns BootstrapEnvelopeError::UnsupportedCapability and reports the observed value. The default cannot admit a malformed envelope; it can only route it to the rejection it already deserved.",
    ),
    allowed_in(
        "NSF-MESH-HELLO-JSON-STRING-LIST",
        "src/mesh/hello.rs",
        "json_string_list",
        ".filter_map(serde_json::Value::as_str)",
        "The function's entire contract is to read a list of strings out of JSON and yield what is there. A field that is absent, null, or not an array yields no strings, and an empty Vec<String> is that answer rather than a substitute for one.",
    ),
    allowed_in(
        "NSF-MESH-IDP-METHODS-MOST-RESTRICTIVE",
        "src/mesh/idp.rs",
        "classify_oidc_provider",
        "if methods.iter().any(|method| *method == \"none\") {",
        "FAILS CLOSED. An absent or non-array token_endpoint_auth_methods yields an empty list, which matches neither the `none` test nor the client_secret tests, so classification falls through to IdpProviderCapability::Unsupported -- the most restrictive of the three outcomes. Absence can never be read as SecretlessPublic, which is the one that would weaken a trust decision.",
    ),
    allowed_in(
        "NSF-MODELS-MEMORY-NO-TYPED-SIDECAR",
        "src/models/memory.rs",
        "typed_memory_field_names",
        ".map(typed_memory_valid_field_names)",
        "Documented contract, stated in the doc comment directly above: kinds without a v2 typed sidecar return an empty list, and that same vocabulary is published as ee.memory.typed_fields.v2. An empty field list is the published answer for such a kind, not a stand-in for a lookup that failed.",
    ),
    allowed_in(
        "NSF-MODELS-RECORDER-SECRET-KEY-SCAN",
        "src/models/recorder.rs",
        "contains_secret_like_marker",
        ".rsplit(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')))",
        "UNREACHABLE: str::rsplit always yields at least one element, so `.next()` is always Some. Were it ever reachable, the direction is still safe -- \"\" matches none of the api_key/password/token/secret names, so the scan would decline to claim a secret it had not identified rather than assert one.",
    ),
    allowed_in(
        "NSF-SEARCH-RULE-SCOPE-FIRST-SEGMENT",
        "src/search/mod.rs",
        "normalize_rule_scope_pattern",
        "let first_segment = portable.split('/').next().unwrap_or_default();",
        "UNREACHABLE: str::split always yields at least one element, so `.next()` is always Some -- an empty pattern yields one empty segment rather than no segments. The genuinely absent case is handled earlier and explicitly by `let Some(pattern) = pattern else { return Ok(None) }`.",
    ),
    // bd-apvhh burn-down, tranche 2 (2026-09-17): the provable part of the 58
    // findings in the 14 remaining no-rule files.
    //
    // THIS POPULATION IS NOT BIASED BENIGN, unlike tranche 1, and the allow
    // rate here should be read differently. These files carry identity fields,
    // plan fields and decision fields where an empty string is a FABRICATED
    // VALUE rather than a preserved absence. What is classified below is only
    // what was proved; the rest stays as declared debt on purpose.
    allowed_in(
        "NSF-CONTRADICTION-KEEP-UNREACHABLE",
        "src/core/contradiction_detect.rs",
        "plan_conflict_resolution",
        "chosen: format!(\"keep {}\", keep.as_deref().unwrap_or_default()),",
        "UNREACHABLE. `needs_keep` is matches!(request.verb, Supersede | RejectOne), and when it holds the branch either returns InvalidRequest (missing --keep, or a --keep naming neither side) or produces (Some(keep), Some(lose)). The (None, None) else is reached only for the OTHER verbs, and this site sits inside the Supersede match arm, so keep is necessarily Some here and the plan can never say `keep ` with no id. FRAGILE IN A SPECIFIC WAY: the guard and the match arms are two separate lists of the same two verbs. Add a third verb to one and not the other and this default becomes reachable.",
    ),
    allowed_in(
        "NSF-CONTRADICTION-LOSE-UNREACHABLE",
        "src/core/contradiction_detect.rs",
        "plan_conflict_resolution",
        "memory_id: lose.clone().unwrap_or_default(),",
        "UNREACHABLE by the same needs_keep argument, and this is the site where reachability would matter most: it is the memory_id of a planned ExpireMemory action, so a default here would plan an expiry against the empty id. RejectOne is in needs_keep, so lose is Some whenever this arm runs.",
    ),
    allowed_in(
        "NSF-CONTRADICTION-KEEP-REJECT-ONE-UNREACHABLE",
        "src/core/contradiction_detect.rs",
        "plan_conflict_resolution",
        "\"keep {}; reject the other side\",",
        "UNREACHABLE, same proof: the RejectOne match arm only runs for a verb that needs_keep already admitted, so keep is Some.",
    ),
    allowed_in(
        "NSF-CORE-DECIDE-NO-TYPED-SIDECAR",
        "src/core/decide.rs",
        "decision_fields_from_memory",
        ".map(|raw| typed_memory_fields_from_json(&kind, raw))",
        "A decision memory with no typed v2 sidecar has no typed fields, and an empty field map is that. The distinction that makes it safe is two lines down: a sidecar that EXISTS but does not parse is mapped to decide_storage_error and returned with `?`, so malformed and absent take different paths. FRAGMENT CHOSEN FOR REACH, not readability: the obvious fragment (the decide_storage_error message) also lands inside the context window of `chosen = string_field(..).unwrap_or_default()` three lines below, and this rule absorbed that site on the first attempt. `chosen` and `rationale` are deliberately NOT classified here.",
    ),
    allowed_in(
        "NSF-CORE-DECIDE-MISSING-LIST-FIELD",
        "src/core/decide.rs",
        "string_list_field",
        ".filter_map(non_empty_trimmed)",
        "A list field that is absent, or present but not an array, yields no strings. The helper already drops empty and whitespace-only entries through non_empty_trimmed, so an empty Vec and a Vec of blanks are the same answer by construction rather than by this default.",
    ),
    allowed_in(
        "NSF-CORE-DECIDE-NO-STORE",
        "src/core/decide.rs",
        "load_decisions",
        "if !scope.database_path.exists() {",
        "A workspace with no decision store has recorded no decisions. Existence is checked before opening, and once the file exists every open or query failure propagates through open_decide_database_read_only(..)? -- so an unreadable store is never reported as a workspace with no decisions.",
    ),
    allowed_in(
        "NSF-MCP-OPTIONAL-LIST-ABSENT",
        "src/mcp.rs",
        "optional_string_list",
        "fn optional_string_list(arguments: &Value, names: &[&str])",
        "The function is optional_string_list and an omitted argument is its declared input. Malformed input does NOT take this path: a value that is neither a string nor an array returns Err naming the argument. The fragment is the signature line rather than the `else` because the three early returns sit within four lines of each other, so a fragment nearer the body makes this rule absorb the null and empty-string sites too -- which it did on the first attempt, leaving the next rule owning nothing.",
    ),
    allowed_in(
        "NSF-MCP-OPTIONAL-LIST-NULL",
        "src/mcp.rs",
        "optional_string_list",
        "if value.is_null() {",
        "Explicit JSON null is the caller spelling absence out loud, which is the same answer as omitting the argument. The malformed case still errors.",
    ),
    allowed_in(
        "NSF-MCP-OPTIONAL-LIST-EMPTY-STRING",
        "src/mcp.rs",
        "optional_string_list",
        "if single.is_empty() {",
        "An empty string in the single-value spelling carries no list entries, so it yields none rather than a list holding one empty string. That is the choice that keeps the two spellings -- string and array -- agreeing on what emptiness means.",
    ),
    allowed_in(
        "NSF-CORE-GLOBAL-STORE-NO-DATABASE",
        "src/core/global_store.rs",
        "read_global_store_memories",
        "if !paths.database_path.exists() {",
        "No user-global store file means no user-global memories. Checked before opening; once the file exists, open_file_read_only failures become an Err with the reason.",
    ),
    allowed_in(
        "NSF-CORE-GLOBAL-STORE-NO-WORKSPACE-ROW",
        "src/core/global_store.rs",
        "read_global_store_memories",
        "error.message()",
        "The global workspace row being ABSENT means nothing has been promoted to the global store yet, so there are no memories to list. A resolution ERROR is a different path: it is mapped to \"failed to resolve global workspace row\" and returned with `?` just above. That message line is one line too far above the finding to fall inside its context window, which is why this fragment names the error accessor instead.",
    ),
    allowed_in(
        "NSF-CORE-MEMORY-DEBT-PER-MEMORY-MAPS",
        "src/core/memory_debt.rs",
        "build_memory_debt_report",
        "let memory_signals = signals.get(&memory.id).cloned().unwrap_or_default();",
        "Two adjacent per-memory map lookups (signals and anchor_state), carried as one group with multiplicity 2 rather than two positional keys, because they are one decision about the same construct in the same loop. Both maps are built over the same `memories` population with their errors already propagated by `?`, and both hold keys only for memories that HAVE signals or anchors -- so a miss is a memory with none, and the default says exactly that. A memory with no recorded contradictions genuinely has zero, which is why the default is not a fabricated measurement here the way an absent whole-report would be.",
    ),
    // NOT MINE, AND IT HAD MAIN RED. af22f1611 added these two lines to
    // src/core/ask_corpus.rs, a product file with NO row in the unclassified
    // baseline, so the shrink-only ratchet saw 2 against an allowance of 0 and
    // failed. Classified rather than given a baseline row on purpose: adding a
    // row would record the debt as accepted, which is the weakening this
    // ratchet exists to refuse. Flagged to the ask lane in the same breath --
    // if they disagree with the judgement, replace the rule, do not baseline it.
    allowed_in(
        "NSF-CORE-ASK-CORPUS-PER-RULE-MAPS",
        "src/core/ask_corpus.rs",
        "load_rules",
        "let rule_tags = tags.remove(&rule.id).unwrap_or_default();",
        "Two adjacent per-rule map lookups (tags and source memory ids), carried as one group with multiplicity 2 because they are one decision about the same construct on consecutive lines. Both maps come from list_rule_tags_for_workspace / list_rule_source_memory_ids_for_workspace, whose errors are already propagated by `?` before this loop runs, and both are keyed by rule id over the same workspace -- so a miss is a rule with no tags or no source memories recorded, and the empty default says exactly that. `remove` rather than `get` is safe because rule ids are unique within a workspace, so no rule is visited twice.",
    ),
    // bd-apvhh burn-down, tranche 3 (2026-09-18): src/mesh/team.rs and
    // src/core/resume.rs, the two largest no-rule files, taken deliberately
    // because they are where the answer could come back NOT allowed. It did,
    // once: NSF-MESH-TEAM-JOIN-SYNC-OWN-ORIGIN-SWALLOWED was the first must_fix
    // this burn-down produced.
    //
    // IT IS NOW FIXED AND RETIRED, together with its twin
    // NSF-MESH-FOREGROUND-SYNC-OWN-ORIGIN-SWALLOWED further down. Both sites
    // read the node's own origin id with `.ok()`, collapsing a db error and an
    // absent self member into `""`, which compares unequal to every real origin
    // id and silently disabled classify_inbound's no-echo guard. Both now call
    // `team::resolve_own_origin_node_id`, which returns
    // `Result<Option<String>, _>` and so cannot express a failure as an empty
    // string: the db error propagates (team.rs) or refuses the round (
    // foreground_cli.rs), and the not-yet-enrolled case is an explicit `None`
    // (bd-1jpg7).
    //
    // Their ledger rows went in the same commit. Each rule owned exactly one
    // finding, and the repair removed the `.unwrap_or_default()` those findings
    // were detected on, so both would have dropped to owning zero -- which
    // `every_rule_declares_the_number_of_findings_it_owns` fails on, and which
    // is the dead-rule state bd-epvc1 is about. Note the file-level staleness
    // check would NOT have caught this pair on its own: the fragment
    // `.find(|member| member.is_self)` still appears in both files, in the
    // replacement helper and in the already-correct
    // `resolve_self_origin_node_id`. The count ledger is what made the
    // retirement non-optional.
    allowed_in(
        "NSF-MESH-TEAM-IDENTITY-USER-ID-ROUNDTRIP",
        "src/mesh/team.rs",
        "revalidate_team_identities",
        "user_id.unwrap_or_default()",
        "Three sites in one match, carried with multiplicity 3. The empty string never reaches storage or any comparison: the call site immediately re-lifts it with `(!user_id.is_empty()).then_some(user_id.as_str())`, and upsert_team_member_identity takes Option<&str>, so an absent id is written as SQL NULL rather than as \"\". Readers guard on it too -- the owner lookup does `if let Some(user_id) = recorded.user_id.as_deref()` before comparing, so a NULL id can never collide with another member's. THE ROUND TRIP IS THE WHOLE JUSTIFICATION: delete the `then_some` and \"\" lands in the column, where that equality would match every other member with an empty id. Note the sibling `login` on these same lines uses an explicit \"unknown\" sentinel instead, which is the clearer pattern.",
    ),
    allowed_in(
        "NSF-MESH-TEAM-ACTIVITY-UNATTRIBUTED",
        "src/mesh/team.rs",
        "list_team_activity",
        "let origin_node_id = members",
        "Two adjacent sites, multiplicity 2: a memory with no team provenance and no producer agent gets an empty display name, which is then used to look up an origin node id. That lookup CANNOT mis-attribute, and the proof is in the schema rather than in this file: team member display_name is `TEXT NOT NULL CHECK (length(trim(display_name)) > 0)`, so no member can carry an empty name and `find(|m| m.display_name == \"\")` matches nothing. The result is an unattributed activity row, which is the honest rendering of a memory whose producer is unknown.",
    ),
    allowed_in(
        "NSF-MESH-TEAM-INVITE-AUTH-FLOOR-ABSENT",
        "src/mesh/team.rs",
        "invite_auth_floor",
        ".unwrap_or_default())",
        "An absent authorization clock floor becomes \"\", and that is a NO-OP rather than a bypass. All three consumers test `timestamp < floor` to reject events that precede the floor; \"\" is the minimum of the lexicographic order over RFC3339 strings, so `x < \"\"` is false for every non-empty timestamp and nothing is rejected -- which is exactly what \"no floor has been recorded\" should do. It cannot weaken a floor that EXISTS, because a recorded floor is returned intact and db errors propagate through `?`. The diagnostic path at the same file keeps the Option instead, because counting invites below the floor genuinely needs to tell absent from present.",
    ),
    allowed_in(
        "NSF-MESH-TEAM-JOIN-ATTEMPT-NONCE-BOOKKEEPING",
        "src/mesh/team.rs",
        "complete_join_first_sync",
        "joiner_nonce: existing",
        "Written to the join-attempt row at phase \"first_sync_complete\", i.e. AFTER the handshake has already succeeded, when no prior attempt row exists to copy the nonce from. It is bookkeeping, not a credential: the join proof compares the LIVE protocol messages (`prove.joiner_nonce != hello.joiner_nonce`) and never reads this persisted column, so an empty value here cannot satisfy any verification. Flagged rather than silently accepted: `inviter_nonce` on the very next line keeps its Option, so the two nonce fields of one struct disagree about how to spell absence, and this one loses the distinction in the audit trail.",
    ),
    allowed_in(
        "NSF-CORE-RESUME-SESSION-BOUNDS",
        "src/core/resume.rs",
        "group_sessions",
        "let oldest_at = members",
        "Two sites, multiplicity 2. A session group is built by grouping memories, so it holds at least one member by construction and first()/last() are Some. Reachability is moot in any case: the only consumer immediately does `newest_at.get(..10).unwrap_or(\"unknown\")`, so even an empty bound renders as the explicit label `inferred-unknown` rather than as a fabricated date.",
    ),
    allowed_in(
        "NSF-CORE-RESUME-STALENESS-UNTAGGED",
        "src/core/resume.rs",
        "apply_staleness",
        "let surfaced_tags = tags",
        "The default feeds directly into the guard on the next line: `if surfaced_tags.is_empty() { continue; }`. A memory with no tags is skipped rather than evaluated against an assumed tag set, so the empty slice is consumed by an emptiness test and never reaches a staleness decision.",
    ),
    // bd-apvhh burn-down, tranche 4 (2026-09-18): the 25 findings in the 7
    // no-rule files that were left after tranche 3.
    //
    // READ THE ALLOW RATE HERE THE WAY YOU READ TRANCHE 1's, NOT TRANCHE 3's.
    // This is the EASY REMAINDER by construction: tranche 3 deliberately took
    // the two largest and least tractable files first, so what survives is
    // pre-selected for being small and self-contained. A high allow rate in a
    // population chosen for being easy is not evidence about the codebase. The
    // two must_fix rules it produced are the exception that proves the sampling
    // was still worth doing, not a refutation of the bias.
    //
    // ONE OF THOSE TWO IS NOW FIXED AND ITS ROW IS RETIRED:
    // NSF-MESH-RESPONDER-DISCOVERY-LISTS-SWALLOWED covered
    // `.and_then(|path| load_workspace_lists(path).ok())` in
    // answer_bootstrap_hello, where an unreadable discovery denylist collapsed
    // into an empty one. That call site now declines the exchange instead
    // (bd-zjcx6), so the fragment is gone from the source and the row would be
    // stale -- `must_fix_entries_still_describe_real_code` says to drop a row
    // whose defect is fixed rather than let the gate overstate what is left.
    // Its match-count ledger line was dropped in the same commit, because
    // `every_rule_declares_the_number_of_findings_it_owns` also fails on a
    // ledger row naming a rule that no longer exists. Removing a must_fix here
    // records a defect CLOSED, not an exemption granted; the argument for the
    // fix lives in bd-zjcx6 and in the comment at the call site.
    // NSF-MESH-FOREGROUND-SYNC-OWN-ORIGIN-SWALLOWED was here: the SECOND site
    // of bd-1jpg7, character for character the same construct as the team.rs
    // one. Both are fixed and both rows are retired; the reasoning is at the
    // tranche-3 comment above, with its twin. The observation this rule was
    // written to make still stands and is worth keeping even though the rule is
    // gone: a must_fix covering one of two identical sites LOOKS like coverage
    // in the ledger -- one rule, one declared count, every arm green -- while
    // half the defect goes unwatched. It was found only because tranche 4 read
    // the whole remaining no-rule population instead of stopping once the class
    // was known.
    allowed_in(
        "NSF-MESH-RESPONDER-ADMISSION-CLOCK",
        "src/mesh/responder_broker.rs",
        "admit_authenticated_capability",
        ".duration_since(std::time::UNIX_EPOCH)",
        "duration_since fails only when the system clock predates 1970, in which case every timestamp in the process is wrong rather than this one. The direction is safe regardless: a frozen now_epoch_ms of 0 makes admission MORE restrictive, because backoff_until is computed as now.saturating_add(delay) and tested as `backoff_until > request.now_epoch_ms`, so any backoff once set stays above zero forever and the peer remains backed off. It denies, it cannot admit. The sibling conversions on the same lines choose `.unwrap_or(u64::MAX)` for the same reason -- saturate toward the restrictive end.",
    ),
    allowed_in(
        "NSF-MESH-RESPONDER-NO-REGISTRATIONS-YET",
        "src/mesh/responder_broker.rs",
        "apply_control_register",
        "let mut next = self.durable_registrations.clone().unwrap_or_default();",
        "durable_registrations is Option<Vec<_>> initialised to None and set to Some only once registrations exist, so None means \"none registered yet\" rather than \"failed to load\". With no existing registrations there is no port to conflict with, which is precisely what the `next.first()` check that follows concludes.",
    ),
    allowed_in(
        "NSF-MESH-RESPONDER-CHUNK-SIZE-TOKEN",
        "src/mesh/responder_broker.rs",
        "decode_local_api_chunked_body",
        "let size_token = size_line.split(';').next().unwrap_or_default().trim();",
        "UNREACHABLE: str::split always yields at least one element. And the direction is safe even if it were not, because the empty token goes straight into from_str_radix, whose failure is mapped to ResponderBrokerError::WhoIsUnverified -- a malformed chunk header is rejected, not read as a zero-length chunk.",
    ),
    allowed_in(
        "NSF-MESH-RESPONDER-BODY-FETCH-KEY",
        "src/mesh/responder_broker.rs",
        "load_body_fetch_response",
        ".map(|parsed| parsed.body_cache_key.as_str())",
        "A payload that does not deserialise yields an empty cache key, and the very next statement is `if key.is_empty() || ..`, which rejects it. The default feeds an emptiness guard rather than a lookup.",
    ),
    allowed_in(
        "NSF-MESH-FOREGROUND-COMMAND-MODE-DEFAULT",
        "src/mesh/foreground_cli.rs",
        "mesh_enabled_and_mode",
        ".or_else(|| configured.and_then(|config| config.mesh.command_mode))",
        "A three-tier configuration read: env var, then workspace config, then the type's Default. Neither earlier tier swallows an error -- a malformed env value fails to parse and falls through to config rather than being coerced -- and the final default is the documented default mode, not a stand-in for a lookup that failed.",
    ),
    allowed_in(
        "NSF-CLI-TEAM-ACTIVITY-SINCE-SUFFIX",
        "src/cli/team.rs",
        "handle_team_activity",
        ".map(|since| format!(\" since {since}\"))",
        "The default is an empty SUFFIX for a report with no `since` filter, not an erased value. An unfiltered report should say nothing about a filter.",
    ),
    allowed_in(
        "NSF-CLI-TEAM-JOIN-MISSING-INVITE",
        "src/cli/team.rs",
        "handle_team_join",
        "args.invite.clone().unwrap_or_default()",
        "Immediately guarded: `if invite_code.is_empty() { return write_domain_error(..) }`. A missing invite becomes an empty string only long enough to be rejected with a structured error on the next line.",
    ),
    allowed_in(
        "NSF-CORE-GRAPH-DIFF-ABSENT-ARRAYS",
        "src/core/graph_diff.rs",
        "parse_snapshot_graph",
        ".and_then(serde_json::Value::as_array)",
        "Two sites, multiplicity 2: nodes and edges. A snapshot that carries no nodes array and no edges array describes a graph with no nodes and no edges, and the diff over two explicit snapshot inputs is entitled to say so. Both lookups already try two spellings (`nodes` and `/graph/nodes`) before defaulting, so a differently-shaped snapshot is accommodated rather than silently emptied.",
    ),
    allowed_in(
        "NSF-CORE-ASK-MISS-EMPTY-RESULTS",
        "src/core/ask.rs",
        "record_ask_query_miss_best_effort",
        "\"empty_results\"",
        "The default feeds an emptiness test, not a value: `report.nearest_evidence.as_deref().unwrap_or_default().is_empty()` classifies the miss as \"empty_results\". Absent evidence and an empty evidence list are the same miss, and this collapses them deliberately.",
    ),
    allowed_in(
        "NSF-CORE-ASK-ASSIST-EMPTY-EVIDENCE",
        "src/core/ask.rs",
        "ask_query_assist_json",
        "let nearest_evidence = report.nearest_evidence.as_deref().unwrap_or_default();",
        "Same shape and the same next line: the empty slice is consumed by `if nearest_evidence.is_empty()` to choose a reason string. Note the guard above it, which is the part that matters for honesty: a source-integrity failure returns None BEFORE this point, so a withheld answer is never rendered as missing knowledge.",
    ),
    allowed_in(
        "NSF-CORE-WRITE-OWNER-UNRECORDED-COUNTERS",
        "src/core/write_owner.rs",
        "read_write_group_commit_counters",
        "Some(key) => store.get(&Some(key)).copied().unwrap_or_default(),",
        "Process-local telemetry counters. A workspace with no recorded group commits has counted zero of them, so the zero is the measurement rather than a substitute for one -- the same reason an absent memory-debt REPORT is not equivalent (there the zero stood in for an unmeasured population; here the population is the events this process has seen).",
    ),
    allowed_in(
        "NSF-CORE-WRITE-OWNER-TELEMETRY-CONFIG-DEFAULT",
        "src/core/write_owner.rs",
        "write_group_commit_telemetry",
        ".map(|config| WriteHotPathConfig::from_write_config(&config.write))",
        "No workspace path, or a workspace with no write config, yields the documented default hot-path configuration. The value is only used to decide whether group-commit telemetry is enabled, and the disabled case is itself named downstream rather than silent.",
    ),
    allowed_in(
        "NSF-CORE-WRITE-OWNER-INTAKE-CONFIG-DEFAULT",
        "src/core/write_owner.rs",
        "run_one_shot_write_intake",
        ".map(|config| WriteHotPathConfig::from_write_config(&config.write))",
        "The same default in the intake path, and here the honesty is explicit: `if !config.enabled` produces WriteGroupCommitFallbackReason::Disabled, a named fallback reason carried in the result, so running without group commit is reported rather than assumed.",
    ),
    allowed_in(
        "NSF-CORE-MODEL-LAST-SEGMENT",
        "src/core/model.rs",
        "last_segment",
        ".rsplit('/')",
        "UNREACHABLE: str::rsplit always yields at least one element, so a path with no separator returns itself. The helper exists to compare embedder identities and an empty segment could only arise from an empty input, which compares equal to another empty input -- the correct answer for two unnamed embedders.",
    ),
    allowed_in(
        "NSF-CORE-MODEL-INDEX-METADATA-PATHS",
        "src/core/model.rs",
        "read_model_lifecycle_index_metadata",
        ".map(redact_lifecycle_metadata_path)",
        "An absent or non-array metadata field yields no paths. The collection is a list of redacted lifecycle paths for reporting; nothing downstream treats an empty list as a claim that no paths exist on disk.",
    ),
    must_fix(
        "NSF-CASS-PIPE-READ",
        "src/cass/process.rs",
        "read_to_end",
        "eidetic_engine_cli-sos5.2",
        "CASS subprocess pipe read errors must become CassError or explicit degradations.",
    ),
    must_fix(
        "NSF-HOOK-INSTALLER-JSON",
        "src/hooks/installer.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Hook installer JSON is machine-facing output and must not serialize to an empty string on failure.",
    ),
    must_fix(
        "NSF-OUTPUT-SHADOW-INCUMBENT",
        "src/output/mod.rs",
        "incumbent_outcome.clone().unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Shadow decision output should distinguish missing incumbent evidence from an empty incumbent outcome.",
    ),
    must_fix(
        "NSF-CLI-CERTIFICATE-JSON",
        "src/cli/mod.rs",
        "serde_json::to_string_pretty",
        "eidetic_engine_cli-sos5.3",
        "Certificate JSON handlers bypass the shared renderer and silently erase serialization failures.",
    ),
    must_fix(
        "NSF-CLI-CERTIFICATE-ERROR",
        "src/cli/mod.rs",
        "report.error.clone().unwrap_or_default()",
        "eidetic_engine_cli-sos5.3",
        "Certificate error reports should not convert a missing error message into an empty machine string.",
    ),
    must_fix(
        "NSF-CLI-DEMO-AUDIT",
        "src/cli/mod.rs",
        "latest_demo_audit_by_id",
        "eidetic_engine_cli-sos5.4",
        "Demo status output should distinguish missing audit storage from an empty run map.",
    ),
    allowed(
        "NSF-MODELS-JSONL-BUILDERS",
        "src/models/jsonl.rs",
        "ExportRecordBuildError",
        "JSONL export builders reject missing required IDs, timestamps, content, and schema fields with ExportRecordBuildError.",
    ),
    must_fix(
        "NSF-CURATE-CERTIFICATE-BUILDER",
        "src/curate/mod.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Curation risk certificate builders default machine-facing IDs/timestamps to empty values.",
    ),
    must_fix(
        "NSF-MODELS-DECISION-BUILDER",
        "src/models/decision.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Decision records should distinguish a missing outcome from an empty outcome string.",
    ),
    must_fix(
        "NSF-MODELS-MUTATION-JSON",
        "src/models/mutation.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Mutation reports are machine-facing and must not serialize to empty strings on failure.",
    ),
    must_fix(
        "NSF-MODELS-PROGRESS-BUILDER",
        "src/models/progress.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Progress records default required operation/message/timestamp fields to empty values.",
    ),
    must_fix(
        "NSF-CORE-BACKUP-IMPORT",
        "src/core/backup.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Backup import/export records should distinguish absent message, next action, and audit target fields.",
    ),
    must_fix(
        "NSF-CORE-CLAIMS-INPUT",
        "src/core/claims.rs",
        "unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Claim parsing defaults optional statement/artifact collections into machine-facing records and needs an explicit contract.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-JSON",
        "src/core/handoff.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Handoff JSON render helpers must not hide serialization failures.",
    ),
    must_fix(
        "NSF-CORE-LAB-JSON",
        "src/core/lab.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Lab report JSON helpers must not silently serialize to empty.",
    ),
    allowed(
        "NSF-CORE-LEGACY-SKIP-DIR",
        "src/core/legacy_import.rs",
        "fn should_skip_directory",
        "A path without a UTF-8 file name cannot match a skipped legacy directory name.",
    ),
    must_fix(
        "NSF-CORE-OUTCOME-WORKSPACE",
        "src/core/outcome.rs",
        "workspace_id.unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Outcome recording should not turn a missing workspace ID into an empty persisted field.",
    ),
    must_fix(
        "NSF-CORE-PREFLIGHT-JSON",
        "src/core/preflight.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Preflight report JSON helpers must not silently serialize to empty.",
    ),
    must_fix(
        "NSF-CORE-PROCEDURE-JSON",
        "src/core/procedure.rs",
        "serde_json::to_string",
        "eidetic_engine_cli-sos5.3",
        "Procedure report JSON helpers must not silently serialize to empty.",
    ),
    allowed(
        "NSF-CASS-IMPORT-OPTIONAL-FIELDS",
        "src/cass/import.rs",
        "unwrap_or_default()",
        "CASS importer defaults here are parser policy for empty spans, unknown line types, optional counts, or fallback content hashes; malformed required JSON still errors.",
    ),
    allowed(
        "NSF-CLI-WORKSPACE-CWD",
        "src/cli/mod.rs",
        "std::env::current_dir().unwrap_or_default()",
        "CLI workspace fallback is the existing documented relative-workspace behavior; it does not convert parsed machine data to success.",
    ),
    allowed(
        "NSF-CLI-EVAL-NO-EXPECTATIONS",
        "src/cli/mod.rs",
        "query_expectations.is_empty()",
        "An eval fixture with no expected query matches has no retrieval queries to run; index and search failures still propagate once queries exist.",
    ),
    allowed(
        "NSF-CLI-EVAL-FIRST-FAILURE-NO-QUERY",
        "src/cli/mod.rs",
        "\"expectedIds\": query.map",
        "Eval first-failure output uses empty ID arrays only when no failing per-query metric exists; fixture status and reason codes still report the failure.",
    ),
    allowed(
        "NSF-CLI-RESPONSE-FIELD-COUNT",
        "src/cli/mod.rs",
        "map(count_json_object_fields)",
        "A response without a data object has zero selectable data fields for field-selector telemetry.",
    ),
    allowed(
        "NSF-CLI-PACK-DEFAULT-PROFILES",
        "src/cli/mod.rs",
        "pack_profile: args.pack_profile.unwrap_or_default()",
        "Omitted pack/resource profiles intentionally use the default ContextOutputOptions profile.",
    ),
    allowed(
        "NSF-CLI-CONTEXT-OUTPUT-DEFAULT-PROFILES",
        "src/cli/mod.rs",
        "args.pack_profile.unwrap_or_default()",
        "Omitted context output profile arguments intentionally select default pack/resource output profiles.",
    ),
    allowed(
        "NSF-CLI-PACK-DIFF-OPTIONAL-STRING-ARRAYS",
        "src/cli/mod.rs",
        "strings.sort()",
        "Pack diff redaction-class arrays are optional ledger details; absent arrays mean no classes to compare.",
    ),
    must_fix(
        "NSF-CLI-PACK-DIFF-RANK-DEFAULT",
        "src/cli/mod.rs",
        "let old_rank = old_item.rank.unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Pack diff should distinguish a missing ledger rank from rank zero when reporting rank deltas.",
    ),
    allowed(
        "NSF-CLI-PACK-REPLAY-SELECTED-ITEMS",
        "src/cli/mod.rs",
        "ledger_core_array(value, \"selectedItems\")",
        "Pack replay with a missing selectedItems ledger array reports an empty replay section while ledger status/degradations remain available.",
    ),
    allowed(
        "NSF-CLI-PACK-REPLAY-OMITTED-ITEMS",
        "src/cli/mod.rs",
        "ledger_core_array(value, \"omittedItems\")",
        "Pack replay with a missing omittedItems ledger array reports no omitted items while ledger status/degradations remain available.",
    ),
    allowed(
        "NSF-CLI-QUERY-PAGINATION-DEFAULT",
        "src/cli/mod.rs",
        "parse_pagination",
        "Missing query-file pagination intentionally means default pagination bounds.",
    ),
    allowed(
        "NSF-CLI-QUERY-GRAPH-SEEDS-DEFAULT",
        "src/cli/mod.rs",
        "let seed_memories = graph",
        "Missing graph seedMemories in ee.query.v1 intentionally means no explicit graph seeds.",
    ),
    allowed(
        "NSF-CLI-QUERY-GRAPH-TRAVERSAL-DEFAULT",
        "src/cli/mod.rs",
        "let traversal = graph",
        "Missing graph traversal intentionally uses the QueryGraphTraversal default after validation handles malformed values.",
    ),
    allowed(
        "NSF-CLI-QUERY-GRAPH-LINK-TYPES-DEFAULT",
        "src/cli/mod.rs",
        "let include_orphans = graph",
        "Missing graph linkTypes intentionally means no relation filter after validation handles malformed values.",
    ),
    allowed(
        "NSF-CLI-REHEARSE-NO-COMMANDS",
        "src/cli/mod.rs",
        "(None, None) => return Ok(Vec::new())",
        "Omitting both rehearsal command sources intentionally plans no commands; unreadable files and malformed JSON still return DomainError.",
    ),
    allowed(
        "NSF-CLI-MAINTENANCE-NO-HISTORY",
        "src/cli/mod.rs",
        "if !path.exists()",
        "A missing maintenance history JSONL file means no recorded jobs yet; read and parse errors on an existing file still fail.",
    ),
    allowed(
        "NSF-CLI-QUERY-FILTERS",
        "src/cli/mod.rs",
        "parse_filters",
        "Missing query filters are an explicit empty-filter case; malformed recognized fields are validated separately.",
    ),
    allowed(
        "NSF-CORE-BUDGET-SATURATION",
        "src/core/budget.rs",
        "unwrap_or_default()",
        "Budget clock math intentionally saturates reversed or expired durations to zero and documents that behavior.",
    ),
    allowed(
        "NSF-CORE-CAUSAL-OPTIONAL-FILTERS",
        "src/core/causal.rs",
        "unwrap_or_default()",
        "Optional memory IDs are query filters and do not represent parsed storage failure.",
    ),
    allowed(
        "NSF-CORE-CONTEXT-TAGS",
        "src/core/context.rs",
        "tags_map.get",
        "A memory with no tag rows has an explicit empty tag set.",
    ),
    allowed(
        "NSF-CORE-CURATE-PROPOSED-CONTENT-TAGS",
        "src/core/curate.rs",
        "stored.proposed_content.as_deref().unwrap_or_default()",
        "A curation candidate without proposed content can still derive tags from its reason and cluster membership.",
    ),
    allowed(
        "NSF-CORE-CLAIMS-NO-EVIDENCE",
        "src/core/claims.rs",
        "let Some(raw_evidence) = raw_evidence else",
        "Claims without an evidence field deliberately have an empty evidence list; malformed evidence entries still return ClaimParseError.",
    ),
    allowed(
        "NSF-CORE-CLAIMS-NULL-EVIDENCE",
        "src/core/claims.rs",
        "YamlValue::Null => Ok(Vec::new())",
        "A YAML null evidence field is treated as explicitly empty evidence, while non-null malformed evidence is rejected.",
    ),
    allowed(
        "NSF-CORE-DOCTOR-OPTIONAL-REPAIR",
        "src/core/doctor.rs",
        "check.repair.unwrap_or_default()",
        "Doctor command text may be absent; the surrounding check still carries severity and message.",
    ),
    allowed(
        "NSF-CORE-ECONOMY-BASELINE",
        "src/core/economy.rs",
        "unwrap_or_default()",
        "No matching baseline scenario means there are no baseline artifact scores to compare.",
    ),
    allowed(
        "NSF-CORE-HANDOFF-EVIDENCE-LINKS",
        "src/core/handoff.rs",
        "get(\"kind\")",
        "Malformed optional task-frame evidence links are skipped rather than emitted as empty links.",
    ),
    allowed(
        "NSF-CORE-HANDOFF-EVIDENCE-LINK-IDS",
        "src/core/handoff.rs",
        "get(\"id\")",
        "Malformed optional task-frame evidence links are skipped rather than emitted as empty links.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-STALE-ADDED-DEFAULT",
        "src/core/handoff.rs",
        "threshold_field: \"memories_added\"",
        "eidetic_engine_cli-sos5.4",
        "Handoff stale-threshold reporting should distinguish unavailable added-memory counts from zero.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-STALE-EXPIRED-DEFAULT",
        "src/core/handoff.rs",
        "threshold_field: \"any_expired_in_pack\"",
        "eidetic_engine_cli-sos5.4",
        "Handoff stale-threshold reporting should distinguish unavailable expired-memory counts from zero.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-STALE-DRIFT-DEFAULT",
        "src/core/handoff.rs",
        "content_drift_score.unwrap_or_default()",
        "eidetic_engine_cli-sos5.4",
        "Handoff stale-threshold reporting should distinguish unavailable content drift from zero drift.",
    ),
    must_fix(
        "NSF-CORE-HANDOFF-STALE-REVISED-DEFAULT",
        "src/core/handoff.rs",
        "threshold_field: \"memories_revised\"",
        "eidetic_engine_cli-sos5.4",
        "Handoff stale-threshold reporting should distinguish unavailable revised-memory counts from zero.",
    ),
    allowed(
        "NSF-CORE-INDEX-HUMAN-DIMENSION",
        "src/core/index.rs",
        "quality_dimension.unwrap_or_default()",
        "Quality embedder dimension is optional human display text and is gated by quality model presence.",
    ),
    allowed(
        "NSF-CORE-INDEX-VACUUM-NO-PARENT",
        "src/core/index.rs",
        "return Ok(Vec::new());",
        "If the index parent directory does not exist, there are no stale index directories to vacuum.",
    ),
    allowed(
        "NSF-CORE-INIT-CWD",
        "src/core/init.rs",
        "std::env::current_dir",
        "Relative init paths retain the existing workspace fallback and still render the selected path.",
    ),
    allowed(
        "NSF-CORE-INSTALL-OPTIONALS",
        "src/core/install.rs",
        "unwrap_or_default()",
        "Installer planning treats missing artifacts and PATH as empty collections without reporting a successful install.",
    ),
    allowed(
        "NSF-CORE-JSONL-IMPORT-TAGS",
        "src/core/jsonl_import.rs",
        "tags_by_memory",
        "Imported memories without tag records have an explicit empty tag set.",
    ),
    allowed(
        "NSF-CORE-MEMORY-AUTO-LINK-DISABLED",
        "src/core/memory.rs",
        "if !enabled",
        "Disabled remember auto-linking intentionally creates no links before any repository query is attempted.",
    ),
    allowed(
        "NSF-CORE-MEMORY-AUTO-LINK-NO-WORKFLOW",
        "src/core/memory.rs",
        "let Some(workflow_id) = workflow_id else",
        "Remember auto-linking without a workflow ID has no workflow neighborhood to query; repository errors after a workflow is present still propagate.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SUGGEST-LINKS-NO-TAGS",
        "src/core/memory.rs",
        "if tags.is_empty()",
        "Tag-based link suggestions require at least one tag; missing tags are an explicit no-input case.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SUGGEST-LINKS-NO-MATCHES",
        "src/core/memory.rs",
        "if matches.is_empty()",
        "A successful tag lookup with no candidate memories is an explicit empty suggestion set; lookup failures still return DomainError.",
    ),
    allowed(
        "NSF-CORE-LAB-OPTIONAL-FIELDS",
        "src/core/lab.rs",
        "as_deref().unwrap_or_default()",
        "Lab hash input includes optional intervention fields as empty components while retaining the surrounding structured record.",
    ),
    allowed(
        "NSF-CORE-LEARN-CWD",
        "src/core/learn.rs",
        "current_dir().unwrap_or_default()",
        "Learning path resolution keeps the existing relative path fallback and does not manufacture learned evidence.",
    ),
    allowed(
        "NSF-CORE-LEARN-CLUSTER-NO-TAGS",
        "src/core/learn.rs",
        "snapshot\n.memory_tags",
        "Learn-cluster embedding text represents untagged memories with an empty tags line.",
    ),
    allowed(
        "NSF-CORE-LEGACY-NONUTF8-FILENAME",
        "src/core/legacy_import.rs",
        "name.starts_with(\"._\")",
        "A non-UTF-8 legacy filename cannot match macOS metadata filenames and is still sorted by the path wire string.",
    ),
    must_fix(
        "NSF-CORE-MEMORY-LINE-SPAN",
        "src/core/memory.rs",
        "extract_line_span(&contents, *span).unwrap_or_default()",
        "eidetic_engine_cli-sos5.7",
        "Evidence freshness should report an invalid provenance span instead of hashing an empty source excerpt.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SECRET-ALLOWLISTS",
        "src/core/memory.rs",
        "allow_phrases: config",
        "Missing secret-detector allowlist arrays intentionally mean no configured bypass phrases or regexes.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SECRET-ALLOWREGEX",
        "src/core/memory.rs",
        "allow_regex: config",
        "Missing secret-detector allow_regex config intentionally means no configured bypass regexes.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SCOPE-TEAM-MEMBERS",
        "src/core/memory_scope.rs",
        "team_members\n.unwrap_or_default()",
        "Team scope loads durable team_members rows only; missing config cannot mint nicknames.",
    ),
    allowed(
        "NSF-CORE-MEMORY-SCOPE-AGENT-URI",
        "src/core/memory_scope.rs",
        ".split(['/', '#', '?'])",
        "An agent provenance URI with no name segment is normalized away rather than emitted as an empty agent.",
    ),
    allowed(
        "NSF-CORE-PLAN-RAND-ID",
        "src/core/plan.rs",
        "duration_since(SystemTime::UNIX_EPOCH)",
        "Pseudo-random fallback only handles a clock before UNIX_EPOCH and does not feed persisted evidence.",
    ),
    allowed(
        "NSF-CORE-RECORDER-CASS-CLASSIFIER",
        "src/core/recorder.rs",
        "unwrap_or_default()",
        "Recorder CASS line classification maps missing type/role to a conservative message event.",
    ),
    allowed(
        "NSF-CORE-RECORDER-IMPORT-NO-INPUT",
        "src/core/recorder.rs",
        "let Some(input) = options.input_json.as_deref() else",
        "Recorder import with no inline CASS view input is an explicit empty future-connector plan; invalid provided JSON returns recorder_import_invalid_json.",
    ),
    allowed(
        "NSF-CORE-REPRO-MISSING-HASH",
        "src/core/repro.rs",
        "expected_artifacts",
        "A missing expected hash is paired with a failed verification result, not a successful empty hash.",
    ),
    allowed(
        "NSF-CORE-PREFLIGHT-GUARD-NO-RULES",
        "src/core/preflight_guard.rs",
        "let Some(rules_item) = document.get(\"rules\") else",
        "A workspace guard file without a rules table has no rules to enforce; malformed rules tables still return DomainError.",
    ),
    allowed(
        "NSF-CORE-PROCEDURE-NO-STORE",
        "src/core/procedure.rs",
        "let Some(store) = open_procedure_store(workspace)? else",
        "A workspace without a procedure store has no procedures yet; store open errors still propagate through DomainError.",
    ),
    allowed(
        "NSF-CORE-SEARCH-OPTIONAL-DETAIL",
        "src/core/search.rs",
        "last_check_error",
        "Absent index-check detail appends no extra sentence while preserving the high-severity corruption signal.",
    ),
    allowed(
        "NSF-CORE-SEARCH-NO-RELEVANT-TOP-SCORE",
        "src/core/search.rs",
        "let top_note = top_score",
        "A no-relevant-results degradation may omit the optional top-score sentence while keeping the main degradation.",
    ),
    allowed(
        "NSF-CORE-SEARCH-HIT-TAGS",
        "src/core/search.rs",
        "metadata_string(metadata, \"tags\")",
        "Search hits without tag metadata are valid untagged memories.",
    ),
    allowed(
        "NSF-CORE-SEARCH-HIT-TOKEN-CONTENT",
        "src/core/search.rs",
        "estimate_tokens_default",
        "Search hit token estimates fall back to already-required content metadata when the analysis content key is absent.",
    ),
    allowed(
        "NSF-CORE-SEARCH-HIT-SECTION",
        "src/core/search.rs",
        "match (level.unwrap_or_default(), kind.unwrap_or_default())",
        "Missing optional search metadata classifies the pack item into the generic artifacts section.",
    ),
    allowed(
        "NSF-CORE-SEARCH-HIT-PROVENANCE",
        "src/core/search.rs",
        "PackProvenance::new(uri",
        "If derived provenance construction rejects the fallback URI, the hit can still be represented without provenance details.",
    ),
    must_fix(
        "NSF-CORE-STATUS-AUDIT-ACCESS",
        "src/core/status.rs",
        "list_audit_entries",
        "eidetic_engine_cli-sos5.7",
        "Status memory health should surface audit-log read failures instead of treating all memories as never accessed.",
    ),
    allowed(
        "NSF-CORE-SUPPORT-BUNDLE-PACK-QUERY",
        "src/core/support_bundle.rs",
        "let query = row_text(row, 1).unwrap_or_default()",
        "Support-bundle pack summaries may represent a missing query column as an empty diagnostic field.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-BV-TOP-PICKS",
        "src/core/swarm_brief.rs",
        "\"topPickIds\"",
        "A swarm brief without BV top picks intentionally reports an empty top-pick list.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-RECOMMENDATIONS",
        "src/core/swarm_brief.rs",
        "Swarm brief summary",
        "A swarm brief summary without recommendation IDs intentionally renders no recommendation examples.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-CYCLE-EXAMPLES",
        "src/core/swarm_brief.rs",
        "examples.sort()",
        "A Beads dependency-cycle payload without example cycles intentionally reports an empty examples list.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-BV-PICKS",
        "src/core/swarm_brief.rs",
        "let picks_value = quick_ref",
        "BV robot JSON may omit top_picks while still reporting aggregate counts.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-MAIL-RESERVATIONS",
        "src/core/swarm_brief.rs",
        "let inbox = value",
        "Agent Mail snapshots may omit reservations; missing arrays mean empty sections after JSON parse succeeds.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-MAIL-INBOX",
        "src/core/swarm_brief.rs",
        "let threads = value",
        "Agent Mail snapshots may omit inbox entries; missing arrays mean empty sections after JSON parse succeeds.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-MAIL-THREADS",
        "src/core/swarm_brief.rs",
        "let mut reservations = reservations",
        "Agent Mail snapshots may omit thread entries; missing arrays mean empty sections after JSON parse succeeds.",
    ),
    allowed(
        "NSF-CORE-SWARM-BRIEF-RCH-OPTIONAL-WORKER",
        "src/core/swarm_brief.rs",
        "summarize_rch_topology_blocked_message",
        "An RCH-E327 degradation may omit the selected worker; the topology-blocked code and redacted root summary remain explicit.",
    ),
    allowed(
        "NSF-CORE-PERF-FORENSICS-SOURCE-SCHEMA",
        "src/core/perf_forensics.rs",
        "source_schema: normalized.source_schema.unwrap_or_default()",
        "Perf artifacts treat source schema as optional metadata; missing values do not hide metric ingestion failure.",
    ),
    allowed(
        "NSF-CORE-PERF-FORENSICS-UNIT",
        "src/core/perf_forensics.rs",
        "unit.unwrap_or_default().to_lowercase()",
        "Perf metric unit is optional metadata; missing units simply skip unit-based volatility inference.",
    ),
    allowed(
        "NSF-MODELS-QUERY-MISSING-ARRAY-FILTER",
        "src/models/query.rs",
        "Result<Vec<String>, EqlQueryError>",
        "Missing optional EQL array filters are deliberate empty filter sets; present non-array or empty-string values still return EqlQueryError.",
    ),
    allowed(
        "NSF-MODELS-QUERY-TAG-FILTERS",
        "src/models/query.rs",
        "let require_any = object",
        "Missing tag filter arrays in ee.query.v1 intentionally mean no tag filter.",
    ),
    allowed(
        "NSF-MODELS-QUERY-TAG-REQUIRE-ANY-FILTERS",
        "src/models/query.rs",
        "let exclude = object",
        "Missing tag requireAny arrays in ee.query.v1 intentionally mean no alternate tag filter.",
    ),
    allowed(
        "NSF-MODELS-QUERY-TAG-EXCLUDE-FILTERS",
        "src/models/query.rs",
        "TagFilters {",
        "Missing tag exclude arrays in ee.query.v1 intentionally mean no tag exclusion filter.",
    ),
    allowed(
        "NSF-MODELS-QUERY-TRUST-FILTERS",
        "src/models/query.rs",
        "let require_posture = object",
        "Missing trust excludeClasses in ee.query.v1 intentionally means no trust-class exclusions.",
    ),
    allowed(
        "NSF-MODELS-QUERY-REDACTION-FILTERS",
        "src/models/query.rs",
        "RedactionFilters {",
        "Missing redaction allowCategories in ee.query.v1 intentionally means the default redaction policy.",
    ),
    allowed(
        "NSF-DB-FEEDBACK-SIGNAL",
        "src/db/mod.rs",
        "optional_text(row, 0)?.unwrap_or_default()",
        "Missing feedback signal maps to no positive/negative bucket and does not create a successful signal.",
    ),
    allowed(
        "NSF-DB-LATEST-SCHEMA-EMPTY",
        "src/db/mod.rs",
        "MIGRATIONS\n.last()",
        "A build with no compiled migrations would report schema version zero rather than hiding a database operation failure.",
    ),
    allowed(
        "NSF-DB-PACK-LEDGER-NO-DEGRADATIONS",
        "src/db/mod.rs",
        "return Ok(Vec::new());",
        "A pack ledger with no degraded JSON has an explicit empty degradation list.",
    ),
    allowed(
        "NSF-DB-PACK-LEDGER-DEGRADATION-ARRAY",
        "src/db/mod.rs",
        "pack_ledger_core_array(ledger, \"degraded\")",
        "A parsed pack ledger without a degraded array has no ledger-local degradations.",
    ),
    allowed(
        "NSF-DB-PACK-LEDGER-DEGRADATION-SORT",
        "src/db/mod.rs",
        "let severity = value",
        "Missing degradation sort-key fields are used only to produce a deterministic order for malformed diagnostic values.",
    ),
    allowed(
        "NSF-DB-PACK-LEDGER-DEGRADATION-MESSAGE-SORT",
        "src/db/mod.rs",
        "let message = value",
        "Missing degradation messages are used only to produce a deterministic order for malformed diagnostic values.",
    ),
    allowed(
        "NSF-GRAPH-PPR-NO-NEIGHBORS",
        "src/graph/ppr.rs",
        "edges.sort_unstable_by_key",
        "A graph node with no outgoing neighbors intentionally contributes an empty normalized edge list.",
    ),
    allowed(
        "NSF-GRAPH-PACK-DNA-NO-PPR-SEEDS",
        "src/graph/pack_dna.rs",
        "query_seed_weights.is_empty() || limit == 0",
        "Pack DNA PPR neighbors are explicitly empty when there are no valid query seeds or the caller requested a zero-neighbor limit.",
    ),
    allowed(
        "NSF-GRAPH-CAUSAL-CLOSURE-NO-SUCCESSORS",
        "src/graph/causal.rs",
        "closure\n.successors(failure_id)\n.unwrap_or_default()",
        "A failure node with no transitive causal successors intentionally has an empty ancestor list.",
    ),
    allowed(
        "NSF-GRAPH-CAUSAL-TERMINAL-NO-SUCCESSORS",
        "src/graph/causal.rs",
        "graph\n.successors(&ancestor.memory_id)\n.unwrap_or_default()",
        "A reachable ancestor with no outgoing causal successors is intentionally treated as terminal.",
    ),
    allowed(
        "NSF-GRAPH-CAUSAL-NO-NODE-ATTRS",
        "src/graph/causal.rs",
        "graph.node_attrs(node).cloned().unwrap_or_default()",
        "Causal flow projection permits nodes without optional attributes while adding required demand metadata explicitly.",
    ),
    allowed(
        "NSF-GRAPH-CAUSAL-BFS-NO-SUCCESSORS",
        "src/graph/causal.rs",
        "graph.successors(&current).unwrap_or_default()",
        "Causal shortest-path traversal uses an empty successor list as the explicit leaf-node case.",
    ),
    allowed(
        "NSF-OUTPUT-FIELD-SELECTOR-COMMAND",
        "src/output/mod.rs",
        "requested_fields_for_selector(command, selector)",
        "A response without a command name cannot match command-specific field selectors and is returned unchanged.",
    ),
    allowed(
        "NSF-PACK-COORDINATION-SCHEMA",
        "src/pack/mod.rs",
        "coordination_string_field(value, &[\"schema\"])",
        "A coordination snapshot without an explicit schema is treated as the current schema after the required sources array is validated.",
    ),
    allowed(
        "NSF-PACK-COORDINATION-ENTRIES",
        "src/pack/mod.rs",
        "entries.sort()",
        "A coordination source without entries intentionally contributes an empty entry list.",
    ),
    allowed(
        "NSF-PACK-COORDINATION-DEGRADATIONS",
        "src/pack/mod.rs",
        "coordination_string_field(item, &[\"repair\"])",
        "A coordination snapshot without degradation entries intentionally has no source degradations.",
    ),
    allowed(
        "NSF-CURATE-CLUSTER-DIMENSION",
        "src/curate/cluster_coherence.rs",
        "points\n.first()",
        "Cluster coherence converts an empty or zero-dimensional input into an explicit ClusterCoherenceError.",
    ),
    allowed(
        "NSF-CURATE-CLUSTER-REPRESENTATIVE",
        "src/curate/cluster_coherence.rs",
        "representative_memory_id",
        "Cluster representatives are derived after cluster membership validation and sorting.",
    ),
    allowed(
        "NSF-SERVE-DAEMON-DRY-RUN-ROWS",
        "src/serve.rs",
        "report.dry_run || run_id == \"dry-run\"",
        "A dry-run foreground daemon report intentionally produces no durable daemon job rows.",
    ),
    allowed(
        "NSF-SERVE-DAEMON-MISSING-TABLE",
        "src/serve.rs",
        "if !table_path.exists()",
        "A missing daemon job JSONL table means no daemon jobs have been recorded; existing-table read and parse errors still fail.",
    ),
    allowed(
        "NSF-MODELS-DEMO-OPTIONALS",
        "src/models/demo.rs",
        "unwrap_or_default()",
        "Demo fixtures use empty optional descriptions and values for human demonstration metadata only.",
    ),
    allowed(
        "NSF-POLICY-ENV-PROFILE",
        "src/policy/security_profile.rs",
        "read(EnvVar::SecurityProfile)",
        "Absent or invalid environment profile intentionally falls back to the default security profile.",
    ),
    allowed(
        "NSF-STEWARD-RESOURCE-SUMMARY",
        "src/steward/mod.rs",
        "consumption",
        "No recorded consumption for a budgeted resource means zero consumed, not hidden failed I/O.",
    ),
    // ---- bd-3gk66 batch 1: top-file census classification (2026-06-11) ----
    allowed(
        "NSF-INSIGHTS-GRAPH-DATA-ABSENT",
        "src/cli/insights/mod.rs",
        "let Some(data) = load_workspace_insights_graph_data(workspace, database_path)? else",
        "Absent workspace insights graph data is the documented absence protocol for section loaders; storage failures still propagate through the ? operator.",
    ),
    allowed(
        "NSF-INSIGHTS-NO-WORKSPACE",
        "src/cli/insights/mod.rs",
        "let Some(workspace) = workspace else",
        "An omitted workspace argument yields empty insight sections by documented CLI contract; it is input absence, not a converted failure.",
    ),
    allowed(
        "NSF-INSIGHTS-NO-DATABASE",
        "src/cli/insights/mod.rs",
        "open_insights_database(Some(workspace), database_path)? else",
        "A workspace without an openable insights database returns None by design while real open errors propagate through the ? operator.",
    ),
    allowed(
        "NSF-INSIGHTS-NO-WORKSPACE-ID",
        "src/cli/insights/mod.rs",
        "insights_workspace_id(&connection, workspace)? else",
        "An unregistered workspace resolves no workspace id and yields empty sections by design; lookup errors still propagate through the ? operator.",
    ),
    allowed(
        "NSF-INSIGHTS-NO-RUST-PATHS",
        "src/cli/insights/mod.rs",
        "if rust_paths.is_empty()",
        "A project without Rust source files has no symbol surface for blind-spot analysis; empty findings are the true result.",
    ),
    allowed(
        "NSF-INSIGHTS-OPTIONAL-GRAPH-MEMORIES",
        "src/cli/insights/mod.rs",
        ".map(|data| data.memories)",
        "Optional graph data maps to an empty memory list only when the documented absence protocol already returned None; failures propagate earlier.",
    ),
    allowed(
        "NSF-INSIGHTS-NO-LINKS",
        "src/cli/insights/mod.rs",
        "if links.is_empty()",
        "A memory graph with no edges has no proximity hotspots to rank; the empty list is the true analytical result.",
    ),
    allowed(
        "NSF-INSIGHTS-GOMORY-HU-MIN-NODES",
        "src/cli/insights/mod.rs",
        "if graph.node_count() < 2",
        "Gomory-Hu proximity needs at least two nodes; smaller graphs truly have no hotspot pairs.",
    ),
    allowed(
        "NSF-INSIGHTS-BRIDGE-MIN-NODES",
        "src/cli/insights/mod.rs",
        "if graph.node_count() < 3",
        "Articulation-point bridge analysis needs at least three nodes; smaller graphs truly have no bridges.",
    ),
    allowed(
        "NSF-INSIGHTS-NO-CONTRADICTIONS",
        "src/cli/insights/mod.rs",
        "if contradiction_links.is_empty()",
        "A graph without contradiction-marked edges has no contradiction clusters; the empty list is the true result.",
    ),
    allowed(
        "NSF-INSIGHTS-GAPS-EMPTY-GRAPH",
        "src/cli/insights/mod.rs",
        "if data.memories.is_empty() && data.links.is_empty()",
        "An empty knowledge graph has no gaps to surface; the empty list is the true result.",
    ),
    allowed(
        "NSF-INSIGHTS-BRIDGE-SPAN-COUNT",
        "src/cli/insights/mod.rs",
        "let evidence_span_count = incident_evidence",
        "A bridge memory missing from the evidence-span count map truly has zero counted spans; the map was built from the same loaded data.",
    ),
    allowed(
        "NSF-INSIGHTS-TOP-EMPTY-GRAPH",
        "src/cli/insights/mod.rs",
        "if data.memories.is_empty() || data.links.is_empty()",
        "Top-memory ranking over an empty memory or link set has nothing to rank; the empty list is the true result.",
    ),
    allowed(
        "NSF-INSIGHTS-NO-PAGERANK",
        "src/cli/insights/mod.rs",
        "if pagerank_scores.is_empty()",
        "PageRank over an empty projection yields no scores, so there are no top memories to report.",
    ),
    allowed(
        "NSF-INSIGHTS-TOP-LINK-COUNTS",
        "src/cli/insights/mod.rs",
        "let counts = link_counts",
        "A memory absent from the link-count map truly has zero links; the map was built from the same loaded link set.",
    ),
    allowed(
        "NSF-INSIGHTS-INCOMING-COUNT",
        "src/cli/insights/mod.rs",
        "incoming.get(&memory_id).copied().unwrap_or_default()",
        "A memory with no entry in the incoming-link map truly has zero incoming links.",
    ),
    allowed(
        "NSF-INSIGHTS-OUTGOING-COUNT",
        "src/cli/insights/mod.rs",
        "outgoing.get(&memory_id).copied().unwrap_or_default()",
        "A memory with no entry in the outgoing-link map truly has zero outgoing links.",
    ),
    allowed(
        "NSF-INSIGHTS-EMPTY-PROJECTION",
        "src/cli/insights/mod.rs",
        "if graph.node_count() == 0",
        "An empty graph projection has no items to report for load-bearing or revision-frontier sections; the empty list is the true result.",
    ),
    allowed(
        "NSF-INSIGHTS-FRONTIER-SUCCESSORS",
        "src/cli/insights/mod.rs",
        ".successors(&item.memory_id)",
        "A revision-frontier node without outgoing edges truly has no successors; graph construction failures propagate earlier.",
    ),
    allowed(
        "NSF-INSIGHTS-FRONTIER-PREDECESSORS",
        "src/cli/insights/mod.rs",
        ".predecessors(&item.memory_id)",
        "A revision-frontier node without incoming edges truly has no predecessors; graph construction failures propagate earlier.",
    ),
    allowed(
        "NSF-CLI-DEGRADED-REPAIR-TEXT",
        "src/cli/mod.rs",
        "entry.repair.as_deref().unwrap_or_default()",
        "Degradation repair hints are optional; human-facing rendering of an absent hint as empty text loses no machine-facing signal.",
    ),
    allowed(
        "NSF-CLI-RECORDER-RUN-ID-NAME",
        "src/cli/mod.rs",
        "if run_id.is_empty()",
        "A recorder run directory without a UTF-8 file name produces an empty run id that the immediate is_empty guard skips explicitly.",
    ),
    allowed(
        "NSF-CLI-DEGRADED-CODE-DEDUP",
        "src/cli/mod.rs",
        "fn push_json_degraded_unique",
        "Degraded-entry dedup keys on the code string; a non-string code becomes an empty key for deduplication only and the entry itself is preserved.",
    ),
    allowed(
        "NSF-CLI-INCIDENT-STRING-ARRAYS",
        "src/cli/mod.rs",
        "fn incident_string_array_or_empty",
        "Incident rendering treats absent optional string arrays as empty lists; the incident payload itself still renders its status and codes.",
    ),
    allowed(
        "NSF-CLI-PACK-PROFILE-LENS-DEFAULT",
        "src/cli/mod.rs",
        "args.pack_profile.or(lens_pack_profile).unwrap_or_default()",
        "Omitted pack output profile flags intentionally fall back through the lens overlay to the documented default profile.",
    ),
    allowed(
        "NSF-CLI-RESOURCE-PROFILE-LENS-DEFAULT",
        "src/cli/mod.rs",
        ".or(lens_resource_profile)",
        "Omitted resource profile flags intentionally fall back through the lens overlay to the documented default profile.",
    ),
    allowed(
        "NSF-CLI-RENDERED-MARKDOWN-OPTIONAL",
        "src/cli/mod.rs",
        "report.rendered_markdown.clone().unwrap_or_default()",
        "Rendered markdown is an optional report field for human display; structured report data remains the machine-facing surface.",
    ),
    allowed(
        "NSF-CLI-OPTIONAL-INPUT-PATH-EMPTY",
        "src/cli/mod.rs",
        "let Some(path) = path else",
        "An omitted optional input path yields an empty record list by documented behavior; read or parse failures on a provided path still error.",
    ),
    allowed(
        "NSF-CLI-BROKER-NO-SOURCES",
        "src/cli/mod.rs",
        "fn verification_broker_source_label",
        "Verification broker readers return empty evidence only when both optional source paths are omitted; provided sources still surface their failures.",
    ),
    allowed(
        "NSF-CLI-EE-ERROR-CODE-OPTIONAL",
        "src/cli/mod.rs",
        "from_ee_error(code.unwrap_or_default(), message)",
        "An absent ee error code normalizes to a code-less canonical diagnostic; the message and source are preserved.",
    ),
    allowed(
        "NSF-CLI-ENVELOPE-DEGRADED-ARRAY",
        "src/cli/mod.rs",
        ".get(\"degraded\")",
        "Response envelopes without a degraded array truly carry no degradations; the consumer-side default keeps the envelope contract stable.",
    ),
    allowed(
        "NSF-CLI-DEGRADED-TABLE-REPAIR",
        "src/cli/mod.rs",
        "entry[\"repair\"].as_str().unwrap_or_default()",
        "Degradation repair text is optional in human-facing degraded tables; machine consumers read the structured envelope instead.",
    ),
    allowed(
        "NSF-CLI-DEGRADED-LIST-REPAIR-CLONE",
        "src/cli/mod.rs",
        "entry.repair.clone().unwrap_or_default()",
        "Degradation repair hints are optional when flattening entries for the human-facing degraded table; the structured envelope keeps the full entry.",
    ),
    allowed(
        "NSF-CLI-VERIFIER-EVIDENCE-OPTIONAL",
        "src/cli/mod.rs",
        "let Some(path) = &args.verifier_evidence else",
        "An omitted optional verifier-evidence path yields an empty evidence list by documented behavior; reading a provided path still errors loudly.",
    ),
    allowed(
        "NSF-CLI-CLOSEOUT-RUNS-OPTIONAL",
        "src/cli/mod.rs",
        "verification_run_records_from_j1_jsonl(&input)",
        "Verification closeout run records are empty only when no run JSONL input was provided; a provided input still surfaces parse failures.",
    ),
    allowed(
        "NSF-CLI-SERVE-STARTUP-DEGRADED",
        "src/cli/mod.rs",
        ".pointer(\"/startup/degraded\")",
        "Serve listener metadata without a startup degraded array truly carries no startup degradations; the envelope keeps an explicit empty array.",
    ),
    allowed(
        "NSF-CLI-MAINTENANCE-DEGRADED-REPAIR",
        "src/cli/mod.rs",
        "data[\"repair\"].as_str().unwrap_or_default()",
        "Maintenance degraded-table repair text is optional human-facing detail; severity and message fall back to explicit defaults alongside.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-OUTPUT-TAILS",
        "src/core/swarm_brief.rs",
        "evidence.output.stdout_tail.clone().unwrap_or_default()",
        "Source evidence output tails are capped optional captures; an absent tail is reported as empty output for that source, not as source success.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-EPOCH-MS",
        "src/core/swarm_brief.rs",
        "duration.as_millis().try_into().unwrap_or(u64::MAX)",
        "Clock conversion saturates explicitly to u64::MAX and only defaults when the system clock itself fails; brief freshness fields stay advisory.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-BV-TOP-PICKS",
        "src/core/swarm_brief.rs",
        "item.get(\"id\").and_then(Value::as_str)",
        "A BV snapshot without recommendation ids renders an empty top-picks list; BV source health is reported separately in source status.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-OPTIONAL-STRING-ARRAYS",
        "src/core/swarm_brief.rs",
        ".filter_map(Value::as_str)",
        "Summary string arrays (degraded codes, scenario ids, evidence hashes) are optional sections; absent arrays truly mean no entries for that section.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-ARTIFACT-MODIFIED-MS",
        "src/core/swarm_brief.rs",
        "u64::try_from(duration.as_millis()).ok()",
        "Replay artifact modified-time is advisory ordering metadata; unreadable metadata already returned None before this conversion.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-INCIDENT-SCENARIOS",
        "src/core/swarm_brief.rs",
        "incident.get(\"scenarioId\").and_then(Value::as_str)",
        "Incident summaries without scenario ids render an empty list while incident counts and statuses stay explicit.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-MAIL-RESERVATIONS",
        "src/core/swarm_brief.rs",
        ".filter_map(parse_file_reservation)",
        "An Agent Mail snapshot without a reservations array truly has no reservations; snapshot staleness is reported through source freshness.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-MAIL-HEALTH-LEVEL",
        "src/core/swarm_brief.rs",
        "format!(\" with healthLevel={level}\")",
        "Agent Mail health level is optional narrative detail in degradation messages; the degradation entry itself is always emitted.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-MAIL-SEMANTIC-STATUS",
        "src/core/swarm_brief.rs",
        "format!(\", semanticStatus={status}\")",
        "Agent Mail semantic status is optional narrative detail in degradation messages; the degradation entry itself is always emitted.",
    ),
    allowed(
        "NSF-SWARM-BRIEF-RCH-WORKERS",
        "src/core/swarm_brief.rs",
        "rch_worker_pressure_observation(index, worker)",
        "RCH telemetry without a workers array yields no per-worker observations; RCH source status still reports availability explicitly.",
    ),
    allowed(
        "NSF-CURATE-PROMOTION-LEVEL-ARROW",
        "src/core/curate.rs",
        "format!(\" -> {level}\")",
        "The optional promotion level renders as an empty arrow suffix in human-facing candidate text; candidate data is unchanged.",
    ),
    allowed(
        "NSF-CURATE-SESSION-SPANS",
        "src/core/curate.rs",
        "spans_by_session.remove(&session.id).unwrap_or_default()",
        "A session with no entry in the span map truly contributed zero evidence spans; the map was built from the same import batch.",
    ),
    allowed(
        "NSF-CURATE-SOURCE-ID-SPLIT",
        "src/core/curate.rs",
        ".split(',')",
        "An absent candidate source id splits to an empty id set for duplicate matching; it cannot mask a stored source id.",
    ),
    allowed(
        "NSF-CURATE-PACKAGE-KEY-TARGET",
        "src/core/curate.rs",
        "candidate.target_memory_id.clone().unwrap_or_default()",
        "Non-derived candidates without a target memory id intentionally key their dedup package on the empty target.",
    ),
    allowed(
        "NSF-CURATE-CANONICAL-KEYS",
        "src/core/curate.rs",
        "canonical_json_key",
        "Canonical dedup keys serialize freshly built JSON values; the keys group candidates for review and never replace stored candidate data.",
    ),
    allowed(
        "NSF-CURATE-DERIVED-OPTIONAL-COLLECTIONS",
        "src/core/curate.rs",
        "CurateShowPlannedDerivedLink",
        "Derived-candidate optional inputs (source refs, attachments) are legitimately empty when the derivation spec omits them.",
    ),
    allowed(
        "NSF-CURATE-TAG-FILTER",
        "src/core/curate.rs",
        "!tag.is_empty()",
        "Filtering empty tag strings from an optional tag list legitimately yields an empty tag set.",
    ),
    allowed(
        "NSF-CURATE-DERIVATION-REFS-KEY",
        "src/core/curate.rs",
        "fn canonical_derivation_memory_spec_key",
        "Canonical dedup keys serialize freshly built JSON payloads for review grouping only; stored candidate data is never replaced.",
    ),
    allowed(
        "NSF-CURATE-PLANNED-ATTACHMENTS",
        "src/core/curate.rs",
        "let planned_attachments = derived",
        "Planned attachment previews are empty when the derived input carries no evidence refs; the application decision stays explicit.",
    ),
    allowed(
        "NSF-CURATE-PLANNED-APPLICATION",
        "src/core/curate.rs",
        "CurateShowPlannedApplication",
        "Planned application previews collect optional derived references that are legitimately empty when the spec omits them.",
    ),
    allowed(
        "NSF-STREAMING-TRAILER-OPTIONAL-SECTIONS",
        "src/output/streaming.rs",
        "optional_object(&pack, \"provenanceFooter\")",
        "Pack stream trailer sections are schema-optional; optional_object/array errors on malformed values still propagate and absence renders as empty sections.",
    ),
    allowed(
        "NSF-STREAMING-ITEM-OPTIONAL-SECTIONS",
        "src/output/streaming.rs",
        "optional_object(item, \"trust\")",
        "Pack stream item metadata sections are schema-optional; optional_object/array errors on malformed values still propagate and absence renders as empty sections.",
    ),
    allowed(
        "NSF-STREAMING-DEGRADED-REPAIR",
        "src/output/streaming.rs",
        "entry.repair.clone().unwrap_or_default()",
        "Degradation repair hints are optional in stream frames; the degradation code and message are always emitted.",
    ),
    allowed(
        "NSF-CONTEXT-WRITE-OVERHEAD-SATURATION",
        "src/core/context.rs",
        ".checked_sub(self.record_write + self.item_writes + self.omission_writes)",
        "Persistence overhead timing saturates to zero on underflow; it is advisory timing telemetry, not pack data.",
    ),
    allowed(
        "NSF-CONTEXT-MEMORY-TAGS-OPTIONAL",
        "src/core/context.rs",
        ".get_memory_tags(&target_memory.id)",
        "Tag lookups for related-memory expansion default to no tags; tags only widen candidate discovery and never alter stored memories.",
    ),
    allowed(
        "NSF-CONTEXT-L2-DIRECTORY-OPTIONAL",
        "src/core/context.rs",
        "config.directory.clone()",
        "A missing L2 cache directory configuration intentionally disables the cache path rather than failing pack assembly.",
    ),
    allowed(
        "NSF-CONTEXT-HASH-OPTIONAL-INPUTS",
        "src/core/context.rs",
        "timestamp.to_rfc3339()",
        "Optional as-of timestamps hash as the empty string deterministically; the hash input set is fixed and documented.",
    ),
    allowed(
        "NSF-CONTEXT-HASH-PPR-WEIGHT",
        "src/core/context.rs",
        "weight.to_bits().to_string()",
        "Optional PPR weights hash as the empty string deterministically; absent weights are a valid retrieval configuration.",
    ),
    allowed(
        "NSF-CONTEXT-HASH-SNAPSHOT-PATH",
        "src/core/context.rs",
        "path.to_string_lossy().into_owned()",
        "Optional coordination snapshot paths hash as the empty string deterministically; absent snapshots are a valid configuration.",
    ),
    allowed(
        "NSF-CONTEXT-READ-POOL-DEFAULT",
        "src/core/context.rs",
        "config.storage.read_pool",
        "A workspace without storage configuration uses the documented default read-pool settings; environment overrides still apply afterward.",
    ),
    allowed(
        "NSF-CONTEXT-OMISSIONS-NO-CANDIDATES",
        "src/core/context.rs",
        "if candidates.is_empty()",
        "An empty candidate list has no omissions to evaluate; policy and storage failures still propagate through ContextPackError.",
    ),
    allowed(
        "NSF-CONTEXT-WHY-NOT-TAGS",
        "src/core/context.rs",
        "connection.get_memory_tags(&memory.id).unwrap_or_default()",
        "Why-not explanations treat tags as optional descriptive metadata; the explanation itself is built from the loaded memory record.",
    ),
    allowed(
        "NSF-LAB-HOST-CLASS-UNKNOWN-OBSERVATION",
        "src/core/lab.rs",
        "observation.logical_cpu_count.unwrap_or_default()",
        "Unknown CPU or memory observations already emit swarm_replay_cpu_unknown/swarm_replay_memory_unknown degraded codes; the zero default only steers the conservative Smoke classification.",
    ),
    allowed(
        "NSF-LAB-LATENCY-EMPTY-SAMPLES",
        "src/core/lab.rs",
        "samples.last().copied().unwrap_or_default()",
        "Latency summaries over zero samples truly have zero max latency; sample counts are reported alongside.",
    ),
    allowed(
        "NSF-LAB-RATIO-DIV-ZERO",
        "src/core/lab.rs",
        ".checked_div(denominator)",
        "Basis-point ratios over a zero denominator saturate to zero explicitly; counts are reported alongside the ratio.",
    ),
    allowed(
        "NSF-LAB-REPLAY-HASH-FIRST",
        "src/core/lab.rs",
        "replay_hashes.first().cloned().unwrap_or_default()",
        "The first-replay-hash default applies only to empty run sets whose emptiness the identical-run check reports explicitly.",
    ),
    allowed(
        "NSF-LAB-NORMALIZED-RUN-FIRST",
        "src/core/lab.rs",
        "normalized_runs.first().cloned().unwrap_or_default()",
        "The first-normalized-run default applies only to empty run sets whose emptiness the identical-run check reports explicitly.",
    ),
    allowed(
        "NSF-LAB-SWAP-REVISION-DEFAULT",
        "src/core/lab.rs",
        "swap.swap_revision.unwrap_or_default()",
        "Intervention swap revisions default to the documented at-capture revision mode when the spec omits an explicit revision.",
    ),
    allowed(
        "NSF-SUPPORT-OPTIONAL-SUMMARY-ARRAYS",
        "src/core/support_bundle.rs",
        "})\n.unwrap_or_default()",
        "Support-bundle summaries collect optional redaction-safe arrays (codes, hashes, safe strings) that are legitimately empty when the underlying summary section is absent; section statuses stay explicit.",
    ),
    allowed(
        "NSF-SUPPORT-QOS-EPOCH",
        "src/core/support_bundle.rs",
        "Utc::now().timestamp_millis().try_into().unwrap_or_default()",
        "The QoS lane summary timestamp saturates to zero only if the system clock fails; lane data and statuses are reported alongside.",
    ),
    // ---- bd-3gk66 batch 2: long-tail census classification (2026-06-11) ----
    allowed(
        "NSF-SNA-CANDIDATE-STALE-REASONS",
        "src/core/swarm_next_action.rs",
        ".map(|candidate| candidate.stale_reasons.clone())",
        "A missing candidate carries no stale or missing-field reasons; the empty lists truly describe an absent candidate.",
    ),
    allowed(
        "NSF-SNA-CANDIDATE-UNSAFE-REASONS",
        "src/core/swarm_next_action.rs",
        ".map(|candidate| candidate.unsafe_reasons.clone())",
        "A missing candidate has no unsafe reasons to report; the empty list truly describes an absent candidate.",
    ),
    allowed(
        "NSF-SNA-BLOCKAGE-PATH-KEY",
        "src/core/swarm_next_action.rs",
        ".and_then(Value::as_str)",
        "An absent path normalizes to an empty dedup key for blockage reasons; the blockage entry itself is still emitted.",
    ),
    allowed(
        "NSF-SNA-KNOWN-BLOCKER-CODES",
        "src/core/swarm_next_action.rs",
        "string_array_from_keys(known_blocker,",
        "A known blocker without a degraded-codes array truly contributes no codes; other evidence sources still feed the set.",
    ),
    allowed(
        "NSF-SNA-CARD-SORT-KEY",
        "src/core/swarm_next_action.rs",
        "std::cmp::Reverse(card.candidate_id.clone().unwrap_or_default())",
        "An absent candidate id participates only as a deterministic sort key for recommendation cards.",
    ),
    allowed(
        "NSF-SNA-AFFECTED-COMMAND-KINDS",
        "src/core/swarm_next_action.rs",
        ".map(affected_command_kinds)",
        "Blocker evidence without affected commands truly affects no command kinds; the empty list is the true result.",
    ),
    allowed(
        "NSF-CLI-MESH-WORKSPACE-LISTS",
        "src/cli/mesh.rs",
        "load_workspace_lists(&workspace_path).unwrap_or_default()",
        "Absent workspace allow/deny lists are a valid empty discovery policy; present malformed lists surface through their own parse path.",
    ),
    allowed(
        "NSF-CLI-MESH-DISCOVERY-MODE-DEFAULT",
        "src/cli/mesh.rs",
        ".or(config_modes.discovery_mode)",
        "An omitted discovery mode intentionally selects the documented default mode.",
    ),
    allowed(
        "NSF-CLI-MESH-RESPOND-MODE-DEFAULT",
        "src/cli/mesh.rs",
        ".or(config_modes.respond_mode)",
        "An omitted respond mode intentionally selects the documented default mode.",
    ),
    allowed(
        "NSF-CLI-MESH-NO-REVOCATIONS",
        "src/cli/mesh.rs",
        "return Ok(Vec::new());",
        "An empty node-key set has no peer revocations to apply; the empty result is the true outcome.",
    ),
    allowed(
        "NSF-CLI-MESH-COMMAND-MODE-DEFAULT",
        "src/cli/mesh.rs",
        ".unwrap_or(false);",
        "An omitted mesh command mode intentionally selects the documented default mode.",
    ),
    allowed(
        "NSF-MESH-AE-ACTIVE-HEADS",
        "src/mesh/anti_entropy_model.rs",
        "active_head_event_ids: heads",
        "A logical memory absent from the active-head map truly has no active head events.",
    ),
    allowed(
        "NSF-MESH-AE-LAST-EVENT-HASH",
        "src/mesh/anti_entropy_protocol.rs",
        "first_event_hash: events[0].event_hash.clone()",
        "The last-event hash defaults only for an empty event window whose emptiness the protocol frame reports explicitly.",
    ),
    allowed(
        "NSF-MESH-POLICY-OPTIONAL-PEERS",
        "src/mesh/policy.rs",
        ".unwrap_or_default()",
        "An absent peer-policy list is the documented open default; configured policies still apply verbatim.",
    ),
    allowed(
        "NSF-MESH-TS-SELF-NODE-KEY",
        "src/mesh/tailscale_autodiscovery.rs",
        "local.self_node_key.as_deref().unwrap_or_default()",
        "An absent self node key only relaxes self-filtering during peer discovery; peer records keep their own keys.",
    ),
    allowed(
        "NSF-CLI-SANDBOX-NO-BASELINE",
        "src/cli/sandbox.rs",
        "return Ok(Vec::new());",
        "A sandbox without a baseline database or workspace truly has no baseline memories to diff.",
    ),
    allowed(
        "NSF-CORE-SANDBOX-SESSION-LOAD",
        "src/core/sandbox.rs",
        "std::fs::read_to_string(path)",
        "Sandbox scratch sessions are documented to load as empty when the scratch file is absent or corrupt; sandbox state is disposable by design.",
    ),
    allowed(
        "NSF-CORE-SOURCE-RUN-STDERR-TAIL",
        "src/core/source_run.rs",
        "evidence.output.stderr_tail.as_deref().unwrap_or_default()",
        "Source-run stderr tails are capped optional captures; an absent tail renders as empty detail while the run status stays explicit.",
    ),
    allowed(
        "NSF-CORE-QOS-REGISTRY-DEFAULT",
        "src/core/qos.rs",
        "read_registry_document(&path)?.unwrap_or_default()",
        "A missing QoS registry document starts empty by design; real read failures propagate through the ? operator.",
    ),
    allowed(
        "NSF-CORE-PLAN-CLOCK-FALLBACK",
        "src/core/plan.rs",
        ".unwrap_or_default();",
        "The pseudo-random id clock fallback applies only when the system clock predates the UNIX epoch; plan identity remains locally unique.",
    ),
    allowed(
        "NSF-DOMINANCE-FRONTIER-ABSENT",
        "src/graph/dominance.rs",
        "frontiers.get(memory_id).cloned().unwrap_or_default()",
        "A node absent from the dominance-frontier map truly has an empty frontier.",
    ),
    allowed(
        "NSF-DOMINANCE-DEGREE-CHECKS",
        "src/graph/dominance.rs",
        "!graph.predecessors(node).unwrap_or_default().is_empty()",
        "Root-finding degree checks treat an absent edge list as zero edges; graph construction failures propagate earlier.",
    ),
    allowed(
        "NSF-DOMINANCE-SUCCESSOR-CHECK",
        "src/graph/dominance.rs",
        "!graph.successors(node).unwrap_or_default().is_empty()",
        "Root-finding degree checks treat an absent successor list as zero outgoing edges.",
    ),
    allowed(
        "NSF-DOMINANCE-BFS-PREDECESSORS",
        "src/graph/dominance.rs",
        "let predecessors = graph.predecessors(&node).unwrap_or_default()",
        "The frontier walk treats an absent predecessor list as a leaf node.",
    ),
    allowed(
        "NSF-WHY-REVISION-SUCCESSORS",
        "src/core/why.rs",
        "graph.successors(memory_id).unwrap_or_default().is_empty()",
        "Revision-context detection treats an absent successor list as no revision edges.",
    ),
    allowed(
        "NSF-WHY-DOMINANCE-DOWNGRADE",
        "src/core/why.rs",
        ".unwrap_or_default();",
        "Dominance computation failures downgrade the why explanation to the non-graph path; the explanation itself still reports its sources.",
    ),
    allowed(
        "NSF-WHY-FRONTIER-ABSENT",
        "src/core/why.rs",
        "frontiers.get(memory_id).cloned().unwrap_or_default()",
        "A memory absent from the frontier map truly has an empty dominance frontier.",
    ),
    allowed(
        "NSF-WHY-BFS-PREDECESSORS",
        "src/core/why.rs",
        "graph.predecessors(&id).unwrap_or_default()",
        "The ancestry walk treats an absent predecessor list as zero incoming edges.",
    ),
    allowed(
        "NSF-CAUSAL-FAILURE-SUCCESSORS",
        "src/graph/causal.rs",
        "graph.successors(failure_id).unwrap_or_default().is_empty()",
        "Causal path analysis treats an absent successor list as zero outgoing causal edges.",
    ),
    allowed(
        "NSF-COOP-REFRESH-PARTIAL-SCORES",
        "src/graph/cooperative_refresh.rs",
        ".unwrap_or_default();",
        "Cooperative refresh merges partial centrality results; an algorithm that timed out contributes no scores and the timeout is reported in refresh status.",
    ),
    allowed(
        "NSF-TAG-BITMAPS-FIRST-TAG",
        "src/search/tag_bitmaps.rs",
        "self.by_tag.get(first).cloned().unwrap_or_default()",
        "A tag absent from the bitmap index truly matches zero documents, the correct intersection base case.",
    ),
    allowed(
        "NSF-HOTSET-OPTIONAL-ARRAYS",
        "src/cache/hotset.rs",
        "return Ok(Vec::new());",
        "Hotset snapshot parsers treat absent optional arrays as empty sections; malformed present values still fail parsing.",
    ),
    allowed(
        "NSF-GRAPH-MEMORY-BUDGET-CONFIG",
        "src/core/graph_memory_budget.rs",
        ".unwrap_or_default();",
        "Graph memory budgets are advisory tuning; an absent budget configuration uses the documented defaults.",
    ),
    allowed(
        "NSF-GOVERNOR-POSITION-KEYS",
        "src/output/governor.rs",
        ".unwrap_or_default();",
        "Governor position keys default to empty placement strings for elements without positions; element payloads are not altered.",
    ),
    allowed(
        "NSF-OUTPUT-DEGRADED-REPAIR-CLONE",
        "src/output/mod.rs",
        "entry.repair.clone().unwrap_or_default()",
        "Degradation repair hints are optional in human-facing rendering; the degradation code and message are always emitted.",
    ),
    allowed(
        "NSF-OUTPUT-INTEGRITY-REPAIR",
        "src/output/mod.rs",
        "entry.repair.unwrap_or_default()",
        "Integrity degradation repair hints are optional in human-facing rendering; the entry itself is always emitted.",
    ),
    allowed(
        "NSF-OUTPUT-AUDIT-SHARD-ID",
        "src/output/mod.rs",
        ".map(|value| format!(\" shard={value}\"))",
        "Audit issue shard ids are optional display detail in the human-facing timeline.",
    ),
    allowed(
        "NSF-FOCUS-DEGRADED-REPAIR",
        "src/core/focus.rs",
        "entry.repair.clone().unwrap_or_default()",
        "Degradation repair hints are optional in focus rendering; the degradation entry is always emitted.",
    ),
    allowed(
        "NSF-FOCUS-SUGGEST-MEMORY-LINKS",
        "src/core/focus_suggest.rs",
        ".unwrap_or_default();",
        "A suggestion without memory links truly derives from zero memories; the suggestion payload stays explicit.",
    ),
    allowed(
        "NSF-SERVE-DEGRADED-CODES",
        "src/serve.rs",
        ".filter_map(|entry| entry.get(\"code\").and_then(JsonValue::as_str))",
        "Serve payloads without a degraded array truly carry no degradations; the extracted code list mirrors the envelope.",
    ),
    allowed(
        "NSF-SERVE-EXCHANGE-STATUS-LINE",
        "src/serve.rs",
        "response.lines().next().unwrap_or_default()",
        "The recorded status line comes from the server's own freshly rendered response, and every render path emits an HTTP/1.1 status line first; there is no inbound response that could be malformed here.",
    ),
    allowed(
        "NSF-SERVE-JOB-TABLE-ABSENT",
        "src/serve.rs",
        "daemon_job_table_path_is_regular_file(&table_path, \"read\")?",
        "An absent daemon job table is path-validated and yields an empty job list by design; read failures on a present table still error.",
    ),
    allowed(
        "NSF-DAEMON-OPTIONAL-CONFIG-FIELDS",
        "src/daemon/server.rs",
        ".transpose()?",
        "Optional daemon config fields default only when absent; malformed present values surface through the transpose ? operator.",
    ),
    allowed(
        "NSF-DAEMON-SOCKET-NAME",
        "src/daemon/server.rs",
        ".map(|name| name.to_os_string())",
        "Socket file-name extraction defaults to an empty component only for pathological bind paths; binding still validates the final path.",
    ),
    allowed(
        "NSF-CONFIG-LENS-OVERRIDES-ABSENT",
        "src/config/file.rs",
        "item_path(document, &[\"task_lens\"], \"overrides\")",
        "An absent task_lens.overrides table is explicit input absence; present malformed tables still return ConfigParseError.",
    ),
    allowed(
        "NSF-CONFIG-LENS-OPTIONAL-ARRAYS",
        "src/config/file.rs",
        "optional_table_string_array(table, &prefix, \"allowed_kinds\")?",
        "Optional lens string-array fields default to empty when absent; malformed present arrays still error through the ? operator.",
    ),
    allowed(
        "NSF-VERIFICATION-RCH-STATUS-NORMALIZE",
        "src/models/verification.rs",
        "raw_status.unwrap_or_default().trim().to_ascii_lowercase()",
        "RCH status normalization maps an absent status to the explicit empty-string match arm; the verdict logic handles it deliberately.",
    ),
    allowed(
        "NSF-VERIFICATION-GHA-NORMALIZE",
        "src/models/verification.rs",
        "status.unwrap_or_default().trim().to_ascii_lowercase()",
        "GitHub Actions status and conclusion normalize absent values to the explicit empty-string match arms.",
    ),
    allowed(
        "NSF-VERIFICATION-GHA-EXIT-CODE",
        "src/models/verification.rs",
        "match (status, conclusion.unwrap_or_default())",
        "The GHA exit-code mapping handles an absent conclusion through explicit match arms.",
    ),
    allowed(
        "NSF-MEMORY-ANCHOR-FIRST-TOKEN",
        "src/models/memory_anchor.rs",
        "line.split_whitespace().next().unwrap_or_default()",
        "Command-token extraction defaults to empty for blank lines, which match no anchor command patterns.",
    ),
    allowed(
        "NSF-MODELS-REFLECTION-RECOVERY",
        "src/models/mod.rs",
        ".map(derivation_reflection_recovery_actions_for_code)",
        "No inferred degradation code means no recovery actions to suggest; the reflection status stays explicit.",
    ),
    allowed(
        "NSF-HOOKS-CHAIN-CONTENT-ABSENT",
        "src/hooks/installer.rs",
        "let mut combined_content = content.unwrap_or_default()",
        "An absent chained hook file contributes empty content to the combined script; the chain target itself is still validated and written.",
    ),
    allowed(
        "NSF-DB-MIGRATE-ROW-COUNTS",
        "src/db/migrate.rs",
        "actual.get(table).copied().unwrap_or_default()",
        "Shard-migration verification treats a table absent from the count map as zero rows, which the equality check then reports honestly.",
    ),
    allowed(
        "NSF-DB-LEDGER-NOTE",
        "src/db/mod.rs",
        "note.contains(\"cross_shard_read\")",
        "An absent ledger note simply fails the substring predicate; ledger entries keep their structured fields.",
    ),
    allowed(
        "NSF-DB-MESH-FAILURE-ACTION",
        "src/db/mod.rs",
        "let failure_action = failure",
        "An absent mesh failure action fails the equality predicate explicitly rather than matching any action.",
    ),
    allowed(
        "NSF-DB-MESH-FAILURE-CODE",
        "src/db/mod.rs",
        "let failure_code = failure",
        "An absent mesh failure code fails the prefix predicate explicitly rather than matching any code class.",
    ),
    allowed(
        "NSF-DB-PROVENANCE-URI-CHECK",
        "src/db/mod.rs",
        ".provenance_uri",
        "An absent provenance URI fails the containment predicate; stored provenance is never rewritten.",
    ),
    allowed(
        "NSF-DB-SHARD-AUDIT-ROWS",
        "src/db/shard.rs",
        ".unwrap_or_default();",
        "Shard audit-row serialization contributes an empty detail section only when there are no expected rows to record.",
    ),
    allowed(
        "NSF-EVAL-EXPECTED-SCENARIOS",
        "src/eval/runner.rs",
        ".unwrap_or_default();",
        "A fixture without expectations requires zero scenarios; coverage checks then pass trivially and honestly.",
    ),
    allowed(
        "NSF-BEADS-PARSE-ERROR-LINES",
        "src/core/beads_integrity.rs",
        ".map(|error| vec![error.line])",
        "No JSONL parse error means no invalid line numbers to report; the repair context flag keys off the same option.",
    ),
    allowed(
        "NSF-BEADS-MERGE-ARTIFACTS",
        "src/core/beads_integrity.rs",
        "doctor_string_array_detail(check, &[\"files\", \"paths\", \"artifacts\"])",
        "A doctor check without artifact details truly lists no merge artifacts; check presence is evaluated separately.",
    ),
    allowed(
        "NSF-BEADS-DOCTOR-MESSAGE",
        "src/core/beads_integrity.rs",
        "fn doctor_message(check: &serde_json::Value)",
        "A doctor check without a message reads as empty text for keyword classification; the check status fields stay authoritative.",
    ),
    allowed(
        "NSF-BEADS-EXCERPT-INVARIANT",
        "src/core/beads_integrity.rs",
        ".map(|e| e.excerpt.len())",
        "An absent parse error has a zero-length excerpt for the truncation invariant; the bound only constrains present excerpts.",
    ),
    allowed(
        "NSF-DOCTOR-UNDO-LOG-READ",
        "src/core/doctor_runtime.rs",
        "fs::read_to_string(&undo_log_path)",
        "An unreadable undo log yields no already-undone sequences, so undo conservatively replays from the full action list.",
    ),
    allowed(
        "NSF-DOCTOR-DRIFT-REPORT-HASH",
        "src/core/doctor_runtime.rs",
        "DoctorRuntimeError::UndoStateDrifted",
        "The drift comparison itself uses the optional after-hash; the empty default only fills the expected-hash field of the error already being raised.",
    ),
    allowed(
        "NSF-DOCTOR-RUN-ID-CLOCK",
        "src/core/doctor_runtime.rs",
        "now.timestamp_nanos_opt().unwrap_or_default()",
        "The run-id clock component defaults only for pre-epoch clocks; the sequence counter keeps run ids unique.",
    ),
    allowed(
        "NSF-AGENTSMD-DEDUP-SIMILARITY",
        "src/core/agentsmd.rs",
        "proposal.dedup_similarity.unwrap_or_default()",
        "Dedup similarity is optional display detail in the promotion message; the target memory id is the operative content.",
    ),
    allowed(
        "NSF-PREFLIGHT-ENV-SEGMENTS",
        "src/core/preflight_guard.rs",
        "segment.get(index + 1..).unwrap_or_default()",
        "Environment-assignment splitting treats a missing trailing segment as empty text; the guard still scans the full command line.",
    ),
    allowed(
        "NSF-TAILSCALE-EE-CAPS",
        "src/core/tailscale_probe.rs",
        "ee_version: ee_version.unwrap_or_default()",
        "Peers without advertised ee capabilities are valid non-ee nodes; empty capability strings describe them truthfully.",
    ),
    allowed(
        "NSF-DOCTOR-QOS-EPOCH",
        "src/core/doctor.rs",
        "Utc::now().timestamp_millis().try_into().unwrap_or_default()",
        "The QoS timestamp saturates to zero only if the system clock fails; probe statuses are reported separately.",
    ),
    allowed(
        "NSF-HYGIENE-UTF8-RECOVERY",
        "src/core/hygiene_beads_state.rs",
        "std::str::from_utf8(&bytes[..error.valid_up_to()])",
        "Truncated-write recovery keeps the valid UTF-8 prefix; full parse failures still report a parse error.",
    ),
    allowed(
        "NSF-AUDIT-LANE-WORKSPACE-ID",
        "src/core/audit_lane.rs",
        "workspace_id: input.workspace_id.clone().unwrap_or_default()",
        "from_audit_input and to_audit_input are a lossless pair: the empty-string sentinel round-trips back to None before every audit write, so no fabricated workspace id is persisted.",
    ),
    allowed(
        "NSF-JOURNAL-FIRST-LINE",
        "src/core/journal.rs",
        "entry.body.lines().next().unwrap_or_default()",
        "Journal distillation derives display text from the first body line; an empty body yields empty display text without altering the entry.",
    ),
    allowed(
        "NSF-JOURNAL-STDERR-TAIL",
        "src/core/journal.rs",
        "distill_structured_str(entry, \"stderrTail\")",
        "A journal entry without a stderr tail embeds empty text in the deterministic refinement input.",
    ),
    allowed(
        "NSF-JOURNAL-DEDUP-SIMILARITY",
        "src/core/journal.rs",
        "proposal.dedup_similarity.unwrap_or_default()",
        "Dedup similarity is optional display detail in the proposal reason text.",
    ),
    allowed(
        "NSF-RECALL-ANCHOR-DISPLAY",
        "src/core/recall.rs",
        ".or(item.anchor.symbol.as_deref())",
        "Anchor display text is optional rendering detail when neither path nor symbol is present.",
    ),
    allowed(
        "NSF-RECALL-ANCHOR-PREFERENCE",
        "src/core/recall.rs",
        ".or_else(|| row.symbol.clone())",
        "Anchor preference uses the empty string only as a deterministic dedup tiebreaker.",
    ),
    allowed(
        "NSF-RECALL-TAGS-ABSENT",
        "src/core/recall.rs",
        ".map(|candidate| {",
        "A recall row without tags truly has no tags; the item payload stays explicit.",
    ),
    allowed(
        "NSF-RECALL-PROVENANCE-ABSENT",
        "src/core/recall.rs",
        "uri: uri.clone(),",
        "A recall row without a provenance URI renders an empty provenance array; absence is not a fetch failure.",
    ),
    allowed(
        "NSF-SEARCH-DEGRADED-REPAIR",
        "src/core/search.rs",
        "entry.repair.clone().unwrap_or_default()",
        "Degradation repair hints are optional in search degradation aggregation; codes and messages are always kept.",
    ),
    allowed(
        "NSF-SEARCH-CALIBRATION-METADATA",
        "src/core/search.rs",
        ".and_then(|value| value.as_object().cloned())",
        "Calibration metadata insertion starts from an empty object when the hit has none; the calibration fields are then added explicitly.",
    ),
    allowed(
        "NSF-SEARCH-CALIBRATION-HASH-COMMENT",
        "src/core/search.rs",
        "the recalibrate run is non-mutating in this branch",
        "This line is a comment documenting the deliberate empty-bytes hash fallback below it.",
    ),
    allowed(
        "NSF-SEARCH-CALIBRATION-HASH-BYTES",
        "src/core/search.rs",
        "return Ok(SearchScoreRecalibrationReport {",
        "The calibration feedback hash deliberately covers empty bytes when the file is absent or capped, as the adjacent comment documents.",
    ),
    allowed(
        "NSF-HANDOFF-ATTEST-HASHES",
        "src/core/handoff.rs",
        ".filter_map(serde_json::Value::as_str)",
        "Handoff previews render absent attestation bundle hashes as an empty list; capsule integrity fields stay explicit.",
    ),
    allowed(
        "NSF-HANDOFF-HYPOTHESIS-CODES",
        "src/core/handoff.rs",
        ".filter_map(|hypothesis| hypothesis.get(\"code\").and_then(serde_json::Value::as_str))",
        "Handoff previews render absent hypothesis codes as an empty list; the diagnostic summary status stays explicit.",
    ),
    allowed(
        "NSF-IMPACT-FALLBACK-DEGRADED",
        "src/core/impact.rs",
        "search_degraded_data_json(\"impact.search_fallback\"",
        "No fallback search report means no fallback degradations; the impact surface status is reported alongside.",
    ),
    allowed(
        "NSF-IMPACT-RESULTS-ABSENT",
        "src/core/impact.rs",
        ".unwrap_or_default();",
        "An absent results array renders zero hits; search-level failures are reported through the search report itself.",
    ),
    allowed(
        "NSF-PERF-LIVE-WORKER-HEALTH",
        "src/core/perf_live.rs",
        "infer_healthy_workers(&value)",
        "RCH worker health is advisory telemetry; absent counts render as zero while probe failures surface separately.",
    ),
    allowed(
        "NSF-PERF-LIVE-QUEUE-DEPTH",
        "src/core/perf_live.rs",
        "\"queued_count\",",
        "RCH queue depth is advisory telemetry; an absent metric renders as zero alongside the probe status.",
    ),
    allowed(
        "NSF-PRIMER-PROVENANCE-ABSENT",
        "src/core/primer.rs",
        ".unwrap_or_default(),",
        "A primer candidate without a provenance URI renders an empty provenance array; absence is not a fetch failure.",
    ),
    allowed(
        "NSF-PRIMER-CACHE-SERIALIZE",
        "src/core/primer.rs",
        ".unwrap_or_default();",
        "A primer cache payload that fails to serialize is caught by the empty-content guard and reported as a cache degradation.",
    ),
    allowed(
        "NSF-STATUS-SKYLINE-ROWS",
        "src/core/status.rs",
        ".map(|skyline| skyline.rows)",
        "An absent skyline snapshot contributes no rows; skyline availability is reported separately.",
    ),
    allowed(
        "NSF-STATUS-QOS-EPOCH",
        "src/core/status.rs",
        "Utc::now().timestamp_millis().try_into().unwrap_or_default()",
        "The QoS timestamp saturates to zero only if the system clock fails; lane summaries stay explicit.",
    ),
    allowed(
        "NSF-VERIFY-LEDGER-CODES-ABSENT",
        "src/core/verify_ledger.rs",
        ".filter(|s| !s.is_empty())",
        "A ledger row without degraded codes truly carries none; the row status stays explicit.",
    ),
    allowed(
        "NSF-VERIFY-LEDGER-CODES-PARSE",
        "src/core/verify_ledger.rs",
        "serde_json::from_str::<Vec<String>>(raw)",
        "Malformed stored degraded-code JSON degrades to an empty list that the subsequent validation pass re-derives and reports.",
    ),
    allowed(
        "NSF-CONFORMAL-SCORES-ABSENT",
        "src/core/conformal.rs",
        "load_conformal_nonconformity_scores",
        "Missing calibration residuals trigger the documented conservative quantile path.",
    ),
    allowed(
        "NSF-DOCS-BOOTSTRAP-FIRST-TOKEN",
        "src/core/docs_bootstrap.rs",
        ".unwrap_or_default()",
        "First-token extraction defaults to empty for blank input, which matches no bootstrap command.",
    ),
    allowed(
        "NSF-OUTCOME-QUARANTINE-PREVIEW",
        "src/core/outcome.rs",
        "harmful_burst_quarantine_degradation(q, &[])",
        "A dry run without a quarantine decision previews no quarantine degradations; the preview status stays explicit.",
    ),
    allowed(
        "NSF-CASS-STDOUT-DRAIN-TIMEOUT",
        "src/cass/process.rs",
        "Ok(Vec::new())",
        "The after-timeout drain only feeds outcomes that already carry timed_out=true or error paths that discard the bytes; the stderr side keeps a sentinel because its text is shown in diagnostics.",
    ),
    allowed(
        "NSF-OBS-EVIDENCE-FIELDS",
        "src/obs/verification_evidence.rs",
        ".filter_map(Value::as_str)",
        "Evidence field arrays are optional sections; absence truly means no fields for that record.",
    ),
    allowed(
        "NSF-OBS-FIRST-ERROR-LINE",
        "src/obs/verification_evidence.rs",
        ".unwrap_or_default();",
        "A first-error without a line number renders a location without one; the error text itself is preserved.",
    ),
    allowed(
        "NSF-STEWARD-DISTILL-CANDIDATES",
        "src/steward/mod.rs",
        ".map(|applied| applied.candidate_ids.clone())",
        "A distill run without applied candidates reports empty id lists; the run status stays explicit.",
    ),
    allowed(
        "NSF-STEWARD-DISTILL-AUDITS",
        "src/steward/mod.rs",
        ".map(|applied| applied.audit_ids.clone())",
        "A distill run without applied audits reports empty id lists; the run status stays explicit.",
    ),
    allowed(
        "NSF-PREFLIGHT-ENV-SEGMENT-VALUE",
        "src/core/preflight_guard.rs",
        "segment.get(index + 2..).unwrap_or_default()",
        "Environment-assignment splitting treats a missing trailing segment as empty text; the guard still scans the full command line.",
    ),
    allowed(
        "NSF-WORKSPACE-HYGIENE-MAPS",
        "src/core/workspace.rs",
        "symbols_by_path.get(path).cloned().unwrap_or_default()",
        "Hygiene lookups treat paths absent from the evidence maps as having no recorded risk entries; the maps were built from the same scan.",
    ),
    allowed(
        "NSF-AGENTSMD-MARKER-ATTR",
        "src/core/agentsmd.rs",
        "let attributes = line",
        "Marker attribute extraction yields empty attributes for non-matching prefixes; marker validation happens before extraction.",
    ),
    allowed(
        "NSF-AGENTSMD-NEXT-TOKEN",
        "src/core/agentsmd.rs",
        "tokens.get(index + 1).copied().unwrap_or_default()",
        "A missing next token reads as empty text and matches no modality keyword.",
    ),
    allowed(
        "NSF-TAILSCALE-OS-PARSE",
        "src/core/tailscale_probe.rs",
        "match value.unwrap_or_default().to_ascii_lowercase().as_str()",
        "An absent OS string classifies the peer platform as Other, the documented unknown bucket.",
    ),
    allowed(
        "NSF-ENV-ATTEST-PROCESS-COUNT",
        "src/core/environment_attestation.rs",
        ".unwrap_or_default();",
        "The local-cargo process count is advisory metadata; the scan status field reports probe health separately.",
    ),
    allowed(
        "NSF-HYGIENE-PATTERNS-ABSENT",
        "src/core/hygiene_classifier.rs",
        "return Ok(Vec::new());",
        "An absent hygiene pattern configuration yields the empty pattern set; present malformed values still fail parsing.",
    ),
    allowed(
        "NSF-DOMINANCE-REVISION-DEGREE",
        "src/graph/dominance.rs",
        "if !graph.has_node(memory_id)",
        "Revision-chain membership checks treat absent edge lists as zero edges for a node already verified present.",
    ),
    allowed(
        "NSF-DB-MIGRATE-COPY-COUNTS",
        "src/db/migrate.rs",
        "before.get(table).copied().unwrap_or_default()",
        "Copy-count derivation treats a table absent from the before map as zero rows; the saturating subtraction reports the honest delta.",
    ),
    allowed(
        "NSF-WORKSPACE-HYGIENE-ACTIVITY",
        "src/core/workspace.rs",
        "let agent_name_hashes = activity_by_path",
        "A path absent from the activity map truly has no recorded agent activity; the map was built from the same scan.",
    ),
    allowed(
        "NSF-WORKSPACE-HYGIENE-SYMBOL-EVIDENCE",
        "src/core/workspace.rs",
        "let symbol_evidence = evidence_by_symbol",
        "A symbol absent from the evidence map truly has no recorded evidence; the map was built from the same scan.",
    ),
    // bd-apvhh burn-down, tranche 5 (2026-09-18): the findings in files that
    // ALREADY CARRY RULES. Everything before this point was a file with no rule
    // at all; from here the dominant effect is classify_finding's
    // first-match-wins, which is the condition that produced bd-epvc1's six
    // shadowed entries.
    //
    // APPENDED AT THE END OF THE LIST, DELIBERATELY, AND THAT IS THE WHOLE
    // TRANCHE-5 DISCIPLINE. Earlier tranches inserted near the front, which was
    // harmless while every target file had no other rule. Here it is not: a new
    // rule placed ahead of an existing one can STEAL findings the existing rule
    // owns, silently lowering its count and re-aiming a rule nobody reviewed.
    // At the end, a new rule can only pick up findings that no earlier rule
    // matches -- which is exactly the set that is currently unclassified. The
    // ordering makes the safety structural instead of something to re-verify by
    // hand on every addition.
    //
    // src/core/verify_ledger.rs already carried two FILE-SCOPED rules
    // (NSF-VERIFY-LEDGER-CODES-ABSENT, NSF-VERIFY-LEDGER-CODES-PARSE), so it is
    // a live instance of that hazard rather than a hypothetical one.
    allowed_in(
        "NSF-VERIFY-LEDGER-RECURRENCE-CODES",
        "src/core/verify_ledger.rs",
        "classify_rch_verify_recurrence",
        "let blocker_string = first_line_with_any_code(row.stderr_tail.as_deref(), &error_codes)",
        "A ledger row whose error-code column is absent or does not parse yields no codes, and the only consumer is first_line_with_any_code over the stdout/stderr tails -- with no codes nothing matches, so the blocker string stays None. The direction is toward claiming LESS about a row than the evidence supports, which is the correct bias for a recurrence classifier that exists to avoid inventing blockers.",
    ),
    allowed_in(
        "NSF-VERIFY-LEDGER-CLOSED-REMEDIATION-REFS",
        "src/core/verify_ledger.rs",
        "classify_rch_verify_recurrence",
        ".filter(|bead| closed_remediation_beads.iter().any(|closed| closed == bead))",
        "The empty vector means the row's remediation bead is not in the closed set, which the `.filter` on the line this fragment names has just decided. It feeds `recurs_closed_remediation = row.status == \"blocked\" && !closed_remediation_refs.is_empty()`, so an empty list declines to claim a recurrence-after-close. Declining to raise a finding you cannot substantiate is the honest direction here, and the opposite of it -- defaulting to a bead id -- would fabricate the very evidence this report exists to present.",
    ),
    allowed_in(
        "NSF-VERIFY-LEDGER-ERROR-CODES-FROM",
        "src/core/verify_ledger.rs",
        "error_codes_from",
        "codes.sort();",
        "The shared helper behind the recurrence classifier: a row with no error-code payload produces no codes. Sorting and deduping an empty vector is still an empty vector, and every caller treats \"no codes\" as \"nothing to match against\" rather than as a wildcard.",
    ),
    allowed_in(
        "NSF-VERIFY-LEDGER-COMBINED-TAIL",
        "src/core/verify_ledger.rs",
        "combined_tail",
        "stdout_tail.unwrap_or_default(),",
        "Two sites, multiplicity 2. An absent stdout or stderr tail contributes an empty string to a two-element join, which renders as a leading or trailing newline rather than as fabricated output. The function's whole job is to concatenate whatever tails exist, and a missing tail genuinely contributes nothing.",
    ),
    // src/cli/mod.rs, the densest file in the burn-down: 30 unclassified
    // findings against 37 RESIDENT rules, every one of them FILE-SCOPED, some
    // with fragments as generic as `strings.sort()`, `parse_filters` and
    // `if !path.exists()`. Appended at the end for the reason given above, and
    // here the reason is load-bearing rather than tidy: an insertion ahead of
    // those residents would contest their findings on a 98,000-line file where
    // a generic fragment can reach almost anywhere.
    allowed_in(
        "NSF-CLI-PACK-LEDGER-ITEM-PROJECTION",
        "src/cli/mod.rs",
        "context_delta_item_snapshot_from_pack_ledger",
        ".cloned()",
        "Ten optional projection fields of a verified prior-pack ledger item, carried as one group with multiplicity 10. Each is `Option<&serde_json::Value>` cloned into a default, and serde_json::Value's Default is Value::Null -- so absence is PRESERVED as null rather than replaced by a fabricated zero, rank, or empty string. That type-determined distinction is the whole justification, and it is the same one already accepted for the sibling projection in src/cli/context_delta_evidence.rs from_ledger. The REQUIRED field does not take this path: memoryId uses ok_or_else and returns \"verified prior pack item omitted memoryId\" when absent, so a malformed item is an error rather than a snapshot full of nulls. FRAGMENT IS DELIBERATELY GENERIC -- `.cloned()` reaches all ten context windows across a 35-line span, which no single statement fragment can -- and it is safe ONLY because the rule is function-scoped; as a file-scoped rule in this file it would be catastrophic.",
    ),
    allowed_in(
        "NSF-CLI-HELP-ARG-OPTIONAL-TEXT",
        "src/cli/mod.rs",
        "cli_help_argument",
        ".or_else(|| arg.get_help())",
        "An argument that declares neither long help nor short help has no description, and the introspection surface reports an empty string. get_long_help/get_help are pure accessors over an already-parsed command tree; there is no fallible read here to swallow. SPLIT FROM THE ALIAS RULE AFTER MEASURING: one rule covering all three sites owned only 2, because the fragment I first chose sat at L675, outside the L667 context window. They are also two different concepts, so two rules is the better shape as well as the working one.",
    ),
    allowed_in(
        "NSF-CLI-HELP-ARG-ALIASES",
        "src/cli/mod.rs",
        "cli_help_argument",
        "short_aliases: arg",
        "Two sites, multiplicity 2: long aliases and short aliases. An argument that declares no aliases has none, and clap's accessors return None for exactly that case rather than for a failure.",
    ),
    allowed_in(
        "NSF-CLI-HELP-COMMAND-ABOUT",
        "src/cli/mod.rs",
        "collect_cli_help",
        "description: command",
        "A command that declares no `about` has no description, and the help projection says so with an empty string rather than inventing one. Same pure-accessor argument as the argument-level rule.",
    ),
    allowed_in(
        "NSF-CLI-HOOK-HARNESS-MATCHER-SUFFIX",
        "src/cli/mod.rs",
        "render_hook_harness_human",
        ".map(|matcher| format!(\" ({matcher})\"))",
        "The default is an empty SUFFIX for a snippet with no matcher, not an erased value. The `.map` builds a parenthesised fragment only when a matcher exists, so absence renders as nothing appended.",
    ),
    allowed_in(
        "NSF-CLI-SPLIT-TAGS-ABSENT",
        "src/cli/mod.rs",
        "split_tags",
        ".filter(|tag| !tag.is_empty())",
        "Splitting an absent tag string yields no tags. The inner chain already drops empty entries, so an absent input and an input of separators produce the same empty vector by construction rather than by this default.",
    ),
    allowed_in(
        "NSF-CLI-HOTSET-BEADS-SIGNAL-LINES",
        "src/cli/mod.rs",
        "hotset_stream_beads_signals",
        ".take(6)",
        "A bounded projection of at most six signal lines; when the source produced none there are none to take. The bound itself is the interesting part and it is preserved -- an absent source cannot widen the slice.",
    ),
    allowed_in(
        "NSF-CLI-HOTSET-GIT-STDOUT",
        "src/cli/mod.rs",
        "collect_hotset_git_source",
        "for line in stdout.lines().take(HOTSET_GIT_MAX_LINES)",
        "Reached only inside `match evidence.status { SourceRunStatus::Passed => .. }`, so this is a probe that SUCCEEDED and produced no stdout -- which means no dirty paths, the honest reading. A failed or degraded probe takes a different arm entirely and never reaches this default.",
    ),
    allowed_in(
        "NSF-CLI-HOTSET-BV-STDOUT",
        "src/cli/mod.rs",
        "collect_hotset_bv_source",
        "let stdout = evidence.output.stdout_tail.clone().unwrap_or_default();",
        "Same shape and the same guard as the git source: only a PASSED probe reaches it, so an empty tail is a successful probe with nothing to say rather than a swallowed failure.",
    ),
    allowed_in(
        "NSF-CLI-CONTEXT-SHOW-LEDGER-ARRAYS",
        "src/cli/mod.rs",
        "handle_context_show",
        "let items_json = crate::db::pack_ledger_core_array(&public_ledger, \"selectedItems\")",
        "A pack ledger that carries no selectedItems/omittedItems array describes a pack that selected or omitted nothing, and the renderer shows that rather than failing. WORTH KNOWING FOR bd-epvc1: this file already carries NSF-CLI-PACK-REPLAY-SELECTED-ITEMS and NSF-CLI-PACK-REPLAY-OMITTED-ITEMS for the SAME concept, and neither reaches here because their fragments spell the argument `ledger_core_array(value, \"selectedItems\")` while this call site passes `&public_ledger`. A fragment that hardcodes a caller's variable name is one rename away from owning nothing.",
    ),
    allowed_in(
        "NSF-CLI-ORIENT-FAST-JOINED-STRINGS",
        "src/cli/mod.rs",
        "render_orient_fast_content_human",
        ".filter_map(serde_json::Value::as_str)",
        "A JSON field that is absent, or present but not an array of strings, contributes no members to a comma join, and an empty join is an empty string. This is a human renderer over an already-retrieved payload; nothing here is a fallible read.",
    ),
    allowed_in(
        "NSF-CLI-ORIENT-FAST-PROVENANCE-URIS",
        "src/cli/mod.rs",
        "render_orient_fast_content_human",
        ".filter_map(|entry| entry.get(\"uri\").and_then(serde_json::Value::as_str))",
        "The provenance join in the same renderer, and a SEPARATE rule rather than a second site of the one above because the two chains differ: this one filter_maps over entries reaching for a `uri` field, the other over plain string values. Grouping them under the first rule's fragment left this site unowned, which the ownership delta caught. An item with no provenance entries, or entries without uris, contributes nothing to the join.",
    ),
    allowed_in(
        "NSF-CLI-PUBLIC-DEGRADATION-VALUES",
        "src/cli/mod.rs",
        "public_degradation_values",
        "let mut projected = serde_json::Value::Array(values.to_vec());",
        "UNREACHABLE. `projected` is constructed as Value::Array two lines above, and redact_public_projection_strings redacts in place without changing the variant, so `as_array()` is always Some by construction.",
    ),
    allowed_in(
        "NSF-CLI-RECALL-FALLBACK-MESSAGE",
        "src/cli/mod.rs",
        "handle_recall",
        "let entry = daemon_memory_read_fallback(&reason);",
        "`entry` is built by daemon_memory_read_fallback on the line this fragment names, so its \"message\" field is always present and the default cannot be observed. Note the asymmetry one line below -- `repair` keeps its Option through `.map(str::to_owned)` -- which is the clearer spelling of the same absence.",
    ),
    allowed_in(
        "NSF-CLI-USAGE-ERROR-NO-REPAIR",
        "src/cli/mod.rs",
        "usage_parts",
        "DomainError::Usage { message, repair } =>",
        "A usage error that carries no repair hint has none, and the empty string is that. The surrounding function panics on any other DomainError variant, so it is a narrow extractor over a matched shape rather than a general error path.",
    ),
    allowed_in(
        "NSF-CLI-CLAIM-GATE-DEGRADED-REPAIR-JSON",
        "src/cli/mod.rs",
        "swarm_work_packet_claim_gate_degraded_json",
        "degradation.repair.clone().unwrap_or_default(),",
        "A degradation with no repair command aggregates as an empty repair string. The repair field is advisory guidance for an operator; its absence is not a failure to read anything. This is the third spelling of this same concept in this file, after NSF-CLI-DEGRADED-REPAIR-TEXT and NSF-CLI-DEGRADED-LIST-REPAIR-CLONE.",
    ),
    allowed_in(
        "NSF-CLI-CLAIM-GATE-ONLY-DEGRADED-REPAIR",
        "src/cli/mod.rs",
        "claim_gate_only_degraded_input",
        "DegradationAggregationInput::new(source, code, severity, message, repair.unwrap_or_default())",
        "The fourth spelling. Same justification: an absent repair hint is absent, and every branch above this line supplies its own message and code explicitly, so nothing about the degradation itself is being defaulted.",
    ),
    allowed_in(
        "NSF-CLI-EVAL-ASK-NO-SIDES",
        "src/cli/mod.rs",
        "run_eval_ask_queries",
        "let sides = report.sides.as_deref().unwrap_or_default();",
        "An ask report with no recorded sides has none, and the eval actual records an empty slice. The report itself is already in hand here -- the ask has run and returned -- so this is reading an optional field of a successful result, not recovering from a failed one.",
    ),
];

const MANUAL_FINDINGS: &[ManualFinding] = &[];

const REQUIRED_SURFACE_FILES: &[&str] = &[
    "src/cass/process.rs",
    "src/db/mod.rs",
    "src/output/mod.rs",
    "src/hooks/installer.rs",
    "src/models/jsonl.rs",
];

const fn must_fix(
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    follow_up: &'static str,
    reason: &'static str,
) -> InventoryRule {
    InventoryRule {
        id,
        file,
        function: None,
        fragment,
        disposition: Disposition::MustFix,
        follow_up: Some(follow_up),
        reason,
    }
}

/// `must_fix`, scoped to one enclosing function (bd-apvhh tranche 3).
///
/// The same argument as `allowed_in`, and it matters MORE here. A file-scoped
/// must_fix in a 7000-line file either over-reaches (claiming sites nobody
/// judged) or, if the fragment is narrowed to compensate, drifts out of its
/// finding's context window and owns nothing at all — and a must_fix owning
/// nothing is worse than no rule, because it names a follow-up bead and so
/// looks tracked while watching nothing. bd-epvc1 counts eight of those.
///
/// My first attempt at NSF-MESH-TEAM-JOIN-SYNC-OWN-ORIGIN-SWALLOWED was exactly
/// that failure: `.ok()` was far too broad for the file, and the binding name I
/// replaced it with sat nine lines above the finding, so the rule owned zero.
const fn must_fix_in(
    id: &'static str,
    file: &'static str,
    function: &'static str,
    fragment: &'static str,
    follow_up: &'static str,
    reason: &'static str,
) -> InventoryRule {
    InventoryRule {
        id,
        file,
        function: Some(function),
        fragment,
        disposition: Disposition::MustFix,
        follow_up: Some(follow_up),
        reason,
    }
}

/// `allowed`, scoped to one enclosing function (bd-apvhh).
///
/// Prefer this over `allowed` for new entries. A file-scoped rule classifies
/// every matching finding anywhere in the file; a function-scoped one cannot
/// reach outside the function it names, so its blast radius is bounded by
/// something a reviewer can see. The match-count ledger then carries
/// multiplicity WITHIN that function and fails in both directions.
const fn allowed_in(
    id: &'static str,
    file: &'static str,
    function: &'static str,
    fragment: &'static str,
    reason: &'static str,
) -> InventoryRule {
    InventoryRule {
        id,
        file,
        function: Some(function),
        fragment,
        disposition: Disposition::Allowed,
        follow_up: None,
        reason,
    }
}

const fn allowed(
    id: &'static str,
    file: &'static str,
    fragment: &'static str,
    reason: &'static str,
) -> InventoryRule {
    InventoryRule {
        id,
        file,
        function: None,
        fragment,
        disposition: Disposition::Allowed,
        follow_up: None,
        reason,
    }
}

const UNCLASSIFIED_BASELINE_FIXTURE: &str =
    "tests/fixtures/contracts/no_silent_fallback_unclassified_baseline.txt";

const RULE_MATCH_COUNTS_FIXTURE: &str =
    "tests/fixtures/contracts/no_silent_fallback_rule_match_counts.txt";

/// Declared blast radius per rule: how many findings each one OWNS.
///
/// `classify_finding` takes the FIRST rule whose fragment appears in a
/// finding's +/-4 context window, so a rule's reach is invisible from its text.
/// A fragment owning 3 sites today and 30 next month has silently become a
/// different rule (bd-apvhh).
fn rule_match_counts() -> Result<BTreeMap<String, usize>, String> {
    let raw = include_str!("../fixtures/contracts/no_silent_fallback_rule_match_counts.txt");
    let mut declared = BTreeMap::new();

    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let (count, id) = trimmed.split_once('\t').ok_or_else(|| {
            format!(
                "{RULE_MATCH_COUNTS_FIXTURE}:{}: expected `<count>\\t<rule id>`",
                index + 1
            )
        })?;
        let count = count.trim().parse::<usize>().map_err(|error| {
            format!(
                "{RULE_MATCH_COUNTS_FIXTURE}:{}: bad count `{count}`: {error}",
                index + 1
            )
        })?;
        if declared.insert(id.trim().to_owned(), count).is_some() {
            return Err(format!(
                "{RULE_MATCH_COUNTS_FIXTURE}:{}: duplicate rule id `{}`",
                index + 1,
                id.trim()
            ));
        }
    }

    Ok(declared)
}

/// Which findings each rule actually owns, under `classify_finding`'s
/// first-match-wins semantics.
fn owned_counts(findings: &[SourceFinding]) -> BTreeMap<String, usize> {
    let mut owned: BTreeMap<String, usize> = BTreeMap::new();
    for finding in findings {
        if let Some(rule) = classify_finding(finding) {
            *owned.entry(rule.id.to_owned()).or_insert(0) += 1;
        }
    }
    owned
}

fn unclassified_baseline() -> Result<BTreeMap<String, usize>, String> {
    let raw = include_str!("../fixtures/contracts/no_silent_fallback_unclassified_baseline.txt");
    let mut baseline = BTreeMap::new();

    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let (count, file) = trimmed.split_once('\t').ok_or_else(|| {
            format!(
                "{UNCLASSIFIED_BASELINE_FIXTURE}:{}: expected `<count>\\t<path>`",
                index + 1
            )
        })?;
        let count = count.trim().parse::<usize>().map_err(|error| {
            format!(
                "{UNCLASSIFIED_BASELINE_FIXTURE}:{}: bad count `{count}`: {error}",
                index + 1
            )
        })?;
        if baseline.insert(file.trim().to_owned(), count).is_some() {
            return Err(format!(
                "{UNCLASSIFIED_BASELINE_FIXTURE}:{}: duplicate path `{}`",
                index + 1,
                file.trim()
            ));
        }
    }

    Ok(baseline)
}

fn unclassified_by_file(findings: &[SourceFinding]) -> BTreeMap<String, usize> {
    let mut observed: BTreeMap<String, usize> = BTreeMap::new();
    for finding in findings {
        if classify_finding(finding).is_none() {
            *observed.entry(finding.file.clone()).or_default() += 1;
        }
    }
    observed
}

/// Shrink-only ratchet over unclassified silent-fallback findings.
///
/// This test used to assert that every high-risk line under `src/` carried an
/// inventory entry. That assertion had decayed past usefulness: 289 of the 328
/// rules were `Allowed` and 184 findings were uncovered, so the gate was
/// permanently red and its only landable repair was appending more allowlist
/// rows. An inventory that is 88% allowlist asserts that somebody looked, not
/// that nothing is wrong.
///
/// What it asserts now: uncovered findings may only ever DECREASE, per file.
/// Introducing a new silent default fails, because that file's count rose.
/// Fixing one also fails until the baseline row is lowered to match — without
/// that second direction a stale row decays into exactly the permanent
/// ignore-list this replaced.
///
/// Whole-tree coverage is deliberately retained. Scoping the check to the
/// serialization surfaces in `REQUIRED_SURFACE_FILES` would have gone green
/// immediately (those files hold 7 of the 184) by abandoning `src/core`,
/// `src/cli` and `src/mesh`, where the other 177 live.
/// A file compiled ONLY for tests must not be scanned as product code.
///
/// `ignored_test_module_lines` sees `#[cfg(test)] mod tests { … }` inside one
/// file. It cannot see a whole file gated from elsewhere, so
/// `src/core/ask_privacy_tests.rs` — included by `src/core/ask_corpus.rs:263`
/// behind `#[cfg(test)]` — was scanned as product, and an `assert!` in a test
/// at its :127 counted as a silent-fallback finding.
///
/// Paired deliberately. The positive arm alone would pass against a detector
/// that skipped every file; the negative arm is what proves real product code
/// is still scanned after the exclusion.
/// GUARD 1 (bd-apvhh, ruled 2026-09-17): the allowlist ratio must be PRINTED,
/// not merely true.
///
/// 289 of 313 rules are `allowed` — 92%. A gate whose inventory is mostly
/// "this is fine" is most of the way to not being a gate, and that number
/// should have raised an alarm long before 179 findings accumulated
/// unclassified. It could not, because nothing ever stated it.
///
/// This asserts the ratio is reported on every run and that it stays inside a
/// declared ceiling, so the next person to widen the allowlist sees the number
/// move.
#[test]
fn allowlist_ratio_is_reported_and_ratcheted() -> TestResult {
    let allowed = INVENTORY_RULES
        .iter()
        .filter(|rule| rule.disposition == Disposition::Allowed)
        .count();
    let must_fix = INVENTORY_RULES
        .iter()
        .filter(|rule| rule.disposition == Disposition::MustFix)
        .count();
    let total = allowed + must_fix;
    let mut problems = Vec::new();

    if total == 0 {
        problems.push("inventory is empty; this check would be vacuous".to_owned());
    }

    // Printed on every run, so the ratio is visible without reading the source.
    let percent = if total == 0 { 0 } else { allowed * 100 / total };
    println!(
        "no_silent_fallback inventory: {allowed} allowed, {must_fix} must_fix ({percent}% allowlist)"
    );

    // The allowlist may not GROW without a stated reason per entry.
    //
    // A percentage ceiling was the obvious shape and is a weak one: integer
    // percent barely moves. 289 of 313 is 92%, and so is 300 of 324 — eleven
    // more exemptions would not register. The per-entry requirement is what
    // actually costs something to add, because it makes each exemption carry an
    // argument a reviewer can disagree with.
    const MIN_REASON_CHARS: usize = 24;
    for rule in INVENTORY_RULES {
        if rule.disposition != Disposition::Allowed {
            continue;
        }
        let reason = rule.reason.trim();
        if reason.len() < MIN_REASON_CHARS {
            problems.push(format!(
                "{} is `allowed` with a {}-character reason ({reason:?}). An exemption \
                 needs an argument, not a label; say why the fallback is safe at that \
                 site.",
                rule.id,
                reason.len()
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// GUARD 2 (bd-apvhh, ruled 2026-09-17): every rule declares how many findings
/// it OWNS, and the gate fails in both directions.
///
/// A rule's blast radius is invisible from its text because `classify_finding`
/// matches a +/-4 context window and takes the first hit. Measured when this
/// landed: 9 rules own 5 or more sites, the widest owning 15, and 33 rules own
/// NOTHING because an earlier rule wins every finding they would match.
///
/// Growth means a rule silently absorbed sites nobody reviewed. Shrink means
/// the declaration is stale. Both fail, because one arm alone lets the ledger
/// decay into decoration.
#[test]
fn every_rule_declares_the_number_of_findings_it_owns() -> TestResult {
    let findings = scan_source_findings()?;
    let owned = owned_counts(&findings);
    let declared = rule_match_counts()?;
    let mut problems = Vec::new();

    for rule in INVENTORY_RULES {
        let live = owned.get(rule.id).copied().unwrap_or(0);
        let Some(expected) = declared.get(rule.id).copied() else {
            problems.push(format!(
                "{} owns {live} finding(s) but declares no count in \
                 {RULE_MATCH_COUNTS_FIXTURE}",
                rule.id
            ));
            continue;
        };
        if live > expected {
            problems.push(format!(
                "{} now owns {live} finding(s), declared {expected}. It absorbed \
                 {} site(s) nobody reviewed — widen the reason or split the rule.",
                rule.id,
                live - expected
            ));
        } else if live < expected {
            problems.push(format!(
                "{} owns {live} finding(s) but declares {expected}. Lower the row in \
                 {RULE_MATCH_COUNTS_FIXTURE}; a stale count overstates this rule's reach.",
                rule.id
            ));
        }
    }

    for id in declared.keys() {
        if !INVENTORY_RULES.iter().any(|rule| rule.id == id) {
            problems.push(format!(
                "{RULE_MATCH_COUNTS_FIXTURE} declares `{id}`, which is no longer an \
                 inventory rule; drop the row"
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

#[test]
fn cfg_test_only_files_are_not_scanned_as_product_code() -> TestResult {
    let gated = cfg_test_only_files()?;
    let findings = scan_source_findings()?;
    let mut problems = Vec::new();

    if !gated.contains("src/core/ask_privacy_tests.rs") {
        problems.push(
            "src/core/ask_privacy_tests.rs is included behind #[cfg(test)] from \
             src/core/ask_corpus.rs and must be recognised as test-only"
                .to_owned(),
        );
    }
    if gated.len() < 5 {
        problems.push(format!(
            "only {} cfg(test)-only files detected; a near-empty set would make \
             this check vacuous while the blind spot stayed open",
            gated.len()
        ));
    }

    // The negative arm. Both of these are included with NO cfg(test) --
    // `mod context_delta_evidence;` (src/cli/mod.rs) and
    // `#[path = "ask_candidates.rs"] mod selection;` (src/core/ask.rs) -- and
    // both really do carry findings, so excluding them would silently shrink
    // the gate's subject rather than sharpen it.
    for product in [
        "src/cli/context_delta_evidence.rs",
        "src/core/ask_candidates.rs",
        "src/cli/mod.rs",
    ] {
        if gated.contains(product) {
            problems.push(format!(
                "{product} is product code and must still be scanned"
            ));
        }
    }

    if findings
        .iter()
        .any(|finding| finding.file == "src/core/ask_privacy_tests.rs")
    {
        problems.push(
            "a finding was reported from a cfg(test)-only file; an assert! inside \
             a test is not a silent fallback"
                .to_owned(),
        );
    }
    if findings.len() < 400 {
        problems.push(format!(
            "only {} findings scanned; the exclusion must remove test-only files, \
             not most of the tree",
            findings.len()
        ));
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

#[test]
fn no_silent_fallback_unclassified_findings_only_shrink() -> TestResult {
    let findings = scan_source_findings()?;
    let observed = unclassified_by_file(&findings);
    let baseline = unclassified_baseline()?;
    let mut problems = Vec::new();

    for (file, count) in &observed {
        let allowed = baseline.get(file).copied().unwrap_or(0);
        if *count > allowed {
            problems.push(format!(
                "{file}: {count} unclassified fallback(s), baseline allows {allowed}. \
                 Return a contextual error or degradation, or add a justified \
                 inventory entry with a follow-up bead."
            ));
        }
    }

    for (file, allowed) in &baseline {
        let count = observed.get(file).copied().unwrap_or(0);
        if count < *allowed {
            problems.push(format!(
                "{file}: {count} unclassified fallback(s) but the baseline still \
                 allows {allowed}. Lower that row to {count} in \
                 {UNCLASSIFIED_BASELINE_FIXTURE} (drop the row entirely at 0); a \
                 stale baseline row is a permanent allowlist."
            ));
        }
    }

    if problems.is_empty() {
        return Ok(());
    }

    problems.sort();

    // A per-file list alone cannot distinguish "one file's debt moved" from
    // "this tree is not the tree the baseline was taken on". The totals do:
    // if the scanned high-risk count differs from what the baseline was built
    // against, the disagreement is about which sources were seen, not about
    // classification, and chasing individual rows wastes the run.
    let observed_total: usize = observed.values().sum();
    let baseline_total: usize = baseline.values().sum();
    let summary = format!(
        "no_silent_fallback ratchet: scanned {} high-risk line(s); {} unclassified across {} file(s); \
         baseline allows {} across {} file(s).",
        findings.len(),
        observed_total,
        observed.len(),
        baseline_total,
        baseline.len(),
    );

    Err(format!("{summary}\n{}", problems.join("\n")))
}

#[test]
fn no_silent_fallback_must_fix_entries_have_follow_up_beads() -> TestResult {
    let mut missing = Vec::new();

    for rule in INVENTORY_RULES {
        if rule.disposition == Disposition::MustFix {
            match rule.follow_up {
                Some(bead) if FOLLOW_UP_BEADS.contains(&bead) => {}
                Some(bead) => missing.push(format!(
                    "{} references unknown follow-up `{bead}`: {}",
                    rule.id, rule.reason
                )),
                None => missing.push(format!(
                    "{} has no follow-up bead: {}",
                    rule.id, rule.reason
                )),
            }
        }
    }

    for finding in MANUAL_FINDINGS {
        if !FOLLOW_UP_BEADS.contains(&finding.follow_up) {
            missing.push(format!(
                "{} references unknown follow-up `{}`: {}",
                finding.id, finding.follow_up, finding.reason
            ));
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}

/// A `must_fix` row asserts an outstanding defect. When its site is repaired
/// and the row stays behind, the inventory reports debt that no longer exists.
/// Fourteen of thirty-nine rows did exactly that before this check existed,
/// overstating real must-fix debt by better than a third, and nothing noticed:
/// `..._have_follow_up_beads` verifies a row cites a known bead, not that it
/// still describes live code.
///
/// Only `MustFix` rows are checked. A stale `Allowed` row matches nothing and
/// so grants no exemption to anything; a stale `MustFix` row makes the gate
/// lie about how much is left to do.
///
/// Matching mirrors `classify_finding`, which tests the fragment against a
/// window of TRIMMED source lines. The haystack here is therefore the file's
/// trimmed lines joined, not its raw text: several fragments span lines, and
/// against raw text those would report a false absence.
#[test]
fn no_silent_fallback_must_fix_entries_still_describe_real_code() -> TestResult {
    let mut stale = Vec::new();

    for rule in INVENTORY_RULES {
        if rule.disposition != Disposition::MustFix {
            continue;
        }

        let path = repo_path(rule.file);
        let Ok(source) = fs::read_to_string(&path) else {
            stale.push(format!("{}: file {} no longer exists", rule.id, rule.file));
            continue;
        };

        let trimmed = source.lines().map(str::trim).collect::<Vec<_>>().join("\n");
        if !trimmed.contains(rule.fragment) {
            stale.push(format!(
                "{}: `{}` no longer appears in {}. If the defect is fixed, drop this row; \
                 if it moved, point the fragment at where it went.",
                rule.id, rule.fragment, rule.file
            ));
        }
    }

    if stale.is_empty() {
        Ok(())
    } else {
        Err(stale.join("\n"))
    }
}

#[test]
fn no_silent_fallback_inventory_covers_required_surfaces() -> TestResult {
    let mut missing = Vec::new();
    for required in REQUIRED_SURFACE_FILES {
        if !INVENTORY_RULES.iter().any(|rule| rule.file == *required) {
            missing.push(*required);
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "required no-silent-fallback inventory surface(s) missing: {}",
            missing.join(", ")
        ))
    }
}

#[test]
fn no_silent_fallback_manual_findings_still_point_at_real_code() -> TestResult {
    let mut missing = Vec::new();
    for finding in MANUAL_FINDINGS {
        let path = repo_path(finding.file);
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if !source.contains(finding.fragment) {
            missing.push(format!(
                "{} missing `{}` in {}",
                finding.id, finding.fragment, finding.file
            ));
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}

#[test]
fn no_silent_fallback_guard_rejects_new_unclassified_renderer_default() -> TestResult {
    let synthetic = SourceFinding {
        file: "src/output/new_renderer.rs".to_owned(),
        line: 1,
        text: "serde_json::to_string(report).unwrap_or_default()".to_owned(),
        context: "serde_json::to_string(report).unwrap_or_default()".to_owned(),
        // Module scope: this synthetic finding must not accidentally satisfy a
        // function-scoped rule, and None cannot match any `Some(fn)` scope.
        function: None,
    };

    if classify_finding(&synthetic).is_none() {
        Ok(())
    } else {
        Err("synthetic unclassified renderer fallback was unexpectedly allowlisted".to_owned())
    }
}

#[test]
fn no_silent_fallback_guard_rejects_new_unclassified_empty_vec() -> TestResult {
    let synthetic = SourceFinding {
        file: "src/db/new_repository.rs".to_owned(),
        line: 42,
        text: "return Ok(Vec::new());".to_owned(),
        context: "return Ok(Vec::new());".to_owned(),
        function: None,
    };

    if classify_finding(&synthetic).is_none() {
        Ok(())
    } else {
        Err("synthetic unclassified empty-vector fallback was unexpectedly allowlisted".to_owned())
    }
}

fn classify_finding(finding: &SourceFinding) -> Option<&'static InventoryRule> {
    INVENTORY_RULES.iter().find(|rule| {
        rule.file == finding.file && rule.scopes(finding) && finding.context.contains(rule.fragment)
    })
}

impl InventoryRule {
    /// Whether this rule's function scope admits `finding`.
    ///
    /// `None` means file scope, which is how every rule written before
    /// bd-apvhh behaves and remains valid. A rule that DOES name a function
    /// only classifies findings inside it, so a group key cannot silently
    /// absorb an occurrence elsewhere in the file — which is the
    /// over-exemption that per-site keying exists to prevent.
    fn scopes(&self, finding: &SourceFinding) -> bool {
        match self.function {
            None => true,
            Some(expected) => finding.function.as_deref() == Some(expected),
        }
    }
}

/// Files under `src/` that are compiled ONLY for tests, because some other file
/// includes them behind `#[cfg(test)]`.
///
/// `ignored_test_module_lines` recognises `#[cfg(test)]` followed by
/// `mod tests {` WITHIN one file. It cannot see this shape, which gates a whole
/// file from somewhere else:
///
/// ```ignore
/// // src/core/ask_corpus.rs
/// #[cfg(test)]
/// #[path = "ask_privacy_tests.rs"]
/// mod privacy_tests;
/// ```
///
/// The included file therefore got scanned as product code, and an `assert!`
/// inside a test counted as a silent-fallback finding
/// (`src/core/ask_privacy_tests.rs:127`). Thirteen files under `src/` are
/// included this way; measured when this was written, ZERO of the baselined
/// findings sat in any of them, so this closes a latent hole rather than
/// rewriting live debt — which is the cheap moment to close it, before a
/// spurious entry gets baselined and has to be unpicked.
fn cfg_test_only_files() -> Result<BTreeSet<String>, String> {
    let mut files = Vec::new();
    collect_rust_files(&repo_path("src"), &mut files)?;
    let mut gated = BTreeSet::new();

    for path in &files {
        let source = fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let lines = source.lines().collect::<Vec<_>>();
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?;

        for (index, line) in lines.iter().enumerate() {
            if line.trim() != "#[cfg(test)]" {
                continue;
            }
            // `#[path = "X.rs"] mod y;` — the include names the file directly.
            // `mod y;` — the sibling `y.rs` is the file.
            let mut included: Option<String> = None;
            for follow in lines.iter().skip(index + 1).take(3) {
                let trimmed = follow.trim();
                if let Some(rest) = trimmed.strip_prefix("#[path = \"") {
                    if let Some(name) = rest.split('"').next() {
                        included = Some(name.to_owned());
                    }
                } else if let Some(rest) = trimmed.strip_prefix("mod ") {
                    if let Some(name) = rest.strip_suffix(';') {
                        if included.is_none() {
                            included = Some(format!("{name}.rs"));
                        }
                    }
                    break;
                } else if !trimmed.starts_with('#') {
                    break;
                }
            }

            if let Some(name) = included {
                let candidate = parent.join(name);
                if candidate.is_file() {
                    gated.insert(relative_path(&candidate)?);
                }
            }
        }
    }

    Ok(gated)
}

/// Enclosing function per source line, resolved by brace depth.
///
/// A function stays PENDING until its body brace opens. Without that, a
/// multi-line signature — `pub fn parse_view_json_summary(` with its `{`
/// several lines down — is popped before its body starts, because the depth has
/// not risen yet when the pop condition is tested. Measured while building this:
/// the naive version left 339 of 597 findings with no resolvable function, 57%,
/// and this codebase uses long parameter lists heavily. With the pending state,
/// 0 of 597 are unresolved.
fn enclosing_functions(lines: &[&str]) -> Vec<Option<String>> {
    let mut out = vec![None; lines.len()];
    let mut stack: Vec<(i32, String, bool)> = Vec::new();
    let mut depth = 0_i32;

    for (index, line) in lines.iter().enumerate() {
        if let Some(name) = function_name(line) {
            stack.push((depth, name, false));
        }
        out[index] = stack.last().map(|(_, name, _)| name.clone());

        depth += brace_delta(line);
        if let Some(top) = stack.last_mut() {
            if !top.2 && depth > top.0 {
                top.2 = true;
            }
        }
        while stack
            .last()
            .is_some_and(|(at, _, opened)| *opened && depth <= *at)
        {
            stack.pop();
            out[index] = stack.last().map(|(_, name, _)| name.clone());
        }
    }

    out
}

/// The function name declared on this line, if any.
fn function_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let mut rest = trimmed;
    for prefix in [
        "pub(crate) ",
        "pub(super) ",
        "pub ",
        "const ",
        "async ",
        "unsafe ",
    ] {
        while let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
        }
    }
    let rest = rest.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

fn scan_source_findings() -> Result<Vec<SourceFinding>, String> {
    let mut files = Vec::new();
    collect_rust_files(&repo_path("src"), &mut files)?;
    files.sort();
    let test_only = cfg_test_only_files()?;

    let mut findings = Vec::new();
    for path in files {
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let relative = relative_path(&path)?;
        // Test-only by inclusion, not by an in-file module. See
        // `cfg_test_only_files`.
        if test_only.contains(&relative) {
            continue;
        }
        let ignored = ignored_test_module_lines(&source);
        let lines = source.lines().collect::<Vec<_>>();
        let functions = enclosing_functions(&lines);

        for (index, line) in lines.iter().enumerate() {
            if ignored[index] || !is_high_risk_line(line) {
                continue;
            }
            findings.push(SourceFinding {
                file: relative.clone(),
                line: index + 1,
                text: line.trim().to_owned(),
                context: context_window(&lines, index),
                function: functions.get(index).cloned().flatten(),
            });
        }
    }

    Ok(findings)
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?
    {
        let entry = entry.map_err(|error| format!("failed to read dir entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            if is_temp_sync_dir(&path) {
                continue;
            }
            collect_rust_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn is_temp_sync_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".tmp-sync"))
}

fn ignored_test_module_lines(source: &str) -> Vec<bool> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut ignored = vec![false; lines.len()];
    let mut pending_cfg_test = false;
    let mut in_test_module = false;
    let mut brace_depth = 0_i32;

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if in_test_module {
            ignored[index] = true;
            brace_depth += brace_delta(line);
            if brace_depth <= 0 {
                in_test_module = false;
            }
            continue;
        }

        if pending_cfg_test && trimmed.starts_with("mod tests") && trimmed.contains('{') {
            ignored[index] = true;
            in_test_module = true;
            brace_depth = brace_delta(line);
            pending_cfg_test = false;
            if brace_depth <= 0 {
                in_test_module = false;
            }
            continue;
        }

        if trimmed == "#[cfg(test)]" {
            pending_cfg_test = true;
        } else if pending_cfg_test
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("//")
        {
            pending_cfg_test = false;
        }
    }

    ignored
}

fn brace_delta(line: &str) -> i32 {
    line.chars().fold(0_i32, |depth, ch| match ch {
        '{' => depth + 1,
        '}' => depth - 1,
        _ => depth,
    })
}

fn is_high_risk_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains(".unwrap_or_default()")
        || trimmed.contains("Ok(Vec::new())")
        || (trimmed.starts_with("let _ =") && trimmed.contains("read_to_end"))
        || trimmed.contains("join().unwrap_or_default()")
}

fn context_window(lines: &[&str], index: usize) -> String {
    let start = index.saturating_sub(4);
    let end = (index + 5).min(lines.len());
    lines[start..end]
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n")
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn relative_path(path: &Path) -> Result<String, String> {
    let root = repo_path("");
    let relative = path
        .strip_prefix(&root)
        .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}
