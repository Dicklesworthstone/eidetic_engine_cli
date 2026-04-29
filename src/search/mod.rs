use crate::models::CapabilityStatus;

pub const SUBSYSTEM: &str = "search";
pub const MODULE_CONTRACT: &str = "ee.search.module.v1";
pub const REQUIRED_RETRIEVAL_ENGINE: &str = "frankensearch::TwoTierSearcher";

static SEARCH_CAPABILITIES: [SearchCapability; 7] = [
    SearchCapability::ready(
        SearchCapabilityName::ModuleBoundary,
        SearchSurface::Status,
        "Search module is present.",
    ),
    SearchCapability::pending(
        SearchCapabilityName::FrankensearchDependency,
        SearchSurface::IndexAndQuery,
        "Add the frankensearch dependency profile before indexing or querying.",
    ),
    SearchCapability::pending(
        SearchCapabilityName::CanonicalDocument,
        SearchSurface::Indexing,
        "Define canonical search documents before rebuilding indexes.",
    ),
    SearchCapability::pending(
        SearchCapabilityName::IndexJobs,
        SearchSurface::Indexing,
        "Persist search index jobs through the database layer.",
    ),
    SearchCapability::pending(
        SearchCapabilityName::IndexRebuild,
        SearchSurface::Indexing,
        "Wire index rebuild through Frankensearch.",
    ),
    SearchCapability::pending(
        SearchCapabilityName::JsonSearch,
        SearchSurface::Query,
        "Expose search results through the stable JSON response envelope.",
    ),
    SearchCapability::pending(
        SearchCapabilityName::ScoreExplanation,
        SearchSurface::Explanation,
        "Carry Frankensearch score metadata into ee explanations.",
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchModuleReadiness {
    contract: &'static str,
    subsystem: &'static str,
    retrieval_engine: &'static str,
    capabilities: &'static [SearchCapability],
}

impl SearchModuleReadiness {
    #[must_use]
    pub const fn contract(&self) -> &'static str {
        self.contract
    }

    #[must_use]
    pub const fn subsystem(&self) -> &'static str {
        self.subsystem
    }

    #[must_use]
    pub const fn retrieval_engine(&self) -> &'static str {
        self.retrieval_engine
    }

    #[must_use]
    pub const fn capabilities(&self) -> &'static [SearchCapability] {
        self.capabilities
    }

    #[must_use]
    pub fn status(&self) -> CapabilityStatus {
        if self
            .capabilities
            .iter()
            .all(|capability| capability.status() == CapabilityStatus::Ready)
        {
            CapabilityStatus::Ready
        } else {
            CapabilityStatus::Pending
        }
    }

    pub fn missing_capabilities(&self) -> impl Iterator<Item = SearchCapability> + '_ {
        self.capabilities
            .iter()
            .copied()
            .filter(|capability| capability.status() != CapabilityStatus::Ready)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchCapability {
    name: SearchCapabilityName,
    surface: SearchSurface,
    status: CapabilityStatus,
    repair: &'static str,
}

impl SearchCapability {
    const fn ready(
        name: SearchCapabilityName,
        surface: SearchSurface,
        repair: &'static str,
    ) -> Self {
        Self {
            name,
            surface,
            status: CapabilityStatus::Ready,
            repair,
        }
    }

    const fn pending(
        name: SearchCapabilityName,
        surface: SearchSurface,
        repair: &'static str,
    ) -> Self {
        Self {
            name,
            surface,
            status: CapabilityStatus::Pending,
            repair,
        }
    }

    #[must_use]
    pub const fn name(self) -> SearchCapabilityName {
        self.name
    }

    #[must_use]
    pub const fn surface(self) -> SearchSurface {
        self.surface
    }

    #[must_use]
    pub const fn status(self) -> CapabilityStatus {
        self.status
    }

    #[must_use]
    pub const fn repair(self) -> &'static str {
        self.repair
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchCapabilityName {
    ModuleBoundary,
    FrankensearchDependency,
    CanonicalDocument,
    IndexJobs,
    IndexRebuild,
    JsonSearch,
    ScoreExplanation,
}

impl SearchCapabilityName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModuleBoundary => "module_boundary",
            Self::FrankensearchDependency => "frankensearch_dependency",
            Self::CanonicalDocument => "canonical_document",
            Self::IndexJobs => "index_jobs",
            Self::IndexRebuild => "index_rebuild",
            Self::JsonSearch => "json_search",
            Self::ScoreExplanation => "score_explanation",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSurface {
    Status,
    Indexing,
    Query,
    Explanation,
    IndexAndQuery,
}

impl SearchSurface {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Indexing => "indexing",
            Self::Query => "query",
            Self::Explanation => "explanation",
            Self::IndexAndQuery => "index_and_query",
        }
    }
}

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[must_use]
pub const fn module_readiness() -> SearchModuleReadiness {
    SearchModuleReadiness {
        contract: MODULE_CONTRACT,
        subsystem: SUBSYSTEM,
        retrieval_engine: REQUIRED_RETRIEVAL_ENGINE,
        capabilities: &SEARCH_CAPABILITIES,
    }
}

#[cfg(test)]
mod tests {
    use crate::models::CapabilityStatus;

    use super::{
        REQUIRED_RETRIEVAL_ENGINE, SearchCapabilityName, SearchSurface, module_readiness,
        subsystem_name,
    };

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "search");
    }

    #[test]
    fn module_contract_names_frankensearch_boundary() {
        let readiness = module_readiness();

        assert_eq!(readiness.contract(), "ee.search.module.v1");
        assert_eq!(readiness.subsystem(), "search");
        assert_eq!(readiness.retrieval_engine(), REQUIRED_RETRIEVAL_ENGINE);
        assert_eq!(
            readiness.retrieval_engine(),
            "frankensearch::TwoTierSearcher"
        );
    }

    #[test]
    fn readiness_reports_pending_until_integration_lands() {
        let readiness = module_readiness();

        assert_eq!(readiness.status(), CapabilityStatus::Pending);
        assert_eq!(
            readiness
                .capabilities()
                .first()
                .map(|capability| capability.status()),
            Some(CapabilityStatus::Ready)
        );
        assert_eq!(readiness.missing_capabilities().count(), 6);
    }

    #[test]
    fn capabilities_are_in_dependency_order() {
        let names: Vec<&str> = module_readiness()
            .capabilities()
            .iter()
            .map(|capability| capability.name().as_str())
            .collect();

        assert_eq!(
            names,
            vec![
                "module_boundary",
                "frankensearch_dependency",
                "canonical_document",
                "index_jobs",
                "index_rebuild",
                "json_search",
                "score_explanation",
            ]
        );
    }

    #[test]
    fn capability_surfaces_are_stable() {
        let surfaces: Vec<&str> = module_readiness()
            .capabilities()
            .iter()
            .map(|capability| capability.surface().as_str())
            .collect();

        assert_eq!(
            surfaces,
            vec![
                "status",
                "index_and_query",
                "indexing",
                "indexing",
                "indexing",
                "query",
                "explanation",
            ]
        );
    }

    #[test]
    fn missing_capabilities_keep_repair_metadata() {
        let missing: Vec<_> = module_readiness().missing_capabilities().collect();

        assert_eq!(
            missing.first().map(|capability| capability.name()),
            Some(SearchCapabilityName::FrankensearchDependency)
        );
        assert_eq!(
            missing.first().map(|capability| capability.surface()),
            Some(SearchSurface::IndexAndQuery)
        );
        assert!(
            missing
                .first()
                .map(|capability| capability.repair().contains("frankensearch"))
                .unwrap_or(false)
        );
    }
}
