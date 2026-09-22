#!/usr/bin/env python3
"""Source-bound, one-time wiring for the exact memory-time implementation."""
from pathlib import Path
import hashlib

root = Path('.')
names = ['src/db/mod.rs', 'src/core/memory.rs', 'src/core/jsonl_import.rs']
original = {name: (root / name).read_text() for name in names}
if 'mod memory_temporal;' in original[names[0]]:
    if 'SecondsFormat::AutoSi, true' not in original[names[1]]:
        raise SystemExit('Partial temporal integration; refusing automatic replay')
    raise SystemExit(0)
expected = ['4f5a9f4c13df61cc4ae6229d9be8b2cba20685b9', '3b405374d3c64818343aa17f835d889d16d8cb87', '8adaba4307bc9c079323912395e8254e4b9c767d']
for name, digest in zip(names, expected):
    data = (root / name).read_bytes()
    actual = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
    if actual != digest:
        raise SystemExit(f'Source changed; review before integrating {name}')


def replace_body(text, name, new):
    pos = text.index('    pub fn ' + name + '(')
    start = text.index('{', pos)
    depth, end = 1, start + 1
    while depth:
        char = text[end]
        depth += (char == '{') - (char == '}')
        end += 1
    assert text[end:end + 2] == '\n\n', name
    return text[:start + 1] + '\n' + new + '\n    }' + text[end:]


t = original[names[0]].replace('pub mod migrate;', 'mod memory_temporal;\npub mod migrate;', 1)
for name, body in {
    'list_memories_valid_at': '        memory_temporal::current(self, workspace_id, level, include_tombstoned, as_of)',
    'list_recent_current_memories_for_retrieval': '        memory_temporal::recent(self, workspace_id, as_of, limit)',
    'list_memories_by_tag_valid_at': '        memory_temporal::by_tag(self, workspace_id, tag, as_of)',
    'get_tag_counts_valid_at': '        memory_temporal::tag_counts(self, workspace_id, as_of)',
    'list_all_tags_valid_at': '''        let mut tags: Vec<_> = memory_temporal::tag_counts(self, workspace_id, as_of)?
            .into_iter().map(|row| row.tag).collect();
        tags.sort();
        Ok(tags)''',
    'expire_memory_valid_to': '        memory_temporal::tighten_end(self, id, valid_to, memory_temporal::EndColumn::ValidTo)',
    'mark_memory_superseded': '        memory_temporal::tighten_end(self, id, superseded_at, memory_temporal::EndColumn::SupersededAt)',
}.items():
    t = replace_body(t, name, body)
a = t.index('    /// Callers pass the same `SecondsFormat::Secs` UTC spelling')
b = t.index('    pub fn list_memories_valid_at', a)
t = t[:a] + '''    /// Endpoints are compared as exact RFC3339 instants, including fractional
    /// seconds and legacy offsets. The inclusive expiry boundary matches
    /// `validity_status_at`; malformed expiry values do not grant admission.
''' + t[b:]
a = t.index('    /// Load a deterministic, SQL-bounded window of currently admissible')
b = t.index('    pub fn list_recent_current_memories_for_retrieval', a)
t = t[:a] + '''    /// Load a deterministic bounded window of currently admissible memories.
    ///
    /// Exact instant comparisons exclude future, expired, superseded and
    /// post-`as_of` rows before final ordering and limiting. SQL's Julian day
    /// only locates coarse creation-time buckets; complete ties are examined
    /// in bounded pages within one nested read snapshot. Callers still apply
    /// scope, trust, provenance and redaction admission to the returned rows.
''' + t[b:]
t = t.replace('''    /// The guard still refuses to move an existing marker backwards, so a
    /// double-supersede is a no-op rather than a silent rewrite of history.''', '''    /// An equal or later marker is a no-op. An earlier marker tightens the
    /// end, preserving the existing monotonic contract across offset spellings.''')
updated = {names[0]: t}
t = original[names[1]]
a = t.index('/// Canonical spelling for the AUTHOR VALIDITY columns')
b = t.index('/// Canonical spelling for the ROW BOOKKEEPING', a)
t = t[:a] + '''/// Canonical UTC spelling for author validity, preserving nanosecond precision.
/// Whole-second inputs keep their existing `Z` representation. Reader boundaries
/// compare parsed instants, never this spelling or a rounded SQL Julian day.
/// Truncating here would change applicability during import and backup recovery.
pub(crate) fn normalize_validity_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

''' + t[b:]
a = t.index('    // bd-tmv70 stopgap: `valid_to` doubles as the revision-supersession marker,')
b = t.index('    // If filtering by tag', a)
t = t[:a] + '''    // Use one exact reference for tag membership and memory applicability.
    // Revision identity and author expiry are separate database fields.
    let validity_bound = normalize_validity_timestamp(Utc::now());

''' + t[b:]
t = t.replace('let expires_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);', 'let expires_at = normalize_validity_timestamp(Utc::now());', 1)
t = t.replace('let revised_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);', 'let revised_at = normalize_validity_timestamp(Utc::now());', 1)
updated[names[1]] = t
t = original[names[2]]
a = t.index('/// Re-emit an imported RFC3339 timestamp')
b = t.index('fn normalize_imported_timestamp', a)
t = t[:a] + '''/// Normalize the offset spelling without changing the represented instant.
/// Authentication has already bound the original transport bytes. Row columns
/// retain their historical offset form; validity uses UTC `Z`, with fractional
/// precision preserved in both cases. Validation belongs to the admission pass.
''' + t[b:]
t = t.replace('/// `valid_from`, `valid_to` -- `SecondsFormat::Secs` `Z` form.', '/// `valid_from`, `valid_to` -- exact UTC `Z` form.')
a = t.index('        // bd-o22r0. `cases` above are the ARCHIVE spellings')
b = t.index('        let expected:', a)
t = t[:a] + '''        // Hand-computed UTC instants, not values obtained from the production
        // normalizer. Offsets may change spelling, but the validity inherited
        // from created_at must retain all nine fractional digits.
''' + t[b:]
t = t.replace('"2020-01-01T21:34:05Z",', '"2020-01-01T21:34:05.123456789Z",', 1)
updated[names[2]] = t
# All source preconditions and transformations succeeded before any write.
for name, text in updated.items():
    (root / name).write_text(text)
