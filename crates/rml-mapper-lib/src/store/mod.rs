//! RDF Quad Store
//!
//! This module provides storage and querying capabilities for RDF quads,
//! matching the Java RML Mapper's QuadStore architecture.
//!
//! # Architecture
//!
//! The module follows the Java QuadStore.java design:
//! - `QuadStore` trait: Common interface for all quad store implementations
//! - `InMemoryQuadStore`: HashSet-based in-memory implementation
//! - `RdfFormat`: Supported RDF serialization formats
//! - Namespace prefix management
//! - Pattern matching for quad queries
//! - RDF parsing and serialization via oxigraph
//!
//! # Examples
//!
//! ```
//! use rml_mapper::store::{QuadStore, InMemoryQuadStore};
//! use rml_mapper::term::{NamedNode, Literal};
//!
//! let mut store = InMemoryQuadStore::new();
//!
//! // Add a quad
//! let subject = NamedNode::new("http://example.org/person/1").unwrap();
//! let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
//! let object = Literal::new("John Doe");
//! store.add_quad(subject.into(), predicate, object.into(), None).unwrap();
//!
//! // Query quads
//! let quads = store.get_quads(None, None, None, None).unwrap();
//! assert_eq!(quads.len(), 1);
//! ```

use crate::error::{Result, RmlError};
use crate::term::{BlankNode, Literal, NamedNode, Quad, Term, TermRef};
use oxigraph::io::{RdfFormat as OxiFormat, RdfParser, RdfSerializer};
use oxigraph::model::{
    vocab::xsd, BlankNode as OxiBlankNode, Literal as OxiLiteral, NamedNode as OxiNamedNode,
    Quad as OxiQuad, Subject as OxiSubject, Term as OxiTerm,
};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};

/// RDF serialization formats
///
/// Supported formats for reading and writing RDF data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RdfFormat {
    /// Turtle format (.ttl)
    Turtle,
    /// N-Triples format (.nt)
    NTriples,
    /// N-Quads format (.nq)
    NQuads,
    /// TriG format (.trig)
    TriG,
    /// RDF/XML format (.rdf, .xml)
    RdfXml,
}

impl RdfFormat {
    /// Converts to oxigraph's RdfFormat
    fn to_oxigraph(&self) -> OxiFormat {
        match self {
            RdfFormat::Turtle => OxiFormat::Turtle,
            RdfFormat::NTriples => OxiFormat::NTriples,
            RdfFormat::NQuads => OxiFormat::NQuads,
            RdfFormat::TriG => OxiFormat::TriG,
            RdfFormat::RdfXml => OxiFormat::RdfXml,
        }
    }

    /// Detects format from file extension
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "ttl" => Some(RdfFormat::Turtle),
            "nt" => Some(RdfFormat::NTriples),
            "nq" => Some(RdfFormat::NQuads),
            "trig" => Some(RdfFormat::TriG),
            "rdf" | "xml" => Some(RdfFormat::RdfXml),
            _ => None,
        }
    }
}

/// Trait for RDF quad stores
///
/// This trait defines the interface that all quad store implementations must provide,
/// matching the Java RML Mapper's QuadStore interface.
pub trait QuadStore {
    /// Adds a quad to the store
    ///
    /// # Arguments
    ///
    /// * `subject` - The subject (NamedNode or BlankNode)
    /// * `predicate` - The predicate (NamedNode)
    /// * `object` - The object (any term type)
    /// * `graph` - Optional graph name (NamedNode)
    fn add_quad(
        &mut self,
        subject: TermRef,
        predicate: NamedNode,
        object: TermRef,
        graph: Option<NamedNode>,
    ) -> Result<()>;

    /// Adds multiple quads to the store
    ///
    /// # Arguments
    ///
    /// * `quads` - Vector of quads to add
    fn add_quads(&mut self, quads: Vec<Quad>) -> Result<()> {
        for quad in quads {
            self.add_quad(
                quad.subject().clone(),
                quad.predicate().clone(),
                quad.object().clone(),
                quad.graph().cloned(),
            )?;
        }
        Ok(())
    }

    /// Removes quads matching the given pattern
    ///
    /// # Arguments
    ///
    /// * `subject` - Subject pattern (None matches any)
    /// * `predicate` - Predicate pattern (None matches any)
    /// * `object` - Object pattern (None matches any)
    /// * `graph` - Graph pattern (None matches any)
    ///
    /// # Returns
    ///
    /// Number of quads removed
    fn remove_quads(
        &mut self,
        subject: Option<&TermRef>,
        predicate: Option<&NamedNode>,
        object: Option<&TermRef>,
        graph: Option<&NamedNode>,
    ) -> Result<usize>;

    /// Gets quads matching the given pattern
    ///
    /// # Arguments
    ///
    /// * `subject` - Subject pattern (None matches any)
    /// * `predicate` - Predicate pattern (None matches any)
    /// * `object` - Object pattern (None matches any)
    /// * `graph` - Graph pattern (None matches any)
    ///
    /// # Returns
    ///
    /// Vector of matching quads
    fn get_quads(
        &self,
        subject: Option<&TermRef>,
        predicate: Option<&NamedNode>,
        object: Option<&TermRef>,
        graph: Option<&NamedNode>,
    ) -> Result<Vec<Quad>>;

    /// Gets a single quad matching the given pattern
    ///
    /// # Returns
    ///
    /// The first matching quad, or None if no match found
    fn get_quad(
        &self,
        subject: Option<&TermRef>,
        predicate: Option<&NamedNode>,
        object: Option<&TermRef>,
        graph: Option<&NamedNode>,
    ) -> Result<Option<Quad>> {
        Ok(self.get_quads(subject, predicate, object, graph)?.into_iter().next())
    }

    /// Checks if the store contains a quad matching the pattern
    ///
    /// # Arguments
    ///
    /// * `subject` - Subject pattern (None matches any)
    /// * `predicate` - Predicate pattern (None matches any)
    /// * `object` - Object pattern (None matches any)
    /// * `graph` - Graph pattern (None matches any)
    fn contains(
        &self,
        subject: Option<&TermRef>,
        predicate: Option<&NamedNode>,
        object: Option<&TermRef>,
        graph: Option<&NamedNode>,
    ) -> Result<bool> {
        Ok(!self.get_quads(subject, predicate, object, graph)?.is_empty())
    }

    /// Returns true if the store is empty
    fn is_empty(&self) -> bool;

    /// Returns the number of quads in the store
    fn size(&self) -> usize;

    /// Reads RDF data from an input stream
    ///
    /// # Arguments
    ///
    /// * `input` - Input stream to read from
    /// * `base` - Base IRI for resolving relative IRIs
    /// * `format` - RDF format of the input
    fn read<R: Read>(&mut self, input: R, base: Option<&str>, format: RdfFormat) -> Result<()>;

    /// Writes RDF data to an output stream
    ///
    /// # Arguments
    ///
    /// * `output` - Output stream to write to
    /// * `format` - RDF format for serialization
    fn write<W: Write>(&self, output: W, format: RdfFormat) -> Result<()>;

    /// Copies namespace prefixes from another store
    ///
    /// # Arguments
    ///
    /// * `other` - Store to copy namespaces from
    fn copy_namespaces(&mut self, other: &InMemoryQuadStore);

    /// Adds a namespace prefix mapping
    ///
    /// # Arguments
    ///
    /// * `prefix` - Namespace prefix
    /// * `iri` - Namespace IRI
    fn add_namespace(&mut self, prefix: String, iri: String);

    /// Removes a namespace prefix mapping
    ///
    /// # Arguments
    ///
    /// * `prefix` - Namespace prefix to remove
    fn remove_namespace(&mut self, prefix: &str);

    /// Gets all namespace prefix mappings
    fn get_namespaces(&self) -> &HashMap<String, String>;

    /// Removes duplicate quads from the store
    ///
    /// # Returns
    ///
    /// Number of duplicates removed
    fn remove_duplicates(&mut self) -> Result<usize>;

    /// Checks if this store is isomorphic to another store
    ///
    /// Two stores are isomorphic if they contain the same quads,
    /// possibly with different blank node labels.
    ///
    /// # Arguments
    ///
    /// * `other` - Store to compare with
    fn is_isomorphic(&self, other: &InMemoryQuadStore) -> Result<bool>;

    /// Checks if this store is a subset of another store
    ///
    /// # Arguments
    ///
    /// * `other` - Store to compare with
    fn is_subset(&self, other: &InMemoryQuadStore) -> Result<bool>;

    /// Gets all unique subjects in the store
    fn get_subjects(&self) -> Result<Vec<TermRef>>;

    /// Tries to translate a property using namespace prefixes
    ///
    /// # Arguments
    ///
    /// * `property` - Property IRI to translate
    ///
    /// # Returns
    ///
    /// Prefixed form if a matching namespace is found, otherwise the original IRI
    fn try_property_translation(&self, property: &str) -> String;

    /// Renames all predicates matching a pattern
    ///
    /// # Arguments
    ///
    /// * `old_predicate` - Predicate to rename
    /// * `new_predicate` - New predicate name
    ///
    /// # Returns
    ///
    /// Number of quads modified
    fn rename_all_predicates(
        &mut self,
        old_predicate: &NamedNode,
        new_predicate: NamedNode,
    ) -> Result<usize>;

    /// Renames all objects matching a pattern
    ///
    /// # Arguments
    ///
    /// * `old_object` - Object to rename
    /// * `new_object` - New object value
    ///
    /// # Returns
    ///
    /// Number of quads modified
    fn rename_all_objects(&mut self, old_object: &TermRef, new_object: TermRef) -> Result<usize>;
}

/// In-memory RDF quad store
///
/// This implementation supports two modes:
/// - **Query mode** (HashSet): Used for mapping parsing where `get_quads` is needed.
/// - **Output mode** (Vec): Used for mapping execution output where speed matters.
///
/// The executor uses `add_quad_direct` which appends to an internal Vec, avoiding
/// the cost of hashing. Call `deduplicate()` explicitly if dedup is needed.
#[derive(Debug, Default)]
pub struct InMemoryQuadStore {
    /// Set of quads (used for query-heavy workloads like mapping parsing)
    quads: HashSet<Quad>,
    /// Fast append buffer (used for output-heavy workloads like execution)
    quads_vec: Vec<Quad>,
    /// Namespace prefix mappings (prefix -> IRI)
    namespaces: HashMap<String, String>,
}

impl InMemoryQuadStore {
    /// Creates a new empty in-memory quad store
    pub fn new() -> Self {
        Self {
            quads: HashSet::new(),
            quads_vec: Vec::new(),
            namespaces: HashMap::new(),
        }
    }

    /// Creates a new quad store with initial capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            quads: HashSet::with_capacity(capacity),
            quads_vec: Vec::new(),
            namespaces: HashMap::new(),
        }
    }

    /// Clears all quads from the store
    pub fn clear(&mut self) {
        self.quads.clear();
        self.quads_vec.clear();
    }

    /// Returns an iterator over all quads (both HashSet and Vec)
    pub fn iter(&self) -> impl Iterator<Item = &Quad> {
        self.quads.iter().chain(self.quads_vec.iter())
    }

    /// Adds a pre-constructed quad directly to the fast Vec buffer.
    /// No hashing, no validation. Used by the executor for output.
    pub fn add_quad_direct(&mut self, quad: Quad) {
        self.quads_vec.push(quad);
    }

    /// Reserves capacity in the Vec buffer for additional quads
    pub fn reserve(&mut self, additional: usize) {
        self.quads_vec.reserve(additional);
    }

    /// Removes duplicate quads from the Vec buffer.
    /// Uses a single batch HashSet construction (faster than per-insert hashing
    /// during execution because the HashSet is pre-sized and never rehashes).
    pub fn deduplicate_vec(&mut self) -> usize {
        if self.quads_vec.is_empty() {
            return 0;
        }
        let before = self.quads_vec.len();
        let mut set = HashSet::with_capacity(before);
        self.quads_vec.retain(|q| set.insert(q.clone()));
        before - self.quads_vec.len()
    }

    /// Checks if a quad matches the given pattern
    fn matches_pattern(
        quad: &Quad,
        subject: Option<&TermRef>,
        predicate: Option<&NamedNode>,
        object: Option<&TermRef>,
        graph: Option<&NamedNode>,
    ) -> bool {
        subject.is_none_or(|s| quad.subject() == s)
            && predicate.is_none_or(|p| quad.predicate() == p)
            && object.is_none_or(|o| quad.object() == o)
            && graph.is_none_or(|g| quad.graph() == Some(g))
    }
}

impl QuadStore for InMemoryQuadStore {
    fn add_quad(
        &mut self,
        subject: TermRef,
        predicate: NamedNode,
        object: TermRef,
        graph: Option<NamedNode>,
    ) -> Result<()> {
        let quad = Quad::new(subject, predicate, object, graph)
            .map_err(RmlError::Validation)?;
        self.quads.insert(quad);
        Ok(())
    }

    fn remove_quads(
        &mut self,
        subject: Option<&TermRef>,
        predicate: Option<&NamedNode>,
        object: Option<&TermRef>,
        graph: Option<&NamedNode>,
    ) -> Result<usize> {
        let to_remove: Vec<Quad> = self
            .quads
            .iter()
            .filter(|q| Self::matches_pattern(q, subject, predicate, object, graph))
            .cloned()
            .collect();

        let count = to_remove.len();
        for quad in to_remove {
            self.quads.remove(&quad);
        }

        Ok(count)
    }

    fn get_quads(
        &self,
        subject: Option<&TermRef>,
        predicate: Option<&NamedNode>,
        object: Option<&TermRef>,
        graph: Option<&NamedNode>,
    ) -> Result<Vec<Quad>> {
        Ok(self
            .iter()
            .filter(|q| Self::matches_pattern(q, subject, predicate, object, graph))
            .cloned()
            .collect())
    }

    fn is_empty(&self) -> bool {
        self.quads.is_empty() && self.quads_vec.is_empty()
    }

    fn size(&self) -> usize {
        self.quads.len() + self.quads_vec.len()
    }

    fn read<R: Read>(&mut self, input: R, base: Option<&str>, format: RdfFormat) -> Result<()> {
        // Create parser with optional base IRI
        let rdf_parser = RdfParser::from_format(format.to_oxigraph());
        
        let rdf_parser = if let Some(base_str) = base {
            rdf_parser.with_base_iri(base_str)
                .map_err(|e| RmlError::Parse(e.to_string()))?
        } else {
            rdf_parser
        };
        
        let parser = rdf_parser.for_reader(input);

        for result in parser {
            let oxi_quad = result.map_err(|e| RmlError::Parse(e.to_string()))?;
            let quad = convert_from_oxigraph_quad(oxi_quad)?;
            self.quads.insert(quad);
        }

        Ok(())
    }

    fn write<W: Write>(&self, output: W, format: RdfFormat) -> Result<()> {
        let mut serializer = RdfSerializer::from_format(format.to_oxigraph())
            .for_writer(output);

        for quad in self.iter() {
            let oxi_quad = convert_to_oxigraph_quad(quad)?;
            serializer
                .serialize_quad(&oxi_quad)
                .map_err(|e| RmlError::Serialization(e.to_string()))?;
        }

        serializer
            .finish()
            .map_err(|e| RmlError::Serialization(e.to_string()))?;

        Ok(())
    }

    fn copy_namespaces(&mut self, other: &InMemoryQuadStore) {
        for (prefix, iri) in other.get_namespaces() {
            self.namespaces.insert(prefix.clone(), iri.clone());
        }
    }

    fn add_namespace(&mut self, prefix: String, iri: String) {
        self.namespaces.insert(prefix, iri);
    }

    fn remove_namespace(&mut self, prefix: &str) {
        self.namespaces.remove(prefix);
    }

    fn get_namespaces(&self) -> &HashMap<String, String> {
        &self.namespaces
    }

    fn remove_duplicates(&mut self) -> Result<usize> {
        // Dedup the vec buffer if it has entries
        let removed = self.deduplicate_vec();
        // HashSet portion is already unique
        Ok(removed)
    }

    fn is_isomorphic(&self, other: &InMemoryQuadStore) -> Result<bool> {
        // Simple implementation: check if sizes match and all quads exist
        // A full isomorphism check would need to handle blank node renaming
        if self.size() != other.size() {
            return Ok(false);
        }

        // Get all quads from other store
        let other_quads = other.get_quads(None, None, None, None)?;

        // Check if all our quads exist in the other store
        for quad in self.iter() {
            if !other_quads.contains(quad) {
                return Ok(false);
            }
        }

        Ok(true)
    }

    fn is_subset(&self, other: &InMemoryQuadStore) -> Result<bool> {
        // Check if all our quads exist in the other store
        let other_quads = other.get_quads(None, None, None, None)?;

        for quad in self.iter() {
            if !other_quads.contains(quad) {
                return Ok(false);
            }
        }

        Ok(true)
    }

    fn get_subjects(&self) -> Result<Vec<TermRef>> {
        let mut subjects: HashSet<TermRef> = HashSet::new();
        for quad in self.iter() {
            subjects.insert(quad.subject().clone());
        }
        Ok(subjects.into_iter().collect())
    }

    fn try_property_translation(&self, property: &str) -> String {
        // Try to find a matching namespace prefix
        for (prefix, iri) in &self.namespaces {
            if property.starts_with(iri) {
                let local_name = &property[iri.len()..];
                return format!("{}:{}", prefix, local_name);
            }
        }
        // No matching prefix found, return original
        property.to_string()
    }

    fn rename_all_predicates(
        &mut self,
        old_predicate: &NamedNode,
        new_predicate: NamedNode,
    ) -> Result<usize> {
        let matching_quads: Vec<Quad> = self
            .quads
            .iter()
            .filter(|q| q.predicate() == old_predicate)
            .cloned()
            .collect();

        let count = matching_quads.len();

        // Remove old quads and add new ones
        for quad in matching_quads {
            self.quads.remove(&quad);
            let new_quad = Quad::new(
                quad.subject().clone(),
                new_predicate.clone(),
                quad.object().clone(),
                quad.graph().cloned(),
            )
            .map_err(RmlError::Validation)?;
            self.quads.insert(new_quad);
        }

        Ok(count)
    }

    fn rename_all_objects(&mut self, old_object: &TermRef, new_object: TermRef) -> Result<usize> {
        let matching_quads: Vec<Quad> = self
            .quads
            .iter()
            .filter(|q| q.object() == old_object)
            .cloned()
            .collect();

        let count = matching_quads.len();

        // Remove old quads and add new ones
        for quad in matching_quads {
            self.quads.remove(&quad);
            let new_quad = Quad::new(
                quad.subject().clone(),
                quad.predicate().clone(),
                new_object.clone(),
                quad.graph().cloned(),
            )
            .map_err(RmlError::Validation)?;
            self.quads.insert(new_quad);
        }

        Ok(count)
    }
}

// Conversion functions between our types and oxigraph types

/// Converts an oxigraph quad to our Quad type
fn convert_from_oxigraph_quad(oxi_quad: OxiQuad) -> Result<Quad> {
    let subject = convert_from_oxigraph_subject(oxi_quad.subject)?;
    let predicate = convert_from_oxigraph_named_node(oxi_quad.predicate)?;
    let object = convert_from_oxigraph_term(oxi_quad.object)?;
    let graph = match oxi_quad.graph_name {
        oxigraph::model::GraphName::NamedNode(n) => Some(convert_from_oxigraph_named_node(n)?),
        oxigraph::model::GraphName::BlankNode(_) => {
            return Err(RmlError::Parse(
                "Blank node graph names are not supported".to_string(),
            ))
        }
        oxigraph::model::GraphName::DefaultGraph => None,
    };

    Quad::new(subject, predicate, object, graph).map_err(RmlError::Validation)
}

/// Converts our Quad to an oxigraph quad
fn convert_to_oxigraph_quad(quad: &Quad) -> Result<OxiQuad> {
    let subject = convert_to_oxigraph_subject(quad.subject())?;
    let predicate = convert_to_oxigraph_named_node(quad.predicate())?;
    let object = convert_to_oxigraph_term(quad.object())?;
    let graph_name = match quad.graph() {
        Some(g) => {
            oxigraph::model::GraphName::NamedNode(convert_to_oxigraph_named_node(g)?)
        }
        None => oxigraph::model::GraphName::DefaultGraph,
    };

    Ok(OxiQuad {
        subject,
        predicate,
        object,
        graph_name,
    })
}

fn convert_from_oxigraph_subject(subject: OxiSubject) -> Result<TermRef> {
    match subject {
        OxiSubject::NamedNode(n) => Ok(TermRef::NamedNode(convert_from_oxigraph_named_node(n)?)),
        OxiSubject::BlankNode(b) => Ok(TermRef::BlankNode(BlankNode::new(b.as_str()))),
        OxiSubject::Triple(_) => Err(RmlError::Parse(
            "RDF-star triples as subjects are not supported".to_string(),
        )),
    }
}

fn convert_to_oxigraph_subject(subject: &TermRef) -> Result<OxiSubject> {
    match subject {
        TermRef::NamedNode(n) => Ok(OxiSubject::NamedNode(convert_to_oxigraph_named_node(n)?)),
        TermRef::BlankNode(b) => Ok(OxiSubject::BlankNode(
            OxiBlankNode::new_unchecked(b.id()),
        )),
        TermRef::Literal(_) => Err(RmlError::Validation(
            "Literal cannot be used as subject".to_string(),
        )),
    }
}

fn convert_from_oxigraph_named_node(node: OxiNamedNode) -> Result<NamedNode> {
    NamedNode::new(node.as_str()).map_err(RmlError::Parse)
}

fn convert_to_oxigraph_named_node(node: &NamedNode) -> Result<OxiNamedNode> {
    OxiNamedNode::new(node.iri()).map_err(|e| RmlError::Parse(e.to_string()))
}

fn convert_from_oxigraph_term(term: OxiTerm) -> Result<TermRef> {
    match term {
        OxiTerm::NamedNode(n) => Ok(TermRef::NamedNode(convert_from_oxigraph_named_node(n)?)),
        OxiTerm::BlankNode(b) => Ok(TermRef::BlankNode(BlankNode::new(b.as_str()))),
        OxiTerm::Literal(l) => Ok(TermRef::Literal(convert_from_oxigraph_literal(l))),
        OxiTerm::Triple(_) => Err(RmlError::Parse(
            "RDF-star triples are not supported".to_string(),
        )),
    }
}

fn convert_to_oxigraph_term(term: &TermRef) -> Result<OxiTerm> {
    match term {
        TermRef::NamedNode(n) => Ok(OxiTerm::NamedNode(convert_to_oxigraph_named_node(n)?)),
        TermRef::BlankNode(b) => Ok(OxiTerm::BlankNode(OxiBlankNode::new_unchecked(b.id()))),
        TermRef::Literal(l) => Ok(OxiTerm::Literal(convert_to_oxigraph_literal(l)?)),
    }
}

fn convert_from_oxigraph_literal(literal: OxiLiteral) -> Literal {
    if let Some(lang) = literal.language() {
        Literal::with_language(literal.value(), lang)
    } else if literal.datatype() != xsd::STRING {
        Literal::with_datatype(literal.value(), literal.datatype().as_str())
    } else {
        Literal::new(literal.value())
    }
}

fn convert_to_oxigraph_literal(literal: &Literal) -> Result<OxiLiteral> {
    if let Some(lang) = literal.language() {
        Ok(OxiLiteral::new_language_tagged_literal_unchecked(
            literal.value(),
            lang,
        ))
    } else if let Some(datatype) = literal.datatype() {
        let dt = OxiNamedNode::new(datatype).map_err(|e| RmlError::Parse(e.to_string()))?;
        Ok(OxiLiteral::new_typed_literal(literal.value(), dt))
    } else {
        Ok(OxiLiteral::new_simple_literal(literal.value()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_empty_store() {
        let store = InMemoryQuadStore::new();
        assert!(store.is_empty());
        assert_eq!(store.size(), 0);
    }

    #[test]
    fn test_add_quad() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");

        store
            .add_quad(subject.into(), predicate, object.into(), None)
            .unwrap();

        assert_eq!(store.size(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn test_add_multiple_quads() {
        let mut store = InMemoryQuadStore::new();

        let subject1 = NamedNode::new("http://example.org/person/1").unwrap();
        let subject2 = NamedNode::new("http://example.org/person/2").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();

        let quad1 = Quad::new(
            TermRef::NamedNode(subject1),
            predicate.clone(),
            TermRef::Literal(Literal::new("John Doe")),
            None,
        )
        .unwrap();

        let quad2 = Quad::new(
            TermRef::NamedNode(subject2),
            predicate,
            TermRef::Literal(Literal::new("Jane Smith")),
            None,
        )
        .unwrap();

        store.add_quads(vec![quad1, quad2]).unwrap();

        assert_eq!(store.size(), 2);
    }

    #[test]
    fn test_get_quads_all() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");

        store
            .add_quad(subject.into(), predicate, object.into(), None)
            .unwrap();

        let quads = store.get_quads(None, None, None, None).unwrap();
        assert_eq!(quads.len(), 1);
    }

    #[test]
    fn test_get_quads_by_subject() {
        let mut store = InMemoryQuadStore::new();

        let subject1 = NamedNode::new("http://example.org/person/1").unwrap();
        let subject2 = NamedNode::new("http://example.org/person/2").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();

        store
            .add_quad(
                subject1.clone().into(),
                predicate.clone(),
                Literal::new("John Doe").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                subject2.into(),
                predicate,
                Literal::new("Jane Smith").into(),
                None,
            )
            .unwrap();

        let subject1_ref = TermRef::NamedNode(subject1);
        let quads = store
            .get_quads(Some(&subject1_ref), None, None, None)
            .unwrap();
        assert_eq!(quads.len(), 1);
        assert_eq!(quads[0].object().value(), "John Doe");
    }

    #[test]
    fn test_get_quads_by_predicate() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let name_pred = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let age_pred = NamedNode::new("http://xmlns.com/foaf/0.1/age").unwrap();

        store
            .add_quad(
                subject.clone().into(),
                name_pred.clone(),
                Literal::new("John Doe").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                subject.into(),
                age_pred,
                Literal::new("30").into(),
                None,
            )
            .unwrap();

        let quads = store.get_quads(None, Some(&name_pred), None, None).unwrap();
        assert_eq!(quads.len(), 1);
        assert_eq!(quads[0].object().value(), "John Doe");
    }

    #[test]
    fn test_contains() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");

        store
            .add_quad(subject.clone().into(), predicate.clone(), object.into(), None)
            .unwrap();

        let subject_ref = TermRef::NamedNode(subject);
        assert!(store
            .contains(Some(&subject_ref), Some(&predicate), None, None)
            .unwrap());

        let other_pred = NamedNode::new("http://xmlns.com/foaf/0.1/age").unwrap();
        assert!(!store
            .contains(Some(&subject_ref), Some(&other_pred), None, None)
            .unwrap());
    }

    #[test]
    fn test_remove_quads() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();

        store
            .add_quad(
                subject.clone().into(),
                predicate.clone(),
                Literal::new("John Doe").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                subject.clone().into(),
                predicate.clone(),
                Literal::new("Jane Doe").into(),
                None,
            )
            .unwrap();

        assert_eq!(store.size(), 2);

        let subject_ref = TermRef::NamedNode(subject);
        let removed = store
            .remove_quads(Some(&subject_ref), Some(&predicate), None, None)
            .unwrap();

        assert_eq!(removed, 2);
        assert_eq!(store.size(), 0);
    }

    #[test]
    fn test_get_subjects() {
        let mut store = InMemoryQuadStore::new();

        let subject1 = NamedNode::new("http://example.org/person/1").unwrap();
        let subject2 = NamedNode::new("http://example.org/person/2").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();

        store
            .add_quad(
                subject1.into(),
                predicate.clone(),
                Literal::new("John Doe").into(),
                None,
            )
            .unwrap();

        store
            .add_quad(
                subject2.into(),
                predicate,
                Literal::new("Jane Smith").into(),
                None,
            )
            .unwrap();

        let subjects = store.get_subjects().unwrap();
        assert_eq!(subjects.len(), 2);
    }

    #[test]
    fn test_namespaces() {
        let mut store = InMemoryQuadStore::new();

        store.add_namespace("foaf".to_string(), "http://xmlns.com/foaf/0.1/".to_string());
        store.add_namespace("ex".to_string(), "http://example.org/".to_string());

        assert_eq!(store.get_namespaces().len(), 2);

        let translated = store.try_property_translation("http://xmlns.com/foaf/0.1/name");
        assert_eq!(translated, "foaf:name");

        store.remove_namespace("foaf");
        assert_eq!(store.get_namespaces().len(), 1);
    }

    #[test]
    fn test_copy_namespaces() {
        let mut store1 = InMemoryQuadStore::new();
        store1.add_namespace("foaf".to_string(), "http://xmlns.com/foaf/0.1/".to_string());

        let mut store2 = InMemoryQuadStore::new();
        store2.copy_namespaces(&store1);

        assert_eq!(store2.get_namespaces().len(), 1);
        assert_eq!(
            store2.get_namespaces().get("foaf"),
            Some(&"http://xmlns.com/foaf/0.1/".to_string())
        );
    }

    #[test]
    fn test_rename_predicates() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let old_pred = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let new_pred = NamedNode::new("http://example.org/fullName").unwrap();

        store
            .add_quad(
                subject.into(),
                old_pred.clone(),
                Literal::new("John Doe").into(),
                None,
            )
            .unwrap();

        let count = store.rename_all_predicates(&old_pred, new_pred.clone()).unwrap();
        assert_eq!(count, 1);

        let quads = store.get_quads(None, Some(&new_pred), None, None).unwrap();
        assert_eq!(quads.len(), 1);

        let quads = store.get_quads(None, Some(&old_pred), None, None).unwrap();
        assert_eq!(quads.len(), 0);
    }

    #[test]
    fn test_rename_objects() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let old_obj = Literal::new("John Doe");
        let new_obj = Literal::new("Jane Doe");

        store
            .add_quad(
                subject.into(),
                predicate,
                old_obj.clone().into(),
                None,
            )
            .unwrap();

        let old_obj_ref = TermRef::Literal(old_obj);
        let new_obj_ref = TermRef::Literal(new_obj.clone());
        let count = store.rename_all_objects(&old_obj_ref, new_obj_ref.clone()).unwrap();
        assert_eq!(count, 1);

        let quads = store.get_quads(None, None, Some(&new_obj_ref), None).unwrap();
        assert_eq!(quads.len(), 1);
        assert_eq!(quads[0].object().value(), "Jane Doe");
    }

    #[test]
    fn test_is_subset() {
        let mut store1 = InMemoryQuadStore::new();
        let mut store2 = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();

        let quad = Quad::new(
            TermRef::NamedNode(subject),
            predicate,
            TermRef::Literal(Literal::new("John Doe")),
            None,
        )
        .unwrap();

        store1.add_quads(vec![quad.clone()]).unwrap();
        store2.add_quads(vec![quad]).unwrap();

        assert!(store1.is_subset(&store2).unwrap());
        assert!(store2.is_subset(&store1).unwrap());
    }

    #[test]
    fn test_is_isomorphic() {
        let mut store1 = InMemoryQuadStore::new();
        let mut store2 = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();

        let quad = Quad::new(
            TermRef::NamedNode(subject),
            predicate,
            TermRef::Literal(Literal::new("John Doe")),
            None,
        )
        .unwrap();

        store1.add_quads(vec![quad.clone()]).unwrap();
        store2.add_quads(vec![quad]).unwrap();

        assert!(store1.is_isomorphic(&store2).unwrap());
    }

    #[test]
    fn test_read_write_turtle() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");

        store
            .add_quad(TermRef::NamedNode(subject), predicate, TermRef::Literal(object), None)
            .unwrap();

        // Write to buffer
        let mut buffer = Vec::new();
        store.write(&mut buffer, RdfFormat::NTriples).unwrap();

        // Read back
        let mut store2 = InMemoryQuadStore::new();
        store2
            .read(buffer.as_slice(), None, RdfFormat::NTriples)
            .unwrap();

        assert_eq!(store2.size(), 1);
        assert!(store.is_isomorphic(&store2).unwrap());
    }

    #[test]
    fn test_format_detection() {
        assert_eq!(RdfFormat::from_extension("ttl"), Some(RdfFormat::Turtle));
        assert_eq!(RdfFormat::from_extension("nt"), Some(RdfFormat::NTriples));
        assert_eq!(RdfFormat::from_extension("nq"), Some(RdfFormat::NQuads));
        assert_eq!(RdfFormat::from_extension("trig"), Some(RdfFormat::TriG));
        assert_eq!(RdfFormat::from_extension("rdf"), Some(RdfFormat::RdfXml));
        assert_eq!(RdfFormat::from_extension("xml"), Some(RdfFormat::RdfXml));
        assert_eq!(RdfFormat::from_extension("unknown"), None);
    }

    #[test]
    fn test_blank_nodes() {
        let mut store = InMemoryQuadStore::new();

        let subject = BlankNode::new("b1");
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");

        store
            .add_quad(TermRef::BlankNode(subject), predicate, TermRef::Literal(object), None)
            .unwrap();

        assert_eq!(store.size(), 1);

        let quads = store.get_quads(None, None, None, None).unwrap();
        assert!(quads[0].subject().is_blank_node());
    }

    #[test]
    fn test_graph_names() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");
        let graph = NamedNode::new("http://example.org/graph1").unwrap();

        store
            .add_quad(
                TermRef::NamedNode(subject),
                predicate.clone(),
                TermRef::Literal(object),
                Some(graph.clone()),
            )
            .unwrap();

        let quads = store.get_quads(None, None, None, Some(&graph)).unwrap();
        assert_eq!(quads.len(), 1);

        let quads = store.get_quads(None, None, None, None).unwrap();
        assert_eq!(quads.len(), 1);
    }

    #[test]
    fn test_get_quad_single() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");

        store
            .add_quad(TermRef::NamedNode(subject.clone()), predicate.clone(), TermRef::Literal(object), None)
            .unwrap();

        let subject_ref = TermRef::NamedNode(subject);
        let quad = store
            .get_quad(Some(&subject_ref), Some(&predicate), None, None)
            .unwrap();

        assert!(quad.is_some());
        assert_eq!(quad.unwrap().object().value(), "John Doe");
    }

    #[test]
    fn test_clear() {
        let mut store = InMemoryQuadStore::new();

        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");

        store
            .add_quad(TermRef::NamedNode(subject), predicate, TermRef::Literal(object), None)
            .unwrap();

        assert_eq!(store.size(), 1);

        store.clear();

        assert_eq!(store.size(), 0);
        assert!(store.is_empty());
    }
}
