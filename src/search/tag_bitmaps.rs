use std::collections::{BTreeMap, BTreeSet};

/// Deterministic per-tag document ordinal index for the SRR4 search hot path.
///
/// The representation deliberately stays behind this module boundary. It is
/// currently a sparse standard-library set so the ee crate can land the filter
/// semantics without changing the shared manifest; replacing the storage with a
/// Roaring bitmap only has to preserve this API and the inline tests below.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TagBitmapIndex {
    all_documents: BTreeSet<u64>,
    by_tag: BTreeMap<String, BTreeSet<u64>>,
}

impl TagBitmapIndex {
    #[must_use]
    pub fn from_documents<I, T>(documents: I) -> Self
    where
        I: IntoIterator<Item = (u64, T)>,
        T: IntoIterator,
        T::Item: AsRef<str>,
    {
        let mut index = Self::default();
        for (document_id, tags) in documents {
            index.insert(document_id, tags);
        }
        index
    }

    pub fn insert<T>(&mut self, document_id: u64, tags: T)
    where
        T: IntoIterator,
        T::Item: AsRef<str>,
    {
        self.all_documents.insert(document_id);
        for tag in tags {
            let tag = normalize_tag(tag.as_ref());
            if tag.is_empty() {
                continue;
            }
            self.by_tag.entry(tag).or_default().insert(document_id);
        }
    }

    #[must_use]
    pub fn document_count(&self) -> usize {
        self.all_documents.len()
    }

    #[must_use]
    pub fn cardinality(&self, tag: &str) -> usize {
        self.by_tag
            .get(&normalize_tag(tag))
            .map_or(0, BTreeSet::len)
    }

    #[must_use]
    pub fn matching(&self, query: &TagBitmapQuery) -> Vec<u64> {
        let mut candidates = if let Some(first) = query.include.first() {
            self.by_tag.get(first).cloned().unwrap_or_default()
        } else {
            self.all_documents.clone()
        };

        for tag in query.include.iter().skip(1) {
            match self.by_tag.get(tag) {
                Some(ids) => {
                    candidates = candidates.intersection(ids).copied().collect();
                }
                None => return Vec::new(),
            }
        }

        for tag in &query.exclude {
            if let Some(ids) = self.by_tag.get(tag) {
                candidates = candidates.difference(ids).copied().collect();
            }
        }

        candidates.into_iter().collect()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TagBitmapQuery {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl TagBitmapQuery {
    #[must_use]
    pub fn new<I, E>(include: I, exclude: E) -> Self
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
        E: IntoIterator,
        E::Item: AsRef<str>,
    {
        let mut include = include
            .into_iter()
            .map(|tag| normalize_tag(tag.as_ref()))
            .filter(|tag| !tag.is_empty())
            .collect::<Vec<_>>();
        include.sort();
        include.dedup();

        let mut exclude = exclude
            .into_iter()
            .map(|tag| normalize_tag(tag.as_ref()))
            .filter(|tag| !tag.is_empty())
            .collect::<Vec<_>>();
        exclude.sort();
        exclude.dedup();

        Self { include, exclude }
    }

    #[must_use]
    pub fn includes(&self) -> &[String] {
        &self.include
    }

    #[must_use]
    pub fn excludes(&self) -> &[String] {
        &self.exclude
    }
}

fn normalize_tag(tag: &str) -> String {
    tag.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{TagBitmapIndex, TagBitmapQuery};

    #[test]
    fn build_index_empty() {
        let index = TagBitmapIndex::from_documents(Vec::<(u64, Vec<&str>)>::new());
        assert_eq!(index.document_count(), 0);
        assert_eq!(index.cardinality("rust"), 0);
        assert!(index.matching(&TagBitmapQuery::default()).is_empty());
    }

    #[test]
    fn build_index_dense() {
        let index = TagBitmapIndex::from_documents([
            (1, vec!["rust", "cli"]),
            (2, vec!["rust", "search"]),
            (3, vec!["rust", "search", "cli"]),
        ]);
        assert_eq!(index.document_count(), 3);
        assert_eq!(index.cardinality("rust"), 3);
        assert_eq!(index.cardinality("search"), 2);
    }

    #[test]
    fn intersection_correctness() {
        let index = TagBitmapIndex::from_documents([
            (1, vec!["rust", "cli"]),
            (2, vec!["rust", "search"]),
            (3, vec!["rust", "search", "cli"]),
            (4, vec!["docs"]),
        ]);
        let query = TagBitmapQuery::new(["rust", "search"], std::iter::empty::<&str>());
        assert_eq!(index.matching(&query), vec![2, 3]);
    }

    #[test]
    fn negation_correctness() {
        let index = TagBitmapIndex::from_documents([
            (1, vec!["rust", "cli"]),
            (2, vec!["rust", "archived"]),
            (3, vec!["rust", "search"]),
        ]);
        let query = TagBitmapQuery::new(["rust"], ["archived"]);
        assert_eq!(index.matching(&query), vec![1, 3]);
    }

    #[test]
    fn query_normalizes_and_deduplicates_tags() {
        let query = TagBitmapQuery::new([" Rust ", "rust", ""], [" Archived ", "archived"]);
        assert_eq!(query.includes(), &["rust".to_string()]);
        assert_eq!(query.excludes(), &["archived".to_string()]);
    }
}
