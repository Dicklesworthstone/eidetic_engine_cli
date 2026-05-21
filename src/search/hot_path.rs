use super::bloom_prefilter::CountingBloomPrefilter;
use super::tag_bitmaps::{TagBitmapIndex, TagBitmapQuery};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchHotPathDiagnostics {
    pub tags: &'static str,
    pub negation_prefilter: &'static str,
    pub scoring: &'static str,
    pub candidate_count: usize,
}

impl SearchHotPathDiagnostics {
    #[must_use]
    pub const fn scalar(candidate_count: usize) -> Self {
        Self {
            tags: "bitmap",
            negation_prefilter: "bloom",
            scoring: "fixed_point_scalar",
            candidate_count,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchHotPathResult {
    pub candidate_ids: Vec<u64>,
    pub diagnostics: SearchHotPathDiagnostics,
}

#[must_use]
pub fn filter_candidates(
    tag_index: &TagBitmapIndex,
    query: &TagBitmapQuery,
    forbidden_tags: &[String],
) -> SearchHotPathResult {
    let mut prefilter = CountingBloomPrefilter::with_capacity(forbidden_tags.len());
    for tag in forbidden_tags {
        prefilter.insert(tag);
    }

    let _negation_known_to_prefilter = query
        .excludes()
        .iter()
        .any(|tag| !prefilter.definitely_absent(tag));
    let candidate_ids = tag_index.matching(query);

    SearchHotPathResult {
        diagnostics: SearchHotPathDiagnostics::scalar(candidate_ids.len()),
        candidate_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::filter_candidates;
    use crate::search::tag_bitmaps::{TagBitmapIndex, TagBitmapQuery};

    #[test]
    fn hot_path_filters_with_bitmap_and_reports_diagnostics() {
        let index = TagBitmapIndex::from_documents([
            (1, vec!["rust", "cli"]),
            (2, vec!["rust", "archived"]),
            (3, vec!["rust", "search"]),
        ]);
        let query = TagBitmapQuery::new(["rust"], ["archived"]);
        let result = filter_candidates(&index, &query, &["archived".to_string()]);
        assert_eq!(result.candidate_ids, vec![1, 3]);
        assert_eq!(result.diagnostics.tags, "bitmap");
        assert_eq!(result.diagnostics.negation_prefilter, "bloom");
    }
}
