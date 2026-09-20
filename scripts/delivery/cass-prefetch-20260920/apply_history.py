#!/usr/bin/env python3
"""Apply bounded, generation-coherent prefetch residency to the current main tree."""
from pathlib import Path

path = Path('src/core/cass_prefetch.rs')
text = path.read_text()

def replace(old: str, new: str) -> None:
    global text
    if new in text:
        assert text.count(new) == 1, 'duplicate installed replacement'
        return
    assert text.count(old) == 1, f'source anchor changed: {old[:100]!r}'
    text = text.replace(old, new, 1)

replace('use std::collections::{BTreeMap, HashMap};',
        'use std::collections::{BTreeMap, HashMap, VecDeque};')
replace('pub const MAX_PREFETCH_HISTORY: usize = 64;', '''pub const MAX_PREFETCH_HISTORY: usize = 64;

/// Bound total daemon residency, not just each individual rolling window.
/// Rotating caller identities must not turn a bounded predictor into an
/// unbounded process-lifetime accumulator.
pub const MAX_PREFETCH_RESIDENT_HISTORIES: usize = 256;
/// Oversized owner/workspace/revision keys are not retained or aliased.
pub const MAX_PREFETCH_OWNER_BYTES: usize = 1024;''')
replace('''    histories: BTreeMap<(AgentScope, String), CassPrefetchHistory>,
}''', '''    histories: BTreeMap<(AgentScope, String), CassPrefetchHistory>,
    // Least-recently observed first. Logical ordering, never wall-clock time.
    observed_order: VecDeque<(AgentScope, String)>,
}''')
replace('''            histories: BTreeMap::new(),
        }
    }

    /// Record one observed''', '''            histories: BTreeMap::new(),
            observed_order: VecDeque::new(),
        }
    }

    /// Record one observed''')
replace('''        let scope = agent_scope.into();
        let entry = self
            .histories
            .entry((scope.clone(), workspace.into()))
            .or_insert_with(|| CassPrefetchHistory::new(scope, Vec::new()));
        // Most-recent-first: the newest observation goes to the front, stamped
        // with the live corpus revision so the revision gate can later reject a
        // trail measured against a since-regenerated index.
        entry.recent_first.insert(
            0,
            CassPrefetchObservation::new(topic).with_corpus_revision(corpus_revision.clone()),
        );''', '''        let scope = agent_scope.into();
        let workspace = workspace.into();
        let topic = topic.into();
        // Do not truncate identities: that could merge two agents' histories.
        // Check before redaction/allocation and again after redaction, which can
        // expand text. Invalid input must not evict a valid resident history.
        if scope.as_str().len() > MAX_PREFETCH_OWNER_BYTES
            || workspace.len() > MAX_PREFETCH_OWNER_BYTES
            || corpus_revision.as_str().len() > MAX_PREFETCH_OWNER_BYTES
            || topic.len() > MAX_PREFETCH_TOPIC_ID_BYTES
        {
            return;
        }
        let observation =
            CassPrefetchObservation::new(topic).with_corpus_revision(corpus_revision.clone());
        if observation.topic_id.as_str().len() > MAX_PREFETCH_TOPIC_ID_BYTES {
            return;
        }
        let key = (scope.clone(), workspace);
        if let Some(position) = self.observed_order.iter().position(|stored| stored == &key) {
            self.observed_order.remove(position);
        } else if self.histories.len() >= MAX_PREFETCH_RESIDENT_HISTORIES
            && let Some(evicted) = self.observed_order.pop_front()
        {
            self.histories.remove(&evicted);
        }
        self.observed_order.push_back(key.clone());
        let entry = self
            .histories
            .entry(key)
            .or_insert_with(|| CassPrefetchHistory::new(scope, Vec::new()));
        // A new observation cannot retrospectively certify older observations.
        // Reindex/workspace changes restart this scoped window; corpus changes
        // also recover immediately instead of poisoning predictions until ten
        // new requests have displaced every old-revision observation.
        if !entry.generation.is_coherent_with(generation)
            || !entry.corpus_revision_is_coherent_with(corpus_revision)
        {
            entry.recent_first.clear();
        }
        entry.recent_first.insert(0, observation);''')
marker = '''\n#[cfg(test)]
#[path = "cass_prefetch_residency_tests.rs"]
mod residency_tests;
'''
if marker not in text:
    text += marker
path.write_text(text)
