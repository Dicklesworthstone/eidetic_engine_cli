use crate::models::CapabilityStatus;

pub const SUBSYSTEM: &str = "graph";
pub const MODULE_CONTRACT: &str = "ee.graph.module.v1";
pub const REQUIRED_GRAPH_ENGINE: &str = "franken_networkx";

static GRAPH_CAPABILITIES: [GraphCapability; 6] = [
    GraphCapability::ready(
        GraphCapabilityName::ModuleBoundary,
        GraphSurface::Status,
        "Graph module is present.",
    ),
    GraphCapability::pending(
        GraphCapabilityName::FrankenNetworkXDependency,
        GraphSurface::Projection,
        "Add the franken_networkx dependency before graph projections.",
    ),
    GraphCapability::pending(
        GraphCapabilityName::MemoryLinkTable,
        GraphSurface::Storage,
        "Add memory_links table for edge storage.",
    ),
    GraphCapability::pending(
        GraphCapabilityName::ProjectionBuilder,
        GraphSurface::Projection,
        "Wire graph projection from memory links.",
    ),
    GraphCapability::pending(
        GraphCapabilityName::CentralityMetrics,
        GraphSurface::Analytics,
        "Compute centrality metrics (PageRank, betweenness).",
    ),
    GraphCapability::pending(
        GraphCapabilityName::JsonGraph,
        GraphSurface::Query,
        "Expose graph metrics through stable JSON response envelope.",
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphModuleReadiness {
    contract: &'static str,
    subsystem: &'static str,
    graph_engine: &'static str,
    capabilities: &'static [GraphCapability],
}

impl GraphModuleReadiness {
    #[must_use]
    pub const fn contract(&self) -> &'static str {
        self.contract
    }

    #[must_use]
    pub const fn subsystem(&self) -> &'static str {
        self.subsystem
    }

    #[must_use]
    pub const fn graph_engine(&self) -> &'static str {
        self.graph_engine
    }

    #[must_use]
    pub const fn capabilities(&self) -> &'static [GraphCapability] {
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

    pub fn missing_capabilities(&self) -> impl Iterator<Item = GraphCapability> + '_ {
        self.capabilities
            .iter()
            .copied()
            .filter(|capability| capability.status() != CapabilityStatus::Ready)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphCapability {
    name: GraphCapabilityName,
    surface: GraphSurface,
    status: CapabilityStatus,
    repair: &'static str,
}

impl GraphCapability {
    const fn ready(name: GraphCapabilityName, surface: GraphSurface, repair: &'static str) -> Self {
        Self {
            name,
            surface,
            status: CapabilityStatus::Ready,
            repair,
        }
    }

    const fn pending(
        name: GraphCapabilityName,
        surface: GraphSurface,
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
    pub const fn name(self) -> GraphCapabilityName {
        self.name
    }

    #[must_use]
    pub const fn surface(self) -> GraphSurface {
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
pub enum GraphCapabilityName {
    ModuleBoundary,
    FrankenNetworkXDependency,
    MemoryLinkTable,
    ProjectionBuilder,
    CentralityMetrics,
    JsonGraph,
}

impl GraphCapabilityName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModuleBoundary => "module_boundary",
            Self::FrankenNetworkXDependency => "frankennetworkx_dependency",
            Self::MemoryLinkTable => "memory_link_table",
            Self::ProjectionBuilder => "projection_builder",
            Self::CentralityMetrics => "centrality_metrics",
            Self::JsonGraph => "json_graph",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphSurface {
    Status,
    Storage,
    Projection,
    Analytics,
    Query,
}

impl GraphSurface {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Storage => "storage",
            Self::Projection => "projection",
            Self::Analytics => "analytics",
            Self::Query => "query",
        }
    }
}

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[must_use]
pub const fn module_readiness() -> GraphModuleReadiness {
    GraphModuleReadiness {
        contract: MODULE_CONTRACT,
        subsystem: SUBSYSTEM,
        graph_engine: REQUIRED_GRAPH_ENGINE,
        capabilities: &GRAPH_CAPABILITIES,
    }
}

#[cfg(test)]
mod tests {
    use crate::models::CapabilityStatus;

    use super::{
        GraphCapabilityName, GraphSurface, REQUIRED_GRAPH_ENGINE, module_readiness, subsystem_name,
    };

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "graph");
    }

    #[test]
    fn module_contract_names_frankennetworkx_boundary() {
        let readiness = module_readiness();

        assert_eq!(readiness.contract(), "ee.graph.module.v1");
        assert_eq!(readiness.subsystem(), "graph");
        assert_eq!(readiness.graph_engine(), REQUIRED_GRAPH_ENGINE);
        assert_eq!(readiness.graph_engine(), "franken_networkx");
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
        assert_eq!(readiness.missing_capabilities().count(), 5);
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
                "frankennetworkx_dependency",
                "memory_link_table",
                "projection_builder",
                "centrality_metrics",
                "json_graph",
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
            vec!["status", "projection", "storage", "projection", "analytics", "query",]
        );
    }

    #[test]
    fn missing_capabilities_keep_repair_metadata() {
        let missing: Vec<_> = module_readiness().missing_capabilities().collect();

        assert_eq!(
            missing.first().map(|capability| capability.name()),
            Some(GraphCapabilityName::FrankenNetworkXDependency)
        );
        assert_eq!(
            missing.first().map(|capability| capability.surface()),
            Some(GraphSurface::Projection)
        );
        assert!(
            missing
                .first()
                .map(|capability| capability.repair().contains("franken_networkx"))
                .unwrap_or(false)
        );
    }
}
