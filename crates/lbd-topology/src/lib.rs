//! IFC topology derivation from relationship entities.

use std::collections::{HashMap, HashSet};

use ifc_model::IfcModel;
use ifc_schema::SpatialType;
use ifc_step::EntityId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TopologyNodeKind {
    Project,
    Site,
    Building,
    Storey,
    Space,
    Element,
    Interface,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TopologyEdgeKind {
    ContainsZone,
    ContainsElement,
    AdjacentElement,
    AdjacentZone,
    HasSubElement,
    IntersectingElement,
    InterfaceOf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TopologyEdge {
    pub source: EntityId,
    pub target: EntityId,
    pub kind: TopologyEdgeKind,
    pub derived_from: Option<&'static str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopologyGraph {
    pub node_kinds: HashMap<EntityId, TopologyNodeKind>,
    pub core_edges: Vec<TopologyEdge>,
    pub extension_edges: Vec<TopologyEdge>,
    pub adjacent_elements_of_space: HashMap<EntityId, Vec<EntityId>>,
    pub hosted_elements_of_host: HashMap<EntityId, Vec<EntityId>>,
    pub contained_elements_of_structure: HashMap<EntityId, Vec<EntityId>>,
}

impl TopologyGraph {
    pub fn core_pairs_of_kind(&self, kind: TopologyEdgeKind) -> Vec<(EntityId, EntityId)> {
        let mut seen = HashSet::new();
        let mut pairs = Vec::new();
        for edge in &self.core_edges {
            if edge.kind != kind {
                continue;
            }
            if seen.insert((edge.source, edge.target)) {
                pairs.push((edge.source, edge.target));
            }
        }
        pairs.sort_unstable();
        pairs
    }
}

pub trait TopologyEnricher {
    fn enrich(&self, model: &IfcModel, graph: &mut TopologyGraph);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTopologyEnricher;

impl TopologyEnricher for NoopTopologyEnricher {
    fn enrich(&self, _model: &IfcModel, _graph: &mut TopologyGraph) {}
}

#[derive(Debug, Default, Clone, Copy)]
pub struct IfcRelationEvidenceEnricher;

impl TopologyEnricher for IfcRelationEvidenceEnricher {
    fn enrich(&self, _model: &IfcModel, _graph: &mut TopologyGraph) {}
}

pub fn build_topology(model: &IfcModel) -> TopologyGraph {
    let mut node_kinds = HashMap::new();
    let mut core_edges = Vec::new();
    let mut adjacent_elements_of_space: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
    let mut spaces_of_adjacent_element: HashMap<EntityId, Vec<(EntityId, bool)>> = HashMap::new();
    let mut hosted_elements_of_host: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
    let mut contained_elements_of_structure: HashMap<EntityId, Vec<EntityId>> = HashMap::new();
    let mut opening_to_host = HashMap::new();

    for (&id, node) in &model.spatial_nodes {
        node_kinds.insert(id, spatial_node_kind(node.spatial_type));
    }
    for &id in model.elements.keys() {
        node_kinds.insert(id, TopologyNodeKind::Element);
    }

    for rel in &model.rel_voids {
        opening_to_host.insert(rel.opening, rel.element);
    }

    for rel in &model.rel_aggregates {
        if !model.spatial_nodes.contains_key(&rel.parent) {
            continue;
        }
        for &child in &rel.children {
            if !model.spatial_nodes.contains_key(&child) {
                continue;
            }
            core_edges.push(TopologyEdge {
                source: rel.parent,
                target: child,
                kind: TopologyEdgeKind::ContainsZone,
                derived_from: Some("IfcRelAggregates"),
            });
        }
    }

    for rel in &model.rel_space_boundaries {
        let Some(element_id) = rel.element else {
            continue;
        };
        if !model.spatial_nodes.contains_key(&rel.space)
            || !model.elements.contains_key(&element_id)
        {
            continue;
        }
        push_unique(&mut adjacent_elements_of_space, rel.space, element_id);
        let is_external = rel
            .internal_or_external
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("EXTERNAL"));
        spaces_of_adjacent_element
            .entry(element_id)
            .or_default()
            .push((rel.space, is_external));
        core_edges.push(TopologyEdge {
            source: rel.space,
            target: element_id,
            kind: TopologyEdgeKind::AdjacentElement,
            derived_from: Some("IfcRelSpaceBoundary"),
        });
    }

    for spaces in spaces_of_adjacent_element.values() {
        for left_index in 0..spaces.len() {
            for right_index in (left_index + 1)..spaces.len() {
                let (left_space, left_external) = spaces[left_index];
                let (right_space, right_external) = spaces[right_index];
                if left_space == right_space || (left_external && right_external) {
                    continue;
                }
                core_edges.push(TopologyEdge {
                    source: left_space,
                    target: right_space,
                    kind: TopologyEdgeKind::AdjacentZone,
                    derived_from: Some("IfcRelSpaceBoundary"),
                });
                core_edges.push(TopologyEdge {
                    source: right_space,
                    target: left_space,
                    kind: TopologyEdgeKind::AdjacentZone,
                    derived_from: Some("IfcRelSpaceBoundary"),
                });
            }
        }
    }

    for rel in &model.rel_fills {
        let Some(host_id) = opening_to_host.get(&rel.opening).copied() else {
            continue;
        };
        if !model.elements.contains_key(&host_id) || !model.elements.contains_key(&rel.element) {
            continue;
        }
        push_unique(&mut hosted_elements_of_host, host_id, rel.element);
        core_edges.push(TopologyEdge {
            source: host_id,
            target: rel.element,
            kind: TopologyEdgeKind::HasSubElement,
            derived_from: Some("IfcRelVoidsElement+IfcRelFillsElement"),
        });
    }

    for (&element_id, &structure_id) in &model.contained_in {
        if !model.elements.contains_key(&element_id)
            || !model.spatial_nodes.contains_key(&structure_id)
        {
            continue;
        }
        push_unique(
            &mut contained_elements_of_structure,
            structure_id,
            element_id,
        );
        core_edges.push(TopologyEdge {
            source: structure_id,
            target: element_id,
            kind: TopologyEdgeKind::ContainsElement,
            derived_from: Some("IfcRelContainedInSpatialStructure"),
        });
    }

    core_edges.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| edge_kind_rank(left.kind).cmp(&edge_kind_rank(right.kind)))
            .then_with(|| left.derived_from.cmp(&right.derived_from))
    });

    TopologyGraph {
        node_kinds,
        core_edges,
        extension_edges: Vec::new(),
        adjacent_elements_of_space,
        hosted_elements_of_host,
        contained_elements_of_structure,
    }
}

pub fn build_topology_with_enricher(
    model: &IfcModel,
    enricher: &impl TopologyEnricher,
) -> TopologyGraph {
    let mut graph = build_topology(model);
    enricher.enrich(model, &mut graph);
    graph
}

fn spatial_node_kind(spatial_type: SpatialType) -> TopologyNodeKind {
    match spatial_type {
        SpatialType::Project => TopologyNodeKind::Project,
        SpatialType::Site => TopologyNodeKind::Site,
        SpatialType::Building => TopologyNodeKind::Building,
        SpatialType::Storey => TopologyNodeKind::Storey,
        SpatialType::Space => TopologyNodeKind::Space,
    }
}

fn push_unique(map: &mut HashMap<EntityId, Vec<EntityId>>, key: EntityId, value: EntityId) {
    let values = map.entry(key).or_default();
    if !values.contains(&value) {
        values.push(value);
    }
}

fn edge_kind_rank(kind: TopologyEdgeKind) -> u8 {
    match kind {
        TopologyEdgeKind::ContainsZone => 0,
        TopologyEdgeKind::ContainsElement => 1,
        TopologyEdgeKind::AdjacentElement => 2,
        TopologyEdgeKind::AdjacentZone => 3,
        TopologyEdgeKind::HasSubElement => 4,
        TopologyEdgeKind::IntersectingElement => 5,
        TopologyEdgeKind::InterfaceOf => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ifc_model::build_model;
    use ifc_step::parse_step_file;
    use std::path::PathBuf;

    fn duplex_model() -> Option<IfcModel> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Duplex.ifc");
        if !path.exists() {
            return None;
        }
        let step = parse_step_file(&path).ok()?;
        build_model(&step).ok()
    }

    #[test]
    fn test_build_topology_from_duplex() {
        let Some(model) = duplex_model() else {
            return;
        };
        let topology = build_topology(&model);

        assert!(topology.adjacent_elements_of_space.len() > 10);
        assert!(topology.hosted_elements_of_host.len() > 10);
        assert!(topology
            .adjacent_elements_of_space
            .values()
            .any(|elements| !elements.is_empty()));
        assert!(topology
            .hosted_elements_of_host
            .values()
            .any(|elements| !elements.is_empty()));
        assert!(topology.contained_elements_of_structure.len() > 10);
        assert!(topology
            .contained_elements_of_structure
            .values()
            .any(|elements| !elements.is_empty()));
        assert!(topology.node_kinds.len() > 10);
        assert!(topology
            .core_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::ContainsElement));
        assert!(topology
            .core_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::ContainsZone));
        assert!(topology
            .core_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::AdjacentElement));
        assert!(topology
            .core_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::AdjacentZone));
        assert!(topology
            .core_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::HasSubElement));
        // IntersectingElement must NOT be derived from void/fill chains — only geometry evidence.
        assert!(!topology
            .core_edges
            .iter()
            .any(|edge| edge.kind == TopologyEdgeKind::IntersectingElement));
    }

    #[test]
    fn test_build_topology_does_not_propagate_containment_to_aggregated_element_children() {
        let Some(model) = duplex_model() else {
            return;
        };
        let topology = build_topology(&model);
        let storey_id = 39;
        let stairflight_ids: Vec<_> = model
            .elements
            .iter()
            .filter_map(|(&id, element)| (element.entity_name == "IFCSTAIRFLIGHT").then_some(id))
            .collect();

        assert!(!stairflight_ids.is_empty());
        assert!(stairflight_ids
            .iter()
            .all(|id| { !topology.contained_elements_of_structure[&storey_id].contains(id) }));
    }

    #[test]
    fn test_build_topology_keeps_containment_on_direct_structure_only() {
        let Some(model) = duplex_model() else {
            return;
        };
        let topology = build_topology(&model);
        let wall_id = model.guid_to_entity["2O2Fr$t4X7Zf8NOew3FNtn"];
        let storey_id = 39;
        let building_id = model.guid_to_entity["1xS3BCk291UvhgP2a6eflK"];

        assert!(topology.contained_elements_of_structure[&storey_id].contains(&wall_id));
        assert!(!topology
            .contained_elements_of_structure
            .get(&building_id)
            .is_some_and(|elements| elements.contains(&wall_id)));
    }

    #[test]
    fn test_build_topology_with_noop_enricher_keeps_core_graph() {
        let Some(model) = duplex_model() else {
            return;
        };
        let plain = build_topology(&model);
        let enriched = build_topology_with_enricher(&model, &NoopTopologyEnricher);

        assert_eq!(plain.node_kinds, enriched.node_kinds);
        assert_eq!(plain.core_edges, enriched.core_edges);
        assert_eq!(plain.extension_edges, enriched.extension_edges);
        assert_eq!(
            plain.contained_elements_of_structure,
            enriched.contained_elements_of_structure
        );
    }

    #[test]
    fn test_build_topology_with_relation_evidence_enricher_keeps_graph_shape() {
        let Some(model) = duplex_model() else {
            return;
        };
        let plain = build_topology(&model);
        let topology = build_topology_with_enricher(&model, &IfcRelationEvidenceEnricher);

        assert_eq!(plain.node_kinds, topology.node_kinds);
        assert_eq!(plain.core_edges, topology.core_edges);
        assert!(topology.extension_edges.is_empty());
    }

    #[test]
    fn test_build_topology_adjacent_zone_has_no_self_loops() {
        let Some(model) = duplex_model() else {
            return;
        };
        let topology = build_topology(&model);

        assert!(topology
            .core_edges
            .iter()
            .filter(|edge| edge.kind == TopologyEdgeKind::AdjacentZone)
            .all(|edge| edge.source != edge.target));
    }
}
