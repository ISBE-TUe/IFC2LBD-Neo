//! RDF Term representation
//!
//! This module defines the core RDF term types following the Java RML Mapper architecture.
//! Terms are the building blocks of RDF triples and quads.
//!
//! # Architecture
//!
//! The module follows a trait-based design similar to the Java implementation:
//! - `Term` trait: Common interface for all RDF terms
//! - `NamedNode`: Represents IRIs (Internationalized Resource Identifiers)
//! - `Literal`: Represents literal values with optional language tags or datatypes
//! - `BlankNode`: Represents blank nodes with unique identifiers
//! - `TermRef`: Enum for pattern matching and storage
//! - `Quad`: Represents an RDF quad (subject, predicate, object, graph)
//!
//! # Examples
//!
//! ```
//! use rml_mapper::term::{NamedNode, Literal, BlankNode, Term};
//!
//! // Create a named node (IRI)
//! let subject = NamedNode::new("http://example.org/person/1").unwrap();
//! assert_eq!(subject.value(), "http://example.org/person/1");
//!
//! // Create a literal
//! let name = Literal::new("John Doe");
//! assert_eq!(name.value(), "John Doe");
//!
//! // Create a literal with language tag
//! let label = Literal::with_language("Hello", "en");
//! assert_eq!(label.language(), Some("en"));
//!
//! // Create a literal with datatype
//! let age = Literal::with_datatype("30", "http://www.w3.org/2001/XMLSchema#integer");
//! assert_eq!(age.datatype(), Some("http://www.w3.org/2001/XMLSchema#integer"));
//!
//! // Create a blank node
//! let blank = BlankNode::new("b1");
//! assert_eq!(blank.value(), "b1");
//! ```

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Common trait for all RDF terms
///
/// This trait provides the interface that all RDF term types must implement,
/// matching the Java RML Mapper's `Term` interface.
pub trait Term {
    /// Returns the string value of this term
    fn value(&self) -> &str;
    
    /// Returns true if this term is an IRI (Named Node)
    fn is_iri(&self) -> bool {
        false
    }
    
    /// Returns true if this term is a literal
    fn is_literal(&self) -> bool {
        false
    }
    
    /// Returns true if this term is a blank node
    fn is_blank_node(&self) -> bool {
        false
    }
}

/// Represents an IRI (Internationalized Resource Identifier) in RDF
///
/// Named nodes are used for subjects, predicates, objects, and graph names.
/// They must be valid IRIs according to RFC 3987.
///
/// # Examples
///
/// ```
/// use rml_mapper::term::{NamedNode, Term};
///
/// let node = NamedNode::new("http://example.org/resource").unwrap();
/// assert!(node.is_iri());
/// assert_eq!(node.value(), "http://example.org/resource");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NamedNode {
    iri: String,
}

impl NamedNode {
    /// Creates a new named node with the given IRI
    ///
    /// # Arguments
    ///
    /// * `iri` - The IRI string
    ///
    /// # Returns
    ///
    /// Returns `Ok(NamedNode)` if the IRI is valid, or `Err` if validation fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use rml_mapper::term::NamedNode;
    ///
    /// let node = NamedNode::new("http://example.org/resource").unwrap();
    /// assert_eq!(node.iri(), "http://example.org/resource");
    /// ```
    pub fn new(iri: impl Into<String>) -> Result<Self, String> {
        let iri = iri.into();
        
        // Basic IRI validation
        if iri.is_empty() {
            return Err("IRI cannot be empty".to_string());
        }
        
        // Check for basic IRI structure (scheme:path or relative IRI)
        if !Self::is_valid_iri(&iri) {
            return Err(format!("Invalid IRI: {}", iri));
        }
        
        Ok(NamedNode { iri })
    }
    
    /// Creates a new named node without validation (use with caution)
    ///
    /// This method bypasses IRI validation and should only be used when
    /// the IRI is known to be valid.
    ///
    /// # Arguments
    ///
    /// * `iri` - The IRI string
    pub fn new_unchecked(iri: impl Into<String>) -> Self {
        NamedNode { iri: iri.into() }
    }
    
    /// Returns the IRI string
    pub fn iri(&self) -> &str {
        &self.iri
    }
    
    /// Validates if a string is a valid IRI
    ///
    /// This performs basic validation:
    /// - Non-empty
    /// - Contains a colon (for absolute IRIs) or starts with # or / (for relative)
    /// - No unescaped spaces or control characters
    fn is_valid_iri(iri: &str) -> bool {
        if iri.is_empty() {
            return false;
        }
        
        // Check for control characters and unescaped spaces
        if iri.chars().any(|c| c.is_control() || c == ' ') {
            return false;
        }
        
        // Accept absolute IRIs (with scheme) or relative IRIs
        iri.contains(':') || iri.starts_with('#') || iri.starts_with('/')
    }
}

impl Term for NamedNode {
    fn value(&self) -> &str {
        &self.iri
    }
    
    fn is_iri(&self) -> bool {
        true
    }
}

impl fmt::Display for NamedNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.iri)
    }
}

impl From<NamedNode> for String {
    fn from(node: NamedNode) -> String {
        node.iri
    }
}

/// Represents a literal value in RDF
///
/// Literals can have:
/// - A plain value (string)
/// - A language tag (e.g., "en", "fr")
/// - A datatype IRI (e.g., xsd:integer, xsd:dateTime)
///
/// Note: A literal cannot have both a language tag and a datatype.
///
/// # Examples
///
/// ```
/// use rml_mapper::term::{Literal, Term};
///
/// // Plain literal
/// let plain = Literal::new("Hello");
/// assert_eq!(plain.value(), "Hello");
///
/// // Literal with language tag
/// let lang = Literal::with_language("Bonjour", "fr");
/// assert_eq!(lang.language(), Some("fr"));
///
/// // Literal with datatype
/// let typed = Literal::with_datatype("42", "http://www.w3.org/2001/XMLSchema#integer");
/// assert_eq!(typed.datatype(), Some("http://www.w3.org/2001/XMLSchema#integer"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Literal {
    value: String,
    language: Option<String>,
    datatype: Option<String>,
}

impl Literal {
    /// Creates a new plain literal
    ///
    /// # Arguments
    ///
    /// * `value` - The literal value
    pub fn new(value: impl Into<String>) -> Self {
        Literal {
            value: value.into(),
            language: None,
            datatype: None,
        }
    }
    
    /// Creates a new literal with a language tag
    ///
    /// # Arguments
    ///
    /// * `value` - The literal value
    /// * `language` - The language tag (e.g., "en", "fr", "de")
    ///
    /// # Examples
    ///
    /// ```
    /// use rml_mapper::term::{Literal, Term};
    ///
    /// let literal = Literal::with_language("Hello", "en");
    /// assert_eq!(literal.value(), "Hello");
    /// assert_eq!(literal.language(), Some("en"));
    /// ```
    pub fn with_language(value: impl Into<String>, language: impl Into<String>) -> Self {
        Literal {
            value: value.into(),
            language: Some(language.into()),
            datatype: None,
        }
    }
    
    /// Creates a new literal with a datatype IRI
    ///
    /// # Arguments
    ///
    /// * `value` - The literal value
    /// * `datatype` - The datatype IRI
    ///
    /// # Examples
    ///
    /// ```
    /// use rml_mapper::term::{Literal, Term};
    ///
    /// let literal = Literal::with_datatype("42", "http://www.w3.org/2001/XMLSchema#integer");
    /// assert_eq!(literal.value(), "42");
    /// assert_eq!(literal.datatype(), Some("http://www.w3.org/2001/XMLSchema#integer"));
    /// ```
    pub fn with_datatype(value: impl Into<String>, datatype: impl Into<String>) -> Self {
        Literal {
            value: value.into(),
            language: None,
            datatype: Some(datatype.into()),
        }
    }
    
    /// Returns the language tag if present
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }
    
    /// Returns the datatype IRI if present
    pub fn datatype(&self) -> Option<&str> {
        self.datatype.as_deref()
    }
    
    /// Returns the lexical form (string value) of the literal
    pub fn lexical_form(&self) -> &str {
        &self.value
    }
}

impl Term for Literal {
    fn value(&self) -> &str {
        &self.value
    }
    
    fn is_literal(&self) -> bool {
        true
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self.value)?;
        
        if let Some(lang) = &self.language {
            write!(f, "@{}", lang)?;
        } else if let Some(datatype) = &self.datatype {
            write!(f, "^^<{}>", datatype)?;
        }
        
        Ok(())
    }
}

impl From<String> for Literal {
    fn from(value: String) -> Self {
        Literal::new(value)
    }
}

impl From<&str> for Literal {
    fn from(value: &str) -> Self {
        Literal::new(value)
    }
}

/// Represents a blank node in RDF
///
/// Blank nodes are anonymous resources identified by a local identifier.
/// They are used when the identity of a resource is not important or unknown.
///
/// # Examples
///
/// ```
/// use rml_mapper::term::{BlankNode, Term};
///
/// let blank = BlankNode::new("b1");
/// assert!(blank.is_blank_node());
/// assert_eq!(blank.value(), "b1");
///
/// // Generate a unique blank node
/// let unique = BlankNode::generate();
/// assert!(unique.value().starts_with("b"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlankNode {
    id: String,
}

impl BlankNode {
    /// Creates a new blank node with the given identifier
    ///
    /// # Arguments
    ///
    /// * `id` - The blank node identifier
    pub fn new(id: impl Into<String>) -> Self {
        BlankNode { id: id.into() }
    }
    
    /// Generates a new blank node with a unique identifier
    ///
    /// The identifier is guaranteed to be unique within this process.
    ///
    /// # Examples
    ///
    /// ```
    /// use rml_mapper::term::{BlankNode, Term};
    ///
    /// let b1 = BlankNode::generate();
    /// let b2 = BlankNode::generate();
    /// assert_ne!(b1.value(), b2.value());
    /// ```
    pub fn generate() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        BlankNode {
            id: format!("b{}", id),
        }
    }
    
    /// Returns the blank node identifier
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl Term for BlankNode {
    fn value(&self) -> &str {
        &self.id
    }
    
    fn is_blank_node(&self) -> bool {
        true
    }
}

impl fmt::Display for BlankNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "_:{}", self.id)
    }
}

/// Enum that can hold any RDF term type
///
/// This enum is useful for pattern matching and storing heterogeneous collections
/// of terms. It implements the `Term` trait by delegating to the contained term.
///
/// # Examples
///
/// ```
/// use rml_mapper::term::{TermRef, NamedNode, Literal, BlankNode, Term};
///
/// let terms = vec![
///     TermRef::NamedNode(NamedNode::new("http://example.org/resource").unwrap()),
///     TermRef::Literal(Literal::new("Hello")),
///     TermRef::BlankNode(BlankNode::new("b1")),
/// ];
///
/// for term in &terms {
///     println!("{}", term.value());
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TermRef {
    /// A named node (IRI)
    NamedNode(NamedNode),
    /// A literal value
    Literal(Literal),
    /// A blank node
    BlankNode(BlankNode),
}

impl Term for TermRef {
    fn value(&self) -> &str {
        match self {
            TermRef::NamedNode(n) => n.value(),
            TermRef::Literal(l) => l.value(),
            TermRef::BlankNode(b) => b.value(),
        }
    }
    
    fn is_iri(&self) -> bool {
        matches!(self, TermRef::NamedNode(_))
    }
    
    fn is_literal(&self) -> bool {
        matches!(self, TermRef::Literal(_))
    }
    
    fn is_blank_node(&self) -> bool {
        matches!(self, TermRef::BlankNode(_))
    }
}

impl fmt::Display for TermRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TermRef::NamedNode(n) => write!(f, "{}", n),
            TermRef::Literal(l) => write!(f, "{}", l),
            TermRef::BlankNode(b) => write!(f, "{}", b),
        }
    }
}

impl From<NamedNode> for TermRef {
    fn from(node: NamedNode) -> Self {
        TermRef::NamedNode(node)
    }
}

impl From<Literal> for TermRef {
    fn from(literal: Literal) -> Self {
        TermRef::Literal(literal)
    }
}

impl From<BlankNode> for TermRef {
    fn from(blank: BlankNode) -> Self {
        TermRef::BlankNode(blank)
    }
}

/// Represents an RDF quad (subject, predicate, object, graph)
///
/// A quad is an RDF triple with an optional graph name. The graph name
/// allows organizing triples into named graphs.
///
/// # Constraints
///
/// - Subject must be a NamedNode or BlankNode
/// - Predicate must be a NamedNode
/// - Object can be any term type
/// - Graph (if present) must be a NamedNode
///
/// # Examples
///
/// ```
/// use rml_mapper::term::{Quad, NamedNode, Literal, Term};
///
/// let subject = NamedNode::new("http://example.org/person/1").unwrap();
/// let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
/// let object = Literal::new("John Doe");
///
/// let quad = Quad::new(subject, predicate, object, None).unwrap();
/// assert_eq!(quad.subject().value(), "http://example.org/person/1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Quad {
    subject: TermRef,
    predicate: NamedNode,
    object: TermRef,
    graph: Option<NamedNode>,
}

impl Quad {
    /// Creates a new quad
    ///
    /// # Arguments
    ///
    /// * `subject` - The subject (must be NamedNode or BlankNode)
    /// * `predicate` - The predicate (must be NamedNode)
    /// * `object` - The object (can be any term type)
    /// * `graph` - Optional graph name
    ///
    /// # Returns
    ///
    /// Returns `Ok(Quad)` if the quad is valid, or `Err` if validation fails.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Subject is a Literal
    /// - Any term is invalid
    pub fn new(
        subject: impl Into<TermRef>,
        predicate: NamedNode,
        object: impl Into<TermRef>,
        graph: Option<NamedNode>,
    ) -> Result<Self, String> {
        let subject = subject.into();
        let object = object.into();
        
        // Validate subject is not a literal
        if subject.is_literal() {
            return Err("Quad subject cannot be a literal".to_string());
        }
        
        Ok(Quad {
            subject,
            predicate,
            object,
            graph,
        })
    }
    
    /// Returns the subject of this quad
    pub fn subject(&self) -> &TermRef {
        &self.subject
    }
    
    /// Returns the predicate of this quad
    pub fn predicate(&self) -> &NamedNode {
        &self.predicate
    }
    
    /// Returns the object of this quad
    pub fn object(&self) -> &TermRef {
        &self.object
    }
    
    /// Returns the graph name if present
    pub fn graph(&self) -> Option<&NamedNode> {
        self.graph.as_ref()
    }
    
    /// Converts this quad into a triple (subject, predicate, object)
    ///
    /// This is useful when working with RDF stores that don't support quads.
    pub fn into_triple(self) -> Triple {
        Triple {
            subject: self.subject,
            predicate: self.predicate,
            object: self.object,
        }
    }
    
    /// Returns a reference to this quad as a triple
    pub fn as_triple(&self) -> TripleRef<'_> {
        TripleRef {
            subject: &self.subject,
            predicate: &self.predicate,
            object: &self.object,
        }
    }
}

impl fmt::Display for Quad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.subject, self.predicate, self.object)?;
        if let Some(graph) = &self.graph {
            write!(f, " {}", graph)?;
        }
        write!(f, " .")
    }
}

/// Represents an RDF triple (subject, predicate, object)
///
/// A triple is the basic unit of RDF data. It represents a statement
/// about a resource.
///
/// # Constraints
///
/// - Subject must be a NamedNode or BlankNode
/// - Predicate must be a NamedNode
/// - Object can be any term type
///
/// # Examples
///
/// ```
/// use rml_mapper::term::{Triple, NamedNode, Literal};
///
/// let subject = NamedNode::new("http://example.org/person/1").unwrap();
/// let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
/// let object = Literal::new("John Doe");
///
/// let triple = Triple::new(subject, predicate, object).unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Triple {
    pub subject: TermRef,
    pub predicate: NamedNode,
    pub object: TermRef,
}

impl Triple {
    /// Creates a new triple
    ///
    /// # Arguments
    ///
    /// * `subject` - The subject (must be NamedNode or BlankNode)
    /// * `predicate` - The predicate (must be NamedNode)
    /// * `object` - The object (can be any term type)
    ///
    /// # Returns
    ///
    /// Returns `Ok(Triple)` if the triple is valid, or `Err` if validation fails.
    pub fn new(
        subject: impl Into<TermRef>,
        predicate: NamedNode,
        object: impl Into<TermRef>,
    ) -> Result<Self, String> {
        let subject = subject.into();
        let object = object.into();
        
        // Validate subject is not a literal
        if subject.is_literal() {
            return Err("Triple subject cannot be a literal".to_string());
        }
        
        Ok(Triple {
            subject,
            predicate,
            object,
        })
    }
    
    /// Converts this triple into a quad with an optional graph name
    pub fn into_quad(self, graph: Option<NamedNode>) -> Quad {
        Quad {
            subject: self.subject,
            predicate: self.predicate,
            object: self.object,
            graph,
        }
    }
}

impl fmt::Display for Triple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {} .", self.subject, self.predicate, self.object)
    }
}

/// A borrowed reference to a triple
///
/// This is useful for working with triples without taking ownership.
#[derive(Debug, Clone, Copy)]
pub struct TripleRef<'a> {
    pub subject: &'a TermRef,
    pub predicate: &'a NamedNode,
    pub object: &'a TermRef,
}

impl<'a> fmt::Display for TripleRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {} .", self.subject, self.predicate, self.object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_named_node() {
        let node = NamedNode::new("http://example.org/resource").unwrap();
        assert_eq!(node.value(), "http://example.org/resource");
        assert!(node.is_iri());
        assert!(!node.is_literal());
        assert!(!node.is_blank_node());
        assert_eq!(node.to_string(), "<http://example.org/resource>");
    }
    
    #[test]
    fn test_named_node_validation() {
        assert!(NamedNode::new("http://example.org").is_ok());
        assert!(NamedNode::new("https://example.org/path").is_ok());
        assert!(NamedNode::new("urn:isbn:0451450523").is_ok());
        assert!(NamedNode::new("#fragment").is_ok());
        assert!(NamedNode::new("/relative").is_ok());
        
        assert!(NamedNode::new("").is_err());
        assert!(NamedNode::new("invalid iri").is_err());
        assert!(NamedNode::new("no-scheme").is_err());
    }
    
    #[test]
    fn test_literal() {
        let lit = Literal::new("Hello");
        assert_eq!(lit.value(), "Hello");
        assert!(lit.is_literal());
        assert!(!lit.is_iri());
        assert!(!lit.is_blank_node());
        assert_eq!(lit.to_string(), "\"Hello\"");
    }
    
    #[test]
    fn test_literal_with_language() {
        let lit = Literal::with_language("Bonjour", "fr");
        assert_eq!(lit.value(), "Bonjour");
        assert_eq!(lit.language(), Some("fr"));
        assert_eq!(lit.datatype(), None);
        assert_eq!(lit.to_string(), "\"Bonjour\"@fr");
    }
    
    #[test]
    fn test_literal_with_datatype() {
        let lit = Literal::with_datatype("42", "http://www.w3.org/2001/XMLSchema#integer");
        assert_eq!(lit.value(), "42");
        assert_eq!(lit.language(), None);
        assert_eq!(lit.datatype(), Some("http://www.w3.org/2001/XMLSchema#integer"));
        assert_eq!(lit.to_string(), "\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>");
    }
    
    #[test]
    fn test_blank_node() {
        let blank = BlankNode::new("b1");
        assert_eq!(blank.value(), "b1");
        assert!(blank.is_blank_node());
        assert!(!blank.is_iri());
        assert!(!blank.is_literal());
        assert_eq!(blank.to_string(), "_:b1");
    }
    
    #[test]
    fn test_blank_node_generate() {
        let b1 = BlankNode::generate();
        let b2 = BlankNode::generate();
        assert_ne!(b1.value(), b2.value());
        assert!(b1.value().starts_with("b"));
        assert!(b2.value().starts_with("b"));
    }
    
    #[test]
    fn test_term_ref() {
        let node = TermRef::NamedNode(NamedNode::new("http://example.org").unwrap());
        assert!(node.is_iri());
        
        let lit = TermRef::Literal(Literal::new("test"));
        assert!(lit.is_literal());
        
        let blank = TermRef::BlankNode(BlankNode::new("b1"));
        assert!(blank.is_blank_node());
    }
    
    #[test]
    fn test_triple() {
        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");
        
        let triple = Triple::new(subject, predicate, object).unwrap();
        assert_eq!(triple.subject.value(), "http://example.org/person/1");
        assert_eq!(triple.predicate.value(), "http://xmlns.com/foaf/0.1/name");
        assert_eq!(triple.object.value(), "John Doe");
    }
    
    #[test]
    fn test_triple_validation() {
        let predicate = NamedNode::new("http://example.org/pred").unwrap();
        let object = Literal::new("value");
        
        // Subject cannot be a literal
        let literal_subject = Literal::new("invalid");
        let result = Triple::new(TermRef::from(literal_subject), predicate.clone(), object);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_quad() {
        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");
        let graph = NamedNode::new("http://example.org/graph1").unwrap();
        
        let quad = Quad::new(subject, predicate, object, Some(graph)).unwrap();
        assert_eq!(quad.subject().value(), "http://example.org/person/1");
        assert_eq!(quad.predicate().value(), "http://xmlns.com/foaf/0.1/name");
        assert_eq!(quad.object().value(), "John Doe");
        assert_eq!(quad.graph().unwrap().value(), "http://example.org/graph1");
    }
    
    #[test]
    fn test_quad_without_graph() {
        let subject = NamedNode::new("http://example.org/person/1").unwrap();
        let predicate = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let object = Literal::new("John Doe");
        
        let quad = Quad::new(subject, predicate, object, None).unwrap();
        assert!(quad.graph().is_none());
    }
    
    #[test]
    fn test_conversions() {
        let node = NamedNode::new("http://example.org").unwrap();
        let term_ref: TermRef = node.clone().into();
        assert!(term_ref.is_iri());
        
        let lit = Literal::new("test");
        let term_ref: TermRef = lit.clone().into();
        assert!(term_ref.is_literal());
        
        let blank = BlankNode::new("b1");
        let term_ref: TermRef = blank.clone().into();
        assert!(term_ref.is_blank_node());
    }
}
