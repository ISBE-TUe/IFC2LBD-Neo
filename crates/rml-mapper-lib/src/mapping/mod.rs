//! RML Mapping Document
//!
//! This module handles parsing and representation of RML mapping documents.
//! It defines the structure of triples maps, logical sources, subject maps,
//! predicate-object maps, and other RML constructs.
//!
//! # Architecture
//!
//! The module follows the Java RML Mapper's MappingFactory design:
//! - `MappingDocument`: Complete RML mapping with all triples maps
//! - `TriplesMap`: Individual mapping from source to RDF triples
//! - `LogicalSource`: Data source definition with iterator
//! - `SubjectMap`: Defines how to generate subjects
//! - `PredicateObjectMap`: Defines properties and their values
//! - `MappingFactory`: Parses RML rules from a QuadStore
//!
//! # Examples
//!
//! ```
//! use rml_mapper::mapping::{MappingFactory, StrictMode};
//! use rml_mapper::store::InMemoryQuadStore;
//!
//! let store = InMemoryQuadStore::new();
//! let factory = MappingFactory::new(None, StrictMode::Strict);
//! // let mapping = factory.create_mapping(&store).unwrap();
//! ```

use crate::error::{Result, RmlError};
use crate::namespaces::{RML2, RR, FNML};
use crate::store::QuadStore;
use crate::term::{NamedNode, TermRef, Term};
use crate::termgenerator::{
    BlankNodeGenerator, ConstantExtractor, LiteralGenerator, NamedNodeGenerator,
    ReferenceExtractor, TemplateExtractor, TermGenerator, ValueExtractor,
};

/// Strict mode for IRI generation
///
/// Controls how invalid IRIs are handled during mapping execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictMode {
    /// Fail on invalid IRIs
    Strict,
    /// Skip invalid IRIs and continue
    BestEffort,
}

/// Represents a complete RML mapping document
pub struct MappingDocument {
    /// Collection of triples maps
    pub triples_maps: Vec<TriplesMap>,
    /// Base IRI for resolving relative IRIs
    pub base_iri: Option<String>,
}

impl MappingDocument {
    /// Creates a new empty mapping document
    pub fn new(base_iri: Option<String>) -> Self {
        Self {
            triples_maps: Vec::new(),
            base_iri,
        }
    }

    /// Adds a triples map to the document
    pub fn add_triples_map(&mut self, triples_map: TriplesMap) {
        self.triples_maps.push(triples_map);
    }

    /// Returns the number of triples maps
    pub fn len(&self) -> usize {
        self.triples_maps.len()
    }

    /// Returns true if there are no triples maps
    pub fn is_empty(&self) -> bool {
        self.triples_maps.is_empty()
    }
}

/// Represents an RML triples map
pub struct TriplesMap {
    /// Unique identifier for this triples map
    pub id: TermRef,
    /// Logical source defining where data comes from
    pub logical_source: LogicalSource,
    /// Subject map defining how to generate subjects
    pub subject_map: SubjectMap,
    /// Predicate-object maps defining properties and values
    pub predicate_object_maps: Vec<PredicateObjectMap>,
}

/// Represents a logical source (data source + iterator)
#[derive(Debug, Clone)]
pub struct LogicalSource {
    /// Source location (file path, URL, etc.)
    pub source: TermRef,
    /// Reference formulation (CSV, JSON, XPath, etc.)
    pub reference_formulation: String,
    /// Iterator expression (e.g., JSONPath, XPath)
    pub iterator: Option<String>,
}

/// Represents a subject map
pub struct SubjectMap {
    /// Term generator for creating subjects
    pub term_generator: Box<dyn TermGenerator>,
    /// RDF classes for the subject (rr:class)
    pub classes: Vec<NamedNode>,
    /// Graph maps for the subject
    pub graph_maps: Vec<GraphMap>,
}

/// Represents a predicate-object map
pub struct PredicateObjectMap {
    /// Predicate maps
    pub predicate_maps: Vec<PredicateMap>,
    /// Object maps
    pub object_maps: Vec<ObjectMap>,
    /// Graph maps for this predicate-object pair
    pub graph_maps: Vec<GraphMap>,
}

/// Represents a predicate map
pub struct PredicateMap {
    /// Term generator for creating predicates
    pub term_generator: Box<dyn TermGenerator>,
}

/// Represents an object map
pub enum ObjectMap {
    /// Regular term map
    TermMap {
        /// Term generator for creating objects
        term_generator: Box<dyn TermGenerator>,
    },
    /// Referencing object map (join with another triples map)
    RefObjectMap {
        /// Parent triples map reference
        parent_triples_map: TermRef,
        /// Join conditions
        join_conditions: Vec<JoinCondition>,
    },
}

/// Join condition for referencing object maps
#[derive(Debug, Clone)]
pub struct JoinCondition {
    /// Child reference (field in current source)
    pub child: String,
    /// Parent reference (field in parent source)
    pub parent: String,
}

/// Graph map
pub struct GraphMap {
    /// Term generator for creating graph names
    pub term_generator: Box<dyn TermGenerator>,
}

/// Factory for creating mapping documents from RML rules
pub struct MappingFactory {
    base_iri: Option<String>,
    strict_mode: StrictMode,
}

impl MappingFactory {
    /// Creates a new mapping factory
    ///
    /// # Arguments
    ///
    /// * `base_iri` - Optional base IRI for resolving relative IRIs
    /// * `strict_mode` - How to handle invalid IRIs
    pub fn new(base_iri: Option<String>, strict_mode: StrictMode) -> Self {
        Self {
            base_iri,
            strict_mode,
        }
    }

    /// Creates a MappingDocument from RML rules in a QuadStore
    ///
    /// # Arguments
    ///
    /// * `store` - QuadStore containing RML mapping rules
    ///
    /// # Returns
    ///
    /// A complete MappingDocument with all triples maps parsed
    pub fn create_mapping(&self, store: &impl QuadStore) -> Result<MappingDocument> {
        let mut document = MappingDocument::new(self.base_iri.clone());

        // Find all triples maps (subjects with rr:logicalSource or rml:logicalSource)
        let triples_map_ids = self.find_triples_maps(store)?;

        for triples_map_id in triples_map_ids {
            let triples_map = self.create_triples_map(&triples_map_id, store)?;
            document.add_triples_map(triples_map);
        }

        if document.is_empty() {
            return Err(RmlError::Mapping(
                "No triples maps found in mapping document".to_string(),
            ));
        }

        Ok(document)
    }

    /// Finds all triples map IDs in the store
    fn find_triples_maps(&self, store: &impl QuadStore) -> Result<Vec<TermRef>> {
        let logical_source_pred = NamedNode::new(format!("{}logicalSource", RML2))
            .map_err(RmlError::Parse)?;

        let quads = store.get_quads(None, Some(&logical_source_pred), None, None)?;

        let mut triples_map_ids = Vec::new();
        for quad in quads {
            triples_map_ids.push(quad.subject().clone());
        }

        // Also check for rr:logicalSource for R2RML compatibility
        let rr_logical_source_pred = NamedNode::new(format!("{}logicalSource", RR))
            .map_err(RmlError::Parse)?;

        let rr_quads = store.get_quads(None, Some(&rr_logical_source_pred), None, None)?;
        for quad in rr_quads {
            if !triples_map_ids.contains(quad.subject()) {
                triples_map_ids.push(quad.subject().clone());
            }
        }

        Ok(triples_map_ids)
    }

    /// Creates a single TriplesMap
    fn create_triples_map(
        &self,
        triples_map_id: &TermRef,
        store: &impl QuadStore,
    ) -> Result<TriplesMap> {
        // Get base IRI for this triples map (may override global base IRI)
        let base_iri = self.get_triples_map_base_iri(triples_map_id, store)?;

        // Parse logical source
        let logical_source = self.parse_logical_source(triples_map_id, store)?;

        // Parse subject map
        let subject_map = self.parse_subject_map(triples_map_id, store, &base_iri)?;

        // Parse predicate-object maps
        let predicate_object_maps =
            self.parse_predicate_object_maps(triples_map_id, store, &base_iri)?;

        Ok(TriplesMap {
            id: triples_map_id.clone(),
            logical_source,
            subject_map,
            predicate_object_maps,
        })
    }

    /// Gets the base IRI for a triples map (may override global base IRI)
    fn get_triples_map_base_iri(
        &self,
        triples_map_id: &TermRef,
        store: &impl QuadStore,
    ) -> Result<Option<String>> {
        let base_iri_pred =
            NamedNode::new(format!("{}baseIRI", RML2)).map_err(RmlError::Parse)?;

        let quads = store.get_quads(Some(triples_map_id), Some(&base_iri_pred), None, None)?;

        if let Some(quad) = quads.first() {
            Ok(Some(quad.object().value().to_string()))
        } else {
            Ok(self.base_iri.clone())
        }
    }

    /// Parses a logical source
    fn parse_logical_source(
        &self,
        triples_map_id: &TermRef,
        store: &impl QuadStore,
    ) -> Result<LogicalSource> {
        // Get logical source node
        let logical_source_node = get_single_object(
            store,
            triples_map_id,
            &[
                &format!("{}logicalSource", RML2),
                &format!("{}logicalSource", RR),
            ],
        )?
        .ok_or_else(|| {
            RmlError::Mapping(format!(
                "No logical source found for triples map {}",
                triples_map_id
            ))
        })?;

        // Get source
        let source = get_single_object(
            store,
            &logical_source_node,
            &[&format!("{}source", RML2), &format!("{}source", RR)],
        )?
        .ok_or_else(|| {
            RmlError::Mapping(format!(
                "No source found in logical source {}",
                logical_source_node
            ))
        })?;

        // Get reference formulation
        let ref_formulation_node = get_single_object(
            store,
            &logical_source_node,
            &[
                &format!("{}referenceFormulation", RML2),
                &format!("{}referenceFormulation", RR),
            ],
        )?
        .ok_or_else(|| {
            RmlError::Mapping(format!(
                "No reference formulation found in logical source {}",
                logical_source_node
            ))
        })?;

        let reference_formulation = ref_formulation_node.value().to_string();

        // Get iterator (optional)
        let iterator = get_single_object(
            store,
            &logical_source_node,
            &[&format!("{}iterator", RML2), &format!("{}iterator", RR)],
        )?
        .map(|t| t.value().to_string());

        Ok(LogicalSource {
            source,
            reference_formulation,
            iterator,
        })
    }

    /// Parses a subject map
    fn parse_subject_map(
        &self,
        triples_map_id: &TermRef,
        store: &impl QuadStore,
        base_iri: &Option<String>,
    ) -> Result<SubjectMap> {
        // Get subject map node (or use shortcut rr:subject)
        let (subject_map_node, is_shortcut) = self.get_subject_map_node(triples_map_id, store)?;

        // Create term generator
        let term_generator = if is_shortcut {
            // For shortcuts, the value is directly the subject IRI/template
            let value = subject_map_node.value();
            if value.contains('{') {
                // It's a template
                let extractor = TemplateExtractor::new(value)?;
                Box::new(NamedNodeGenerator::new(
                    Box::new(extractor),
                    base_iri.clone(),
                    self.strict_mode.into(),
                )) as Box<dyn TermGenerator>
            } else {
                // It's a constant IRI
                let extractor = ConstantExtractor::new(value);
                Box::new(NamedNodeGenerator::new(
                    Box::new(extractor),
                    base_iri.clone(),
                    self.strict_mode.into(),
                )) as Box<dyn TermGenerator>
            }
        } else {
            // Regular subject map node
            let term_type = get_term_type(store, &subject_map_node)?;
            self.create_term_generator(
                store,
                &subject_map_node,
                &term_type,
                base_iri,
                true, // is_subject
            )?
        };

        // Get classes (rr:class) - only if not a shortcut
        let classes = if !is_shortcut {
            self.parse_classes(&subject_map_node, store)?
        } else {
            Vec::new()
        };

        // Get graph maps - only if not a shortcut
        let graph_maps = if !is_shortcut {
            self.parse_graph_maps(&subject_map_node, store, base_iri)?
        } else {
            Vec::new()
        };

        Ok(SubjectMap {
            term_generator,
            classes,
            graph_maps,
        })
    }

    /// Gets the subject map node (handles both rr:subjectMap and rr:subject shortcut)
    fn get_subject_map_node(
        &self,
        triples_map_id: &TermRef,
        store: &impl QuadStore,
    ) -> Result<(TermRef, bool)> {
        // Try rr:subjectMap first
        if let Some(node) = get_single_object(
            store,
            triples_map_id,
            &[
                &format!("{}subjectMap", RML2),
                &format!("{}subjectMap", RR),
            ],
        )? {
            return Ok((node, false)); // Not a shortcut
        }

        // Try rr:subject shortcut
        if let Some(node) = get_single_object(
            store,
            triples_map_id,
            &[&format!("{}subject", RML2), &format!("{}subject", RR)],
        )? {
            return Ok((node, true)); // Is a shortcut
        }

        Err(RmlError::Mapping(format!(
            "No subject map found for triples map {}",
            triples_map_id
        )))
    }

    /// Parses classes from a subject map
    fn parse_classes(
        &self,
        subject_map_node: &TermRef,
        store: &impl QuadStore,
    ) -> Result<Vec<NamedNode>> {
        let class_pred =
            NamedNode::new(format!("{}class", RML2)).map_err(RmlError::Parse)?;

        let quads = store.get_quads(Some(subject_map_node), Some(&class_pred), None, None)?;

        let mut classes = Vec::new();
        for quad in quads {
            if let TermRef::NamedNode(node) = quad.object() {
                classes.push(node.clone());
            }
        }

        // Also check rr:class for R2RML compatibility
        let rr_class_pred =
            NamedNode::new(format!("{}class", RR)).map_err(RmlError::Parse)?;

        let rr_quads = store.get_quads(Some(subject_map_node), Some(&rr_class_pred), None, None)?;
        for quad in rr_quads {
            if let TermRef::NamedNode(node) = quad.object() {
                classes.push(node.clone());
            }
        }

        Ok(classes)
    }

    /// Parses predicate-object maps
    fn parse_predicate_object_maps(
        &self,
        triples_map_id: &TermRef,
        store: &impl QuadStore,
        base_iri: &Option<String>,
    ) -> Result<Vec<PredicateObjectMap>> {
        let pom_pred = NamedNode::new(format!("{}predicateObjectMap", RML2))
            .map_err(RmlError::Parse)?;

        let quads = store.get_quads(Some(triples_map_id), Some(&pom_pred), None, None)?;

        let mut poms = Vec::new();
        for quad in quads {
            let pom_node = quad.object();
            let pom = self.parse_predicate_object_map(pom_node, store, base_iri)?;
            poms.push(pom);
        }

        // Also check rr:predicateObjectMap for R2RML compatibility
        let rr_pom_pred = NamedNode::new(format!("{}predicateObjectMap", RR))
            .map_err(RmlError::Parse)?;

        let rr_quads = store.get_quads(Some(triples_map_id), Some(&rr_pom_pred), None, None)?;
        for quad in rr_quads {
            let pom_node = quad.object();
            let pom = self.parse_predicate_object_map(pom_node, store, base_iri)?;
            poms.push(pom);
        }

        Ok(poms)
    }

    /// Parses a single predicate-object map
    fn parse_predicate_object_map(
        &self,
        pom_node: &TermRef,
        store: &impl QuadStore,
        base_iri: &Option<String>,
    ) -> Result<PredicateObjectMap> {
        // Parse predicate maps
        let predicate_maps = self.parse_predicate_maps(pom_node, store, base_iri)?;

        // Parse object maps
        let object_maps = self.parse_object_maps(pom_node, store, base_iri)?;

        // Parse graph maps
        let graph_maps = self.parse_graph_maps(pom_node, store, base_iri)?;

        Ok(PredicateObjectMap {
            predicate_maps,
            object_maps,
            graph_maps,
        })
    }

    /// Parses predicate maps
    fn parse_predicate_maps(
        &self,
        pom_node: &TermRef,
        store: &impl QuadStore,
        base_iri: &Option<String>,
    ) -> Result<Vec<PredicateMap>> {
        let mut predicate_maps = Vec::new();

        // Try rr:predicateMap
        let pred_map_pred = NamedNode::new(format!("{}predicateMap", RML2))
            .map_err(RmlError::Parse)?;

        let quads = store.get_quads(Some(pom_node), Some(&pred_map_pred), None, None)?;
        for quad in quads {
            let pred_map_node = quad.object();
            let term_generator = self.create_term_generator(
                store,
                pred_map_node,
                &TermType::Iri,
                base_iri,
                false,
            )?;
            predicate_maps.push(PredicateMap { term_generator });
        }

        // Try rr:predicate shortcut
        let pred_pred =
            NamedNode::new(format!("{}predicate", RML2)).map_err(RmlError::Parse)?;

        let pred_quads = store.get_quads(Some(pom_node), Some(&pred_pred), None, None)?;
        for quad in pred_quads {
            if let TermRef::NamedNode(_) = quad.object() {
                let extractor = ConstantExtractor::new(quad.object().value());
                let term_generator: Box<dyn TermGenerator> = Box::new(NamedNodeGenerator::new(
                    Box::new(extractor),
                    base_iri.clone(),
                    self.strict_mode.into(),
                ));
                predicate_maps.push(PredicateMap { term_generator });
            }
        }

        // Also check rr: namespace for R2RML compatibility
        let rr_pred_map_pred = NamedNode::new(format!("{}predicateMap", RR))
            .map_err(RmlError::Parse)?;
        let rr_quads = store.get_quads(Some(pom_node), Some(&rr_pred_map_pred), None, None)?;
        for quad in rr_quads {
            let pred_map_node = quad.object();
            let term_generator = self.create_term_generator(
                store,
                pred_map_node,
                &TermType::Iri,
                base_iri,
                false,
            )?;
            predicate_maps.push(PredicateMap { term_generator });
        }

        let rr_pred_pred =
            NamedNode::new(format!("{}predicate", RR)).map_err(RmlError::Parse)?;
        let rr_pred_quads = store.get_quads(Some(pom_node), Some(&rr_pred_pred), None, None)?;
        for quad in rr_pred_quads {
            if let TermRef::NamedNode(_) = quad.object() {
                let extractor = ConstantExtractor::new(quad.object().value());
                let term_generator: Box<dyn TermGenerator> = Box::new(NamedNodeGenerator::new(
                    Box::new(extractor),
                    base_iri.clone(),
                    self.strict_mode.into(),
                ));
                predicate_maps.push(PredicateMap { term_generator });
            }
        }

        if predicate_maps.is_empty() {
            return Err(RmlError::Mapping(format!(
                "No predicate maps found for predicate-object map {}",
                pom_node
            )));
        }

        Ok(predicate_maps)
    }

    /// Parses object maps
    fn parse_object_maps(
        &self,
        pom_node: &TermRef,
        store: &impl QuadStore,
        base_iri: &Option<String>,
    ) -> Result<Vec<ObjectMap>> {
        let mut object_maps = Vec::new();

        // Try rr:objectMap
        let obj_map_pred =
            NamedNode::new(format!("{}objectMap", RML2)).map_err(RmlError::Parse)?;

        let quads = store.get_quads(Some(pom_node), Some(&obj_map_pred), None, None)?;
        for quad in quads {
            let obj_map_node = quad.object();
            let object_map = self.parse_object_map(obj_map_node, store, base_iri)?;
            object_maps.push(object_map);
        }

        // Try rr:object shortcut
        let obj_pred =
            NamedNode::new(format!("{}object", RML2)).map_err(RmlError::Parse)?;

        let obj_quads = store.get_quads(Some(pom_node), Some(&obj_pred), None, None)?;
        for quad in obj_quads {
            let extractor = ConstantExtractor::new(quad.object().value());
            let term_generator: Box<dyn TermGenerator> = match quad.object() {
                TermRef::NamedNode(_) => Box::new(NamedNodeGenerator::new(
                    Box::new(extractor),
                    base_iri.clone(),
                    self.strict_mode.into(),
                )),
                TermRef::Literal(_) => Box::new(LiteralGenerator::new(Box::new(extractor), None, None)),
                TermRef::BlankNode(_) => Box::new(BlankNodeGenerator::new(Some(Box::new(extractor)))),
            };
            object_maps.push(ObjectMap::TermMap { term_generator });
        }

        // Also check rr: namespace for R2RML compatibility
        let rr_obj_map_pred =
            NamedNode::new(format!("{}objectMap", RR)).map_err(RmlError::Parse)?;
        let rr_quads = store.get_quads(Some(pom_node), Some(&rr_obj_map_pred), None, None)?;
        for quad in rr_quads {
            let obj_map_node = quad.object();
            let object_map = self.parse_object_map(obj_map_node, store, base_iri)?;
            object_maps.push(object_map);
        }

        let rr_obj_pred =
            NamedNode::new(format!("{}object", RR)).map_err(RmlError::Parse)?;
        let rr_obj_quads = store.get_quads(Some(pom_node), Some(&rr_obj_pred), None, None)?;
        for quad in rr_obj_quads {
            let extractor = ConstantExtractor::new(quad.object().value());
            let term_generator: Box<dyn TermGenerator> = match quad.object() {
                TermRef::NamedNode(_) => Box::new(NamedNodeGenerator::new(
                    Box::new(extractor),
                    base_iri.clone(),
                    self.strict_mode.into(),
                )),
                TermRef::Literal(_) => Box::new(LiteralGenerator::new(Box::new(extractor), None, None)),
                TermRef::BlankNode(_) => Box::new(BlankNodeGenerator::new(Some(Box::new(extractor)))),
            };
            object_maps.push(ObjectMap::TermMap { term_generator });
        }

        if object_maps.is_empty() {
            return Err(RmlError::Mapping(format!(
                "No object maps found for predicate-object map {}",
                pom_node
            )));
        }

        Ok(object_maps)
    }

    /// Parses a single object map
    fn parse_object_map(
        &self,
        obj_map_node: &TermRef,
        store: &impl QuadStore,
        base_iri: &Option<String>,
    ) -> Result<ObjectMap> {
        // Check if this is a referencing object map
        let _parent_tm_pred = NamedNode::new(format!("{}parentTriplesMap", RML2))
            .map_err(RmlError::Parse)?;

        if let Some(parent_tm) =
            get_single_object(store, obj_map_node, &[&format!("{}parentTriplesMap", RML2)])?
        {
            // Parse join conditions
            let join_conditions = self.parse_join_conditions(obj_map_node, store)?;

            return Ok(ObjectMap::RefObjectMap {
                parent_triples_map: parent_tm,
                join_conditions,
            });
        }

        // Also check rr:parentTriplesMap for R2RML compatibility
        if let Some(parent_tm) =
            get_single_object(store, obj_map_node, &[&format!("{}parentTriplesMap", RR)])?
        {
            let join_conditions = self.parse_join_conditions(obj_map_node, store)?;

            return Ok(ObjectMap::RefObjectMap {
                parent_triples_map: parent_tm,
                join_conditions,
            });
        }

        // Regular term map
        // For object maps, default to Literal if no termType is specified
        let term_type = get_term_type_for_object_map(store, obj_map_node)?;
        let term_generator =
            self.create_term_generator(store, obj_map_node, &term_type, base_iri, false)?;

        Ok(ObjectMap::TermMap { term_generator })
    }

    /// Parses join conditions
    fn parse_join_conditions(
        &self,
        obj_map_node: &TermRef,
        store: &impl QuadStore,
    ) -> Result<Vec<JoinCondition>> {
        let join_cond_pred = NamedNode::new(format!("{}joinCondition", RML2))
            .map_err(RmlError::Parse)?;

        let quads = store.get_quads(Some(obj_map_node), Some(&join_cond_pred), None, None)?;

        let mut join_conditions = Vec::new();
        for quad in quads {
            let jc_node = quad.object();

            let child = get_single_object(store, jc_node, &[&format!("{}child", RML2)])?
                .ok_or_else(|| {
                    RmlError::Mapping(format!("No child found in join condition {}", jc_node))
                })?
                .value()
                .to_string();

            let parent = get_single_object(store, jc_node, &[&format!("{}parent", RML2)])?
                .ok_or_else(|| {
                    RmlError::Mapping(format!("No parent found in join condition {}", jc_node))
                })?
                .value()
                .to_string();

            join_conditions.push(JoinCondition { child, parent });
        }

        // Also check rr: namespace for R2RML compatibility
        let rr_join_cond_pred = NamedNode::new(format!("{}joinCondition", RR))
            .map_err(RmlError::Parse)?;
        let rr_quads = store.get_quads(Some(obj_map_node), Some(&rr_join_cond_pred), None, None)?;
        for quad in rr_quads {
            let jc_node = quad.object();

            let child = get_single_object(store, jc_node, &[&format!("{}child", RR)])?
                .ok_or_else(|| {
                    RmlError::Mapping(format!("No child found in join condition {}", jc_node))
                })?
                .value()
                .to_string();

            let parent = get_single_object(store, jc_node, &[&format!("{}parent", RR)])?
                .ok_or_else(|| {
                    RmlError::Mapping(format!("No parent found in join condition {}", jc_node))
                })?
                .value()
                .to_string();

            join_conditions.push(JoinCondition { child, parent });
        }

        Ok(join_conditions)
    }

    /// Parses graph maps
    fn parse_graph_maps(
        &self,
        node: &TermRef,
        store: &impl QuadStore,
        base_iri: &Option<String>,
    ) -> Result<Vec<GraphMap>> {
        let mut graph_maps = Vec::new();

        // Try rr:graphMap
        let graph_map_pred =
            NamedNode::new(format!("{}graphMap", RML2)).map_err(RmlError::Parse)?;

        let quads = store.get_quads(Some(node), Some(&graph_map_pred), None, None)?;
        for quad in quads {
            let graph_map_node = quad.object();
            let term_generator = self.create_term_generator(
                store,
                graph_map_node,
                &TermType::Iri,
                base_iri,
                false,
            )?;
            graph_maps.push(GraphMap { term_generator });
        }

        // Try rr:graph shortcut
        let graph_pred =
            NamedNode::new(format!("{}graph", RML2)).map_err(RmlError::Parse)?;

        let graph_quads = store.get_quads(Some(node), Some(&graph_pred), None, None)?;
        for quad in graph_quads {
            if let TermRef::NamedNode(_) = quad.object() {
                let extractor = ConstantExtractor::new(quad.object().value());
                let term_generator: Box<dyn TermGenerator> = Box::new(NamedNodeGenerator::new(
                    Box::new(extractor),
                    base_iri.clone(),
                    self.strict_mode.into(),
                ));
                graph_maps.push(GraphMap { term_generator });
            }
        }

        // Also check rr: namespace for R2RML compatibility
        let rr_graph_map_pred =
            NamedNode::new(format!("{}graphMap", RR)).map_err(RmlError::Parse)?;
        let rr_quads = store.get_quads(Some(node), Some(&rr_graph_map_pred), None, None)?;
        for quad in rr_quads {
            let graph_map_node = quad.object();
            let term_generator = self.create_term_generator(
                store,
                graph_map_node,
                &TermType::Iri,
                base_iri,
                false,
            )?;
            graph_maps.push(GraphMap { term_generator });
        }

        let rr_graph_pred =
            NamedNode::new(format!("{}graph", RR)).map_err(RmlError::Parse)?;
        let rr_graph_quads = store.get_quads(Some(node), Some(&rr_graph_pred), None, None)?;
        for quad in rr_graph_quads {
            if let TermRef::NamedNode(_) = quad.object() {
                let extractor = ConstantExtractor::new(quad.object().value());
                let term_generator: Box<dyn TermGenerator> = Box::new(NamedNodeGenerator::new(
                    Box::new(extractor),
                    base_iri.clone(),
                    self.strict_mode.into(),
                ));
                graph_maps.push(GraphMap { term_generator });
            }
        }

        Ok(graph_maps)
    }

    /// Creates a TermGenerator from a term map
    fn create_term_generator(
        &self,
        store: &impl QuadStore,
        term_map_node: &TermRef,
        term_type: &TermType,
        base_iri: &Option<String>,
        is_subject: bool,
    ) -> Result<Box<dyn TermGenerator>> {
        // Check for fnml:functionValue (function-based term generation)
        if let Some(_func_value) = get_single_object(
            store,
            term_map_node,
            &[&format!("{}functionValue", FNML)],
        )? {
            // TODO: Implement function-based term generation
            return Err(RmlError::Mapping(
                "Function-based term generation not yet implemented".to_string(),
            ));
        }

        // Try rr:constant
        if let Some(constant) =
            get_single_object(store, term_map_node, &[&format!("{}constant", RML2)])?
        {
            return self.create_constant_generator(constant, term_type, base_iri);
        }

        // Also check rr:constant for R2RML compatibility
        if let Some(constant) =
            get_single_object(store, term_map_node, &[&format!("{}constant", RR)])?
        {
            return self.create_constant_generator(constant, term_type, base_iri);
        }

        // Try rr:template
        if let Some(template) =
            get_single_object(store, term_map_node, &[&format!("{}template", RML2)])?
        {
            return self.create_template_generator(
                template.value(),
                term_type,
                term_map_node,
                store,
                base_iri,
            );
        }

        // Also check rr:template for R2RML compatibility
        if let Some(template) =
            get_single_object(store, term_map_node, &[&format!("{}template", RR)])?
        {
            return self.create_template_generator(
                template.value(),
                term_type,
                term_map_node,
                store,
                base_iri,
            );
        }

        // Try rr:reference
        if let Some(reference) =
            get_single_object(store, term_map_node, &[&format!("{}reference", RML2)])?
        {
            return self.create_reference_generator(
                reference.value(),
                term_type,
                term_map_node,
                store,
                base_iri,
            );
        }

        // Also check rr:reference for R2RML compatibility
        if let Some(reference) =
            get_single_object(store, term_map_node, &[&format!("{}reference", RR)])?
        {
            return self.create_reference_generator(
                reference.value(),
                term_type,
                term_map_node,
                store,
                base_iri,
            );
        }

        // If this is a subject and no explicit term map is found, it might be a constant IRI
        if is_subject && term_map_node.is_iri() {
            return self.create_constant_generator(term_map_node.clone(), term_type, base_iri);
        }

        Err(RmlError::Mapping(format!(
            "No valid term map found for node {}",
            term_map_node
        )))
    }

    /// Creates a constant-based term generator
    fn create_constant_generator(
        &self,
        constant: TermRef,
        term_type: &TermType,
        base_iri: &Option<String>,
    ) -> Result<Box<dyn TermGenerator>> {
        let extractor = ConstantExtractor::new(constant.value());

        match term_type {
            TermType::Iri => Ok(Box::new(NamedNodeGenerator::new(
                Box::new(extractor),
                base_iri.clone(),
                self.strict_mode.into(),
            ))),
            TermType::Literal => {
                // Check if the constant is already a literal with language or datatype
                if let TermRef::Literal(lit) = constant {
                    let language = lit.language().map(|s| s.to_string());
                    let datatype = lit.datatype().map(|s| s.to_string());
                    let lang_extractor = language.map(|l| {
                        Box::new(ConstantExtractor::new(l)) as Box<dyn ValueExtractor>
                    });
                    Ok(Box::new(LiteralGenerator::new(
                        Box::new(extractor),
                        lang_extractor,
                        datatype,
                    )))
                } else {
                    Ok(Box::new(LiteralGenerator::new(
                        Box::new(extractor),
                        None,
                        None,
                    )))
                }
            }
            TermType::BlankNode => Ok(Box::new(BlankNodeGenerator::new(Some(Box::new(
                extractor,
            ))))),
        }
    }

    /// Creates a template-based term generator
    fn create_template_generator(
        &self,
        template: &str,
        term_type: &TermType,
        term_map_node: &TermRef,
        store: &impl QuadStore,
        base_iri: &Option<String>,
    ) -> Result<Box<dyn TermGenerator>> {
        let extractor = TemplateExtractor::new(template)?;

        match term_type {
            TermType::Iri => Ok(Box::new(NamedNodeGenerator::new(
                Box::new(extractor),
                base_iri.clone(),
                self.strict_mode.into(),
            ))),
            TermType::Literal => {
                let (language_extractor, datatype) =
                    self.get_literal_modifiers(term_map_node, store)?;
                Ok(Box::new(LiteralGenerator::new(
                    Box::new(extractor),
                    language_extractor,
                    datatype,
                )))
            }
            TermType::BlankNode => Ok(Box::new(BlankNodeGenerator::new(Some(Box::new(
                extractor,
            ))))),
        }
    }

    /// Creates a reference-based term generator
    fn create_reference_generator(
        &self,
        reference: &str,
        term_type: &TermType,
        term_map_node: &TermRef,
        store: &impl QuadStore,
        base_iri: &Option<String>,
    ) -> Result<Box<dyn TermGenerator>> {
        let extractor = ReferenceExtractor::new(reference);

        match term_type {
            TermType::Iri => Ok(Box::new(NamedNodeGenerator::new(
                Box::new(extractor),
                base_iri.clone(),
                self.strict_mode.into(),
            ))),
            TermType::Literal => {
                let (language_extractor, datatype) =
                    self.get_literal_modifiers(term_map_node, store)?;
                Ok(Box::new(LiteralGenerator::new(
                    Box::new(extractor),
                    language_extractor,
                    datatype,
                )))
            }
            TermType::BlankNode => Ok(Box::new(BlankNodeGenerator::new(Some(Box::new(
                extractor,
            ))))),
        }
    }

    /// Gets language and datatype modifiers for literal generators
    fn get_literal_modifiers(
        &self,
        term_map_node: &TermRef,
        store: &impl QuadStore,
    ) -> Result<(Option<Box<dyn ValueExtractor>>, Option<String>)> {
        // Check for language tag
        let language_extractor =
            if let Some(lang) = get_single_object(store, term_map_node, &[&format!("{}language", RML2)])? {
                Some(Box::new(ConstantExtractor::new(lang.value())) as Box<dyn ValueExtractor>)
            } else { get_single_object(store, term_map_node, &[&format!("{}language", RR)])?.map(|lang| Box::new(ConstantExtractor::new(lang.value())) as Box<dyn ValueExtractor>) };

        // Check for datatype
        let datatype =
            if let Some(dt) = get_single_object(store, term_map_node, &[&format!("{}datatype", RML2)])? {
                Some(dt.value().to_string())
            } else { get_single_object(store, term_map_node, &[&format!("{}datatype", RR)])?.map(|dt| dt.value().to_string()) };

        Ok((language_extractor, datatype))
    }
}

/// Term type enumeration
#[derive(Debug, Clone, PartialEq, Eq)]
enum TermType {
    Iri,
    Literal,
    BlankNode,
}

/// Gets the term type from a term map
fn get_term_type(store: &impl QuadStore, term_map_node: &TermRef) -> Result<TermType> {
    let term_type_pred =
        NamedNode::new(format!("{}termType", RML2)).map_err(RmlError::Parse)?;

    if let Some(term_type_node) =
        store.get_quad(Some(term_map_node), Some(&term_type_pred), None, None)?
    {
        let term_type_iri = term_type_node.object().value();

        if term_type_iri.ends_with("IRI") || term_type_iri.ends_with("iri") {
            return Ok(TermType::Iri);
        } else if term_type_iri.ends_with("Literal") {
            return Ok(TermType::Literal);
        } else if term_type_iri.ends_with("BlankNode") {
            return Ok(TermType::BlankNode);
        }
    }

    // Also check rr:termType for R2RML compatibility
    let rr_term_type_pred =
        NamedNode::new(format!("{}termType", RR)).map_err(RmlError::Parse)?;

    if let Some(term_type_node) =
        store.get_quad(Some(term_map_node), Some(&rr_term_type_pred), None, None)?
    {
        let term_type_iri = term_type_node.object().value();

        if term_type_iri.ends_with("IRI") || term_type_iri.ends_with("iri") {
            return Ok(TermType::Iri);
        } else if term_type_iri.ends_with("Literal") {
            return Ok(TermType::Literal);
        } else if term_type_iri.ends_with("BlankNode") {
            return Ok(TermType::BlankNode);
        }
    }

    // Default to IRI if no term type is specified
    Ok(TermType::Iri)
}

/// Gets the term type for an object map
/// According to R2RML spec, object maps default to Literal if no termType is specified
fn get_term_type_for_object_map(store: &impl QuadStore, term_map_node: &TermRef) -> Result<TermType> {
    let term_type_pred =
        NamedNode::new(format!("{}termType", RML2)).map_err(RmlError::Parse)?;

    if let Some(term_type_node) =
        store.get_quad(Some(term_map_node), Some(&term_type_pred), None, None)?
    {
        let term_type_iri = term_type_node.object().value();

        if term_type_iri.ends_with("IRI") || term_type_iri.ends_with("iri") {
            return Ok(TermType::Iri);
        } else if term_type_iri.ends_with("Literal") {
            return Ok(TermType::Literal);
        } else if term_type_iri.ends_with("BlankNode") {
            return Ok(TermType::BlankNode);
        }
    }

    // Also check rr:termType for R2RML compatibility
    let rr_term_type_pred =
        NamedNode::new(format!("{}termType", RR)).map_err(RmlError::Parse)?;

    if let Some(term_type_node) =
        store.get_quad(Some(term_map_node), Some(&rr_term_type_pred), None, None)?
    {
        let term_type_iri = term_type_node.object().value();

        if term_type_iri.ends_with("IRI") || term_type_iri.ends_with("iri") {
            return Ok(TermType::Iri);
        } else if term_type_iri.ends_with("Literal") {
            return Ok(TermType::Literal);
        } else if term_type_iri.ends_with("BlankNode") {
            return Ok(TermType::BlankNode);
        }
    }

    // Default to Literal for object maps (per R2RML spec)
    Ok(TermType::Literal)
}

/// Gets all objects for a subject-predicate pair
fn get_objects(
    store: &impl QuadStore,
    subject: &TermRef,
    predicates: &[&str],
) -> Result<Vec<TermRef>> {
    let mut objects = Vec::new();

    for pred_iri in predicates {
        let predicate = NamedNode::new(*pred_iri).map_err(RmlError::Parse)?;
        let quads = store.get_quads(Some(subject), Some(&predicate), None, None)?;

        for quad in quads {
            objects.push(quad.object().clone());
        }
    }

    Ok(objects)
}

/// Gets a single object for a subject-predicate pair
fn get_single_object(
    store: &impl QuadStore,
    subject: &TermRef,
    predicates: &[&str],
) -> Result<Option<TermRef>> {
    let objects = get_objects(store, subject, predicates)?;

    if objects.is_empty() {
        Ok(None)
    } else if objects.len() == 1 {
        Ok(Some(objects[0].clone()))
    } else {
        Err(RmlError::Mapping(format!(
            "Expected single object for subject {}, but found {}",
            subject,
            objects.len()
        )))
    }
}

impl From<StrictMode> for crate::termgenerator::StrictMode {
    fn from(mode: StrictMode) -> Self {
        match mode {
            StrictMode::Strict => crate::termgenerator::StrictMode::Strict,
            StrictMode::BestEffort => crate::termgenerator::StrictMode::BestEffort,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespaces::QL;
    use crate::store::InMemoryQuadStore;
    use crate::term::Literal;

    fn create_simple_mapping_store() -> InMemoryQuadStore {
        let mut store = InMemoryQuadStore::new();

        // Create a simple triples map
        let tm = NamedNode::new("http://example.org/map1").unwrap();

        // Logical source
        let ls = NamedNode::new("http://example.org/ls1").unwrap();
        store
            .add_quad(
                tm.clone().into(),
                NamedNode::new(format!("{}logicalSource", RML2)).unwrap(),
                ls.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                ls.clone().into(),
                NamedNode::new(format!("{}source", RML2)).unwrap(),
                Literal::new("data.csv").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                ls.into(),
                NamedNode::new(format!("{}referenceFormulation", RML2)).unwrap(),
                NamedNode::new(format!("{}CSV", QL)).unwrap().into(),
                None,
            )
            .unwrap();

        // Subject map with template
        let sm = NamedNode::new("http://example.org/sm1").unwrap();
        store
            .add_quad(
                tm.clone().into(),
                NamedNode::new(format!("{}subjectMap", RML2)).unwrap(),
                sm.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                sm.clone().into(),
                NamedNode::new(format!("{}template", RML2)).unwrap(),
                Literal::new("http://example.org/person/{{id}}").into(),
                None,
            )
            .unwrap();

        // Add a class
        store
            .add_quad(
                sm.into(),
                NamedNode::new(format!("{}class", RML2)).unwrap(),
                NamedNode::new("http://xmlns.com/foaf/0.1/Person")
                    .unwrap()
                    .into(),
                None,
            )
            .unwrap();

        // Predicate-object map
        let pom = NamedNode::new("http://example.org/pom1").unwrap();
        store
            .add_quad(
                tm.into(),
                NamedNode::new(format!("{}predicateObjectMap", RML2)).unwrap(),
                pom.clone().into(),
                None,
            )
            .unwrap();

        // Predicate
        store
            .add_quad(
                pom.clone().into(),
                NamedNode::new(format!("{}predicate", RML2)).unwrap(),
                NamedNode::new("http://xmlns.com/foaf/0.1/name")
                    .unwrap()
                    .into(),
                None,
            )
            .unwrap();

        // Object map with reference
        let om = NamedNode::new("http://example.org/om1").unwrap();
        store
            .add_quad(
                pom.into(),
                NamedNode::new(format!("{}objectMap", RML2)).unwrap(),
                om.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                om.into(),
                NamedNode::new(format!("{}reference", RML2)).unwrap(),
                Literal::new("name").into(),
                None,
            )
            .unwrap();

        store
    }

    #[test]
    fn test_find_triples_maps() {
        let store = create_simple_mapping_store();
        let factory = MappingFactory::new(None, StrictMode::Strict);

        let triples_maps = factory.find_triples_maps(&store).unwrap();
        assert_eq!(triples_maps.len(), 1);
    }

    #[test]
    fn test_parse_logical_source() {
        let store = create_simple_mapping_store();
        let factory = MappingFactory::new(None, StrictMode::Strict);

        let tm = TermRef::NamedNode(NamedNode::new("http://example.org/map1").unwrap());
        let ls = factory.parse_logical_source(&tm, &store).unwrap();

        assert_eq!(ls.source.value(), "data.csv");
        assert!(ls.reference_formulation.contains("CSV"));
    }

    #[test]
    fn test_create_mapping() {
        let store = create_simple_mapping_store();
        let factory = MappingFactory::new(
            Some("http://example.org/".to_string()),
            StrictMode::Strict,
        );

        let mapping = factory.create_mapping(&store).unwrap();
        assert_eq!(mapping.len(), 1);

        let tm = &mapping.triples_maps[0];
        assert_eq!(tm.subject_map.classes.len(), 1);
        assert_eq!(tm.predicate_object_maps.len(), 1);
    }

    #[test]
    fn test_get_term_type_default() {
        let store = InMemoryQuadStore::new();
        let node = TermRef::NamedNode(NamedNode::new("http://example.org/node").unwrap());

        let term_type = get_term_type(&store, &node).unwrap();
        assert_eq!(term_type, TermType::Iri);
    }

    #[test]
    fn test_get_single_object() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/subject").unwrap();
        let predicate = NamedNode::new("http://example.org/predicate").unwrap();
        let object = Literal::new("value");

        store
            .add_quad(
                subject.clone().into(),
                predicate.clone(),
                object.into(),
                None,
            )
            .unwrap();

        let subject_ref = TermRef::NamedNode(subject);
        let result = get_single_object(
            &store,
            &subject_ref,
            &["http://example.org/predicate"],
        )
        .unwrap();

        assert!(result.is_some());
        assert_eq!(result.unwrap().value(), "value");
    }

    #[test]
    fn test_empty_mapping() {
        let store = InMemoryQuadStore::new();
        let factory = MappingFactory::new(None, StrictMode::Strict);

        let result = factory.create_mapping(&store);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_complete_mapping() {
        // Create a more complete mapping with multiple predicate-object maps
        let mut store = InMemoryQuadStore::new();

        let tm = NamedNode::new("http://example.org/map1").unwrap();

        // Logical source
        let ls = NamedNode::new("http://example.org/ls1").unwrap();
        store
            .add_quad(
                tm.clone().into(),
                NamedNode::new(format!("{}logicalSource", RML2)).unwrap(),
                ls.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                ls.clone().into(),
                NamedNode::new(format!("{}source", RML2)).unwrap(),
                Literal::new("data.json").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                ls.clone().into(),
                NamedNode::new(format!("{}referenceFormulation", RML2)).unwrap(),
                NamedNode::new(format!("{}JSONPath", QL)).unwrap().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                ls.into(),
                NamedNode::new(format!("{}iterator", RML2)).unwrap(),
                Literal::new("$.persons[*]").into(),
                None,
            )
            .unwrap();

        // Subject map
        let sm = NamedNode::new("http://example.org/sm1").unwrap();
        store
            .add_quad(
                tm.clone().into(),
                NamedNode::new(format!("{}subjectMap", RML2)).unwrap(),
                sm.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                sm.clone().into(),
                NamedNode::new(format!("{}template", RML2)).unwrap(),
                Literal::new("http://example.org/person/{{id}}").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                sm.into(),
                NamedNode::new(format!("{}class", RML2)).unwrap(),
                NamedNode::new("http://xmlns.com/foaf/0.1/Person")
                    .unwrap()
                    .into(),
                None,
            )
            .unwrap();

        // First predicate-object map (name)
        let pom1 = NamedNode::new("http://example.org/pom1").unwrap();
        store
            .add_quad(
                tm.clone().into(),
                NamedNode::new(format!("{}predicateObjectMap", RML2)).unwrap(),
                pom1.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                pom1.clone().into(),
                NamedNode::new(format!("{}predicate", RML2)).unwrap(),
                NamedNode::new("http://xmlns.com/foaf/0.1/name")
                    .unwrap()
                    .into(),
                None,
            )
            .unwrap();

        let om1 = NamedNode::new("http://example.org/om1").unwrap();
        store
            .add_quad(
                pom1.into(),
                NamedNode::new(format!("{}objectMap", RML2)).unwrap(),
                om1.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                om1.into(),
                NamedNode::new(format!("{}reference", RML2)).unwrap(),
                Literal::new("name").into(),
                None,
            )
            .unwrap();

        // Second predicate-object map (age with datatype)
        let pom2 = NamedNode::new("http://example.org/pom2").unwrap();
        store
            .add_quad(
                tm.into(),
                NamedNode::new(format!("{}predicateObjectMap", RML2)).unwrap(),
                pom2.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                pom2.clone().into(),
                NamedNode::new(format!("{}predicate", RML2)).unwrap(),
                NamedNode::new("http://xmlns.com/foaf/0.1/age")
                    .unwrap()
                    .into(),
                None,
            )
            .unwrap();

        let om2 = NamedNode::new("http://example.org/om2").unwrap();
        store
            .add_quad(
                pom2.into(),
                NamedNode::new(format!("{}objectMap", RML2)).unwrap(),
                om2.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                om2.clone().into(),
                NamedNode::new(format!("{}reference", RML2)).unwrap(),
                Literal::new("age").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                om2.clone().into(),
                NamedNode::new(format!("{}datatype", RML2)).unwrap(),
                NamedNode::new("http://www.w3.org/2001/XMLSchema#integer")
                    .unwrap()
                    .into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                om2.into(),
                NamedNode::new(format!("{}termType", RML2)).unwrap(),
                NamedNode::new(format!("{}Literal", RML2)).unwrap().into(),
                None,
            )
            .unwrap();

        // Parse the mapping
        let factory = MappingFactory::new(
            Some("http://example.org/".to_string()),
            StrictMode::Strict,
        );

        let mapping = factory.create_mapping(&store).unwrap();
        assert_eq!(mapping.len(), 1);

        let tm = &mapping.triples_maps[0];
        assert_eq!(tm.subject_map.classes.len(), 1);
        assert_eq!(tm.predicate_object_maps.len(), 2);

        // Check logical source
        assert_eq!(tm.logical_source.source.value(), "data.json");
        assert!(tm.logical_source.reference_formulation.contains("JSONPath"));
        assert_eq!(
            tm.logical_source.iterator.as_ref().unwrap(),
            "$.persons[*]"
        );
    }

    #[test]
    fn test_r2rml_compatibility() {
        // Test that we can parse R2RML mappings (using rr: namespace)
        let mut store = InMemoryQuadStore::new();

        let tm = NamedNode::new("http://example.org/map1").unwrap();

        // Logical source with rr: namespace
        let ls = NamedNode::new("http://example.org/ls1").unwrap();
        store
            .add_quad(
                tm.clone().into(),
                NamedNode::new(format!("{}logicalSource", RR)).unwrap(),
                ls.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                ls.clone().into(),
                NamedNode::new(format!("{}source", RR)).unwrap(),
                Literal::new("data.csv").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                ls.into(),
                NamedNode::new(format!("{}referenceFormulation", RR)).unwrap(),
                NamedNode::new(format!("{}CSV", QL)).unwrap().into(),
                None,
            )
            .unwrap();

        // Subject with rr:subject shortcut
        store
            .add_quad(
                tm.clone().into(),
                NamedNode::new(format!("{}subject", RR)).unwrap(),
                Literal::new("http://example.org/person/1").into(),
                None,
            )
            .unwrap();

        // Predicate-object map
        let pom = NamedNode::new("http://example.org/pom1").unwrap();
        store
            .add_quad(
                tm.into(),
                NamedNode::new(format!("{}predicateObjectMap", RR)).unwrap(),
                pom.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                pom.clone().into(),
                NamedNode::new(format!("{}predicate", RR)).unwrap(),
                NamedNode::new("http://xmlns.com/foaf/0.1/name")
                    .unwrap()
                    .into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                pom.into(),
                NamedNode::new(format!("{}object", RR)).unwrap(),
                Literal::new("John Doe").into(),
                None,
            )
            .unwrap();

        // Parse the mapping
        let factory = MappingFactory::new(None, StrictMode::Strict);
        let mapping = factory.create_mapping(&store).unwrap();

        assert_eq!(mapping.len(), 1);
        assert_eq!(mapping.triples_maps[0].predicate_object_maps.len(), 1);
    }

    #[test]
    fn test_referencing_object_map() {
        // Test parsing of referencing object maps (joins)
        let mut store = InMemoryQuadStore::new();

        // First triples map (Person)
        let tm1 = NamedNode::new("http://example.org/PersonMap").unwrap();
        let ls1 = NamedNode::new("http://example.org/ls1").unwrap();

        store
            .add_quad(
                tm1.clone().into(),
                NamedNode::new(format!("{}logicalSource", RML2)).unwrap(),
                ls1.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                ls1.clone().into(),
                NamedNode::new(format!("{}source", RML2)).unwrap(),
                Literal::new("persons.csv").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                ls1.into(),
                NamedNode::new(format!("{}referenceFormulation", RML2)).unwrap(),
                NamedNode::new(format!("{}CSV", QL)).unwrap().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                tm1.clone().into(),
                NamedNode::new(format!("{}subject", RML2)).unwrap(),
                Literal::new("http://example.org/person/{{id}}").into(),
                None,
            )
            .unwrap();

        // Predicate-object map with referencing object map
        let pom = NamedNode::new("http://example.org/pom1").unwrap();
        store
            .add_quad(
                tm1.into(),
                NamedNode::new(format!("{}predicateObjectMap", RML2)).unwrap(),
                pom.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                pom.clone().into(),
                NamedNode::new(format!("{}predicate", RML2)).unwrap(),
                NamedNode::new("http://xmlns.com/foaf/0.1/knows")
                    .unwrap()
                    .into(),
                None,
            )
            .unwrap();

        let om = NamedNode::new("http://example.org/om1").unwrap();
        store
            .add_quad(
                pom.into(),
                NamedNode::new(format!("{}objectMap", RML2)).unwrap(),
                om.clone().into(),
                None,
            )
            .unwrap();

        // Parent triples map reference
        store
            .add_quad(
                om.clone().into(),
                NamedNode::new(format!("{}parentTriplesMap", RML2)).unwrap(),
                NamedNode::new("http://example.org/PersonMap")
                    .unwrap()
                    .into(),
                None,
            )
            .unwrap();

        // Join condition
        let jc = NamedNode::new("http://example.org/jc1").unwrap();
        store
            .add_quad(
                om.into(),
                NamedNode::new(format!("{}joinCondition", RML2)).unwrap(),
                jc.clone().into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                jc.clone().into(),
                NamedNode::new(format!("{}child", RML2)).unwrap(),
                Literal::new("friend_id").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                jc.into(),
                NamedNode::new(format!("{}parent", RML2)).unwrap(),
                Literal::new("id").into(),
                None,
            )
            .unwrap();

        // Parse the mapping
        let factory = MappingFactory::new(None, StrictMode::Strict);
        let mapping = factory.create_mapping(&store).unwrap();

        assert_eq!(mapping.len(), 1);

        let pom = &mapping.triples_maps[0].predicate_object_maps[0];
        assert_eq!(pom.object_maps.len(), 1);

        match &pom.object_maps[0] {
            ObjectMap::RefObjectMap {
                parent_triples_map,
                join_conditions,
            } => {
                assert_eq!(
                    parent_triples_map.value(),
                    "http://example.org/PersonMap"
                );
                assert_eq!(join_conditions.len(), 1);
                assert_eq!(join_conditions[0].child, "friend_id");
                assert_eq!(join_conditions[0].parent, "id");
            }
            _ => panic!("Expected RefObjectMap"),
        }
    }
}
