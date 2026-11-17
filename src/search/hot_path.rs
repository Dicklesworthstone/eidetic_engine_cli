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
            negation_prefilter: "none",
            scoring: "fixed_point_scalar",
            candidate_count,
        }
    }

    #[must_use]
    pub const fn with_bloom_prefilter(candidate_count: usize) -> Self {
        Self {
            tags: "bitmap",
            negation_prefilter: "counting_bloom",
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
) -> SearchHotPathResult {
    filter_candidates_with_negation_prefilter(tag_index, query, None)
}

#[must_use]
pub fn filter_candidates_with_negation_prefilter(
    tag_index: &TagBitmapIndex,
    query: &TagBitmapQuery,
    negation_prefilter: Option<&CountingBloomPrefilter>,
) -> SearchHotPathResult {
    let candidate_ids = tag_index.matching_with_negation_prefilter(query, negation_prefilter);
    let diagnostics = if negation_prefilter.is_some() {
        SearchHotPathDiagnostics::with_bloom_prefilter(candidate_ids.len())
    } else {
        SearchHotPathDiagnostics::scalar(candidate_ids.len())
    };

    SearchHotPathResult {
        diagnostics,
        candidate_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::{filter_candidates, filter_candidates_with_negation_prefilter};
    use crate::search::bloom_prefilter::CountingBloomPrefilter;
    use crate::search::tag_bitmaps::{TagBitmapIndex, TagBitmapQuery};

    #[test]
    fn hot_path_filters_with_bitmap_and_reports_diagnostics() {
        let index = TagBitmapIndex::from_documents([
            (1, vec!["rust", "cli"]),
            (2, vec!["rust", "archived"]),
            (3, vec!["rust", "search"]),
        ]);
        let query = TagBitmapQuery::new(["rust"], ["archived"]);
        let result = filter_candidates(&index, &query);
        assert_eq!(result.candidate_ids, vec![1, 3]);
        assert_eq!(result.diagnostics.tags, "bitmap");
        assert_eq!(result.diagnostics.negation_prefilter, "none");

        let mut bloom = CountingBloomPrefilter::with_capacity(16);
        bloom.insert("archived");
        let bloom_result = filter_candidates_with_negation_prefilter(&index, &query, Some(&bloom));
        assert_eq!(bloom_result.candidate_ids, vec![1, 3]);
        assert_eq!(
            bloom_result.diagnostics.negation_prefilter,
            "counting_bloom"
        );
    }
}
