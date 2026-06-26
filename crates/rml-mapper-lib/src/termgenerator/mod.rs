//! Term Generator
//!
//! This module handles the generation of RDF terms from mapping rules and data records.
//! It processes templates, references, and constants to create IRIs, literals, and blank nodes.
//!
//! # Architecture
//!
//! The module follows the Java RML Mapper's TermGenerator design:
//! - `TermGenerator` trait: Common interface for all term generators
//! - `NamedNodeGenerator`: Generates IRI terms
//! - `LiteralGenerator`: Generates literal terms with optional language tags or datatypes
//! - `BlankNodeGenerator`: Generates blank node terms
//! - `ValueExtractor` trait: Extracts values from records
//!   - `ReferenceExtractor`: Extracts values by field name
//!   - `ConstantExtractor`: Returns constant values
//!   - `TemplateExtractor`: Processes templates with placeholders
//!
//! # Examples
//!
//! ```
//! use rml_mapper::termgenerator::{NamedNodeGenerator, TemplateExtractor, StrictMode, TermGenerator};
//! use rml_mapper::records::{Record, RecordValue};
//! use std::collections::HashMap;
//!
//! let mut record = Record::new();
//! record.insert("id".to_string(), RecordValue::String("123".to_string()));
//!
//! let extractor = TemplateExtractor::new("http://example.org/person/{id}").unwrap();
//! let generator = NamedNodeGenerator::new(
//!     Box::new(extractor),
//!     Some("http://example.org/".to_string()),
//!     StrictMode::Strict
//! );
//!
//! let terms = generator.generate(&record).unwrap();
//! assert_eq!(terms.len(), 1);
//! ```

use crate::error::{Result, RmlError};
use crate::records::{Record, RecordValue};
use crate::term::{BlankNode, Literal, NamedNode, TermRef};
use regex::Regex;

/// Trait for generating RDF terms from records
///
/// This trait provides the interface that all term generators must implement,
/// matching the Java RML Mapper's TermGenerator interface.
pub trait TermGenerator: Send + Sync {
    /// Generates terms from a record
    ///
    /// # Arguments
    ///
    /// * `record` - The record to generate terms from
    ///
    /// # Returns
    ///
    /// A vector of generated terms. Multiple terms may be generated if the
    /// extractor returns multiple values (e.g., from an array field).
    fn generate(&self, record: &Record) -> Result<Vec<TermRef>>;

    /// Returns true if this generator needs an EOF marker
    ///
    /// Some generators (e.g., those using streaming data) may need to know
    /// when the end of the data stream is reached.
    fn needs_eof_marker(&self) -> bool {
        false
    }
}

/// Strict mode for IRI generation
///
/// Controls how invalid IRIs are handled during term generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictMode {
    /// Fail on invalid IRIs
    ///
    /// When an invalid IRI is encountered, return an error immediately.
    Strict,

    /// Skip invalid IRIs
    ///
    /// When an invalid IRI is encountered, skip it and continue processing.
    /// This is useful for best-effort processing of data with quality issues.
    BestEffort,
}

/// Trait for extracting values from records
///
/// Value extractors are responsible for extracting string values from records
/// using various strategies (references, templates, constants, etc.).
pub trait ValueExtractor: Send + Sync {
    /// Extracts values from a record
    ///
    /// # Arguments
    ///
    /// * `record` - The record to extract values from
    ///
    /// # Returns
    ///
    /// A vector of extracted string values. Multiple values may be returned
    /// if the extraction targets an array field.
    fn extract(&self, record: &Record) -> Result<Vec<String>>;
}

/// Extracts values by field name reference
///
/// This extractor retrieves values from a record using a field name or path.
/// It supports simple field access (e.g., "name") and nested field access
/// (e.g., "address.city").
///
/// # Examples
///
/// ```
/// use rml_mapper::termgenerator::{ReferenceExtractor, ValueExtractor};
/// use rml_mapper::records::{Record, RecordValue};
///
/// let mut record = Record::new();
/// record.insert("name".to_string(), RecordValue::String("Alice".to_string()));
///
/// let extractor = ReferenceExtractor::new("name");
/// let values = extractor.extract(&record).unwrap();
/// assert_eq!(values, vec!["Alice"]);
/// ```
#[derive(Debug, Clone)]
pub struct ReferenceExtractor {
    reference: String,
}

impl ReferenceExtractor {
    /// Creates a new reference extractor
    ///
    /// # Arguments
    ///
    /// * `reference` - The field name or path to extract
    pub fn new(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
        }
    }
}

impl ValueExtractor for ReferenceExtractor {
    fn extract(&self, record: &Record) -> Result<Vec<String>> {
        // Use the existing extract_value function from records module
        let value = crate::records::extract_value(record, &self.reference).ok_or_else(|| {
            RmlError::Mapping(format!("Reference '{}' not found in record", self.reference))
        })?;

        // Convert the value to strings
        match value {
            RecordValue::Array(arr) => {
                let strings: Vec<String> = arr
                    .iter()
                    .filter(|v| !v.is_empty())
                    .map(|v| v.as_string())
                    .collect();
                if strings.is_empty() {
                    Err(RmlError::Mapping(format!(
                        "Reference '{}' returned empty array",
                        self.reference
                    )))
                } else {
                    Ok(strings)
                }
            }
            RecordValue::Null => Err(RmlError::Mapping(format!(
                "Reference '{}' is null",
                self.reference
            ))),
            other => {
                let s = other.as_string();
                if s.is_empty() {
                    Err(RmlError::Mapping(format!(
                        "Reference '{}' is empty",
                        self.reference
                    )))
                } else {
                    Ok(vec![s])
                }
            }
        }
    }
}

/// Returns a constant value
///
/// This extractor always returns the same constant value, regardless of the
/// input record. It's useful for generating fixed IRIs or literal values.
///
/// # Examples
///
/// ```
/// use rml_mapper::termgenerator::{ConstantExtractor, ValueExtractor};
/// use rml_mapper::records::Record;
///
/// let extractor = ConstantExtractor::new("http://example.org/constant");
/// let values = extractor.extract(&Record::new()).unwrap();
/// assert_eq!(values, vec!["http://example.org/constant"]);
/// ```
#[derive(Debug, Clone)]
pub struct ConstantExtractor {
    value: String,
}

impl ConstantExtractor {
    /// Creates a new constant extractor
    ///
    /// # Arguments
    ///
    /// * `value` - The constant value to return
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl ValueExtractor for ConstantExtractor {
    fn extract(&self, _record: &Record) -> Result<Vec<String>> {
        Ok(vec![self.value.clone()])
    }
}

/// Processes templates with field placeholders
///
/// This extractor processes template strings containing placeholders in the
/// form `{field}` and replaces them with values from the record. Values are
/// URL-encoded when generating IRIs.
///
/// # Examples
///
/// ```
/// use rml_mapper::termgenerator::{TemplateExtractor, ValueExtractor};
/// use rml_mapper::records::{Record, RecordValue};
///
/// let mut record = Record::new();
/// record.insert("id".to_string(), RecordValue::String("123".to_string()));
/// record.insert("name".to_string(), RecordValue::String("Alice".to_string()));
///
/// let extractor = TemplateExtractor::new("http://example.org/{id}/profile").unwrap();
/// let values = extractor.extract(&record).unwrap();
/// assert_eq!(values, vec!["http://example.org/123/profile"]);
/// ```
#[derive(Debug, Clone)]
pub struct TemplateExtractor {
    template: String,
    placeholders: Vec<String>,
    /// Pre-computed "{placeholder}" strings to avoid format!() in hot loop
    placeholder_patterns: Vec<String>,
}

impl TemplateExtractor {
    /// Creates a new template extractor
    ///
    /// # Arguments
    ///
    /// * `template` - The template string with `{field}` placeholders
    ///
    /// # Returns
    ///
    /// Returns `Ok(TemplateExtractor)` if the template is valid, or an error
    /// if the template syntax is invalid.
    pub fn new(template: impl Into<String>) -> Result<Self> {
        let template = template.into();

        // Extract placeholders from the template
        let pattern = Regex::new(r"\{([^}]+)\}")
            .map_err(|e| RmlError::Parse(format!("Invalid template regex: {}", e)))?;

        let placeholders: Vec<String> = pattern
            .captures_iter(&template)
            .map(|cap| cap[1].to_string())
            .collect();

        if placeholders.is_empty() {
            return Err(RmlError::Parse(format!(
                "Template '{}' contains no placeholders",
                template
            )));
        }

        let placeholder_patterns: Vec<String> = placeholders
            .iter()
            .map(|p| format!("{{{}}}", p))
            .collect();

        Ok(Self {
            template,
            placeholders,
            placeholder_patterns,
        })
    }

    /// URL-encodes a value for use in IRIs
    fn url_encode(value: &str) -> String {
        urlencoding::encode(value).to_string()
    }
}

impl ValueExtractor for TemplateExtractor {
    fn extract(&self, record: &Record) -> Result<Vec<String>> {
        // Extract all placeholder values into a Vec (avoids HashMap overhead)
        let mut all_values: Vec<Vec<String>> = Vec::with_capacity(self.placeholders.len());

        for placeholder in &self.placeholders {
            let value = crate::records::extract_value(record, placeholder).ok_or_else(|| {
                RmlError::Mapping(format!(
                    "Template placeholder '{}' not found in record",
                    placeholder
                ))
            })?;

            let strings = match value {
                RecordValue::Array(arr) => arr
                    .iter()
                    .filter(|v| !v.is_empty())
                    .map(|v| Self::url_encode(&v.as_string()))
                    .collect(),
                RecordValue::Null => {
                    return Err(RmlError::Mapping(format!(
                        "Template placeholder '{}' is null",
                        placeholder
                    )))
                }
                other => {
                    let s = other.as_string();
                    if s.is_empty() {
                        return Err(RmlError::Mapping(format!(
                            "Template placeholder '{}' is empty",
                            placeholder
                        )));
                    }
                    vec![Self::url_encode(&s)]
                }
            };

            all_values.push(strings);
        }

        // Fast path: single value per placeholder (most common case)
        let all_single = all_values.iter().all(|v| v.len() == 1);
        if all_single {
            let mut result = self.template.clone();
            for (i, values) in all_values.iter().enumerate() {
                result = result.replace(&self.placeholder_patterns[i], &values[0]);
            }
            return Ok(vec![result]);
        }

        // General path: cartesian product for multiple values
        let mut results = vec![self.template.clone()];

        for (i, values) in all_values.iter().enumerate() {
            let pattern = &self.placeholder_patterns[i];
            let mut new_results = Vec::with_capacity(results.len() * values.len());

            for result in &results {
                for value in values {
                    new_results.push(result.replace(pattern, value));
                }
            }

            results = new_results;
        }

        Ok(results)
    }
}

/// Generates NamedNode (IRI) terms
///
/// This generator creates IRI terms from extracted values, with support for
/// relative IRI resolution and strict/best-effort validation modes.
///
/// # Examples
///
/// ```
/// use rml_mapper::termgenerator::{NamedNodeGenerator, ConstantExtractor, StrictMode, TermGenerator};
/// use rml_mapper::records::Record;
///
/// let extractor = ConstantExtractor::new("http://example.org/resource");
/// let generator = NamedNodeGenerator::new(
///     Box::new(extractor),
///     None,
///     StrictMode::Strict
/// );
///
/// let terms = generator.generate(&Record::new()).unwrap();
/// assert_eq!(terms.len(), 1);
/// ```
pub struct NamedNodeGenerator {
    extractor: Box<dyn ValueExtractor>,
    base_iri: Option<String>,
    strict_mode: StrictMode,
}

impl NamedNodeGenerator {
    /// Creates a new named node generator
    ///
    /// # Arguments
    ///
    /// * `extractor` - The value extractor to use
    /// * `base_iri` - Optional base IRI for resolving relative IRIs
    /// * `strict_mode` - Whether to fail or skip invalid IRIs
    pub fn new(
        extractor: Box<dyn ValueExtractor>,
        base_iri: Option<String>,
        strict_mode: StrictMode,
    ) -> Self {
        Self {
            extractor,
            base_iri,
            strict_mode,
        }
    }

    /// Resolves a relative IRI against the base IRI
    fn resolve_iri(&self, iri: &str) -> String {
        if let Some(base) = &self.base_iri {
            // Check if the IRI is already absolute
            if iri.contains("://") || iri.starts_with("urn:") {
                return iri.to_string();
            }

            // Resolve relative IRI
            if iri.starts_with('/') {
                // Absolute path - replace path in base
                if let Some(pos) = base.find("://") {
                    if let Some(slash_pos) = base[pos + 3..].find('/') {
                        return format!("{}{}", &base[..pos + 3 + slash_pos], iri);
                    }
                }
                return format!("{}{}", base.trim_end_matches('/'), iri);
            } else if iri.starts_with('#') {
                // Fragment - append to base
                return format!("{}{}", base.trim_end_matches('#'), iri);
            } else {
                // Relative path - append to base
                return format!("{}/{}", base.trim_end_matches('/'), iri);
            }
        }

        iri.to_string()
    }
}

impl TermGenerator for NamedNodeGenerator {
    fn generate(&self, record: &Record) -> Result<Vec<TermRef>> {
        let values = self.extractor.extract(record)?;
        let mut terms = Vec::new();

        for value in values {
            let iri = self.resolve_iri(&value);

            match NamedNode::new(&iri) {
                Ok(node) => terms.push(TermRef::NamedNode(node)),
                Err(e) => match self.strict_mode {
                    StrictMode::Strict => {
                        return Err(RmlError::Validation(format!(
                            "Invalid IRI '{}': {}",
                            iri, e
                        )))
                    }
                    StrictMode::BestEffort => {
                        // Skip invalid IRI and continue
                        continue;
                    }
                },
            }
        }

        if terms.is_empty() {
            return Err(RmlError::Mapping(
                "No valid IRIs generated".to_string(),
            ));
        }

        Ok(terms)
    }
}

/// Generates Literal terms
///
/// This generator creates literal terms from extracted values, with optional
/// language tags or datatype IRIs.
///
/// # Examples
///
/// ```
/// use rml_mapper::termgenerator::{LiteralGenerator, ReferenceExtractor, TermGenerator};
/// use rml_mapper::records::{Record, RecordValue};
///
/// let mut record = Record::new();
/// record.insert("name".to_string(), RecordValue::String("Alice".to_string()));
///
/// let extractor = ReferenceExtractor::new("name");
/// let generator = LiteralGenerator::new(Box::new(extractor), None, None);
///
/// let terms = generator.generate(&record).unwrap();
/// assert_eq!(terms.len(), 1);
/// ```
pub struct LiteralGenerator {
    extractor: Box<dyn ValueExtractor>,
    language_extractor: Option<Box<dyn ValueExtractor>>,
    datatype_iri: Option<String>,
}

impl LiteralGenerator {
    /// Creates a new literal generator
    ///
    /// # Arguments
    ///
    /// * `extractor` - The value extractor to use
    /// * `language_extractor` - Optional extractor for language tags
    /// * `datatype_iri` - Optional datatype IRI
    ///
    /// Note: A literal cannot have both a language tag and a datatype.
    pub fn new(
        extractor: Box<dyn ValueExtractor>,
        language_extractor: Option<Box<dyn ValueExtractor>>,
        datatype_iri: Option<String>,
    ) -> Self {
        Self {
            extractor,
            language_extractor,
            datatype_iri,
        }
    }
}

impl TermGenerator for LiteralGenerator {
    fn generate(&self, record: &Record) -> Result<Vec<TermRef>> {
        let values = self.extractor.extract(record)?;
        let mut terms = Vec::new();

        // Extract language tag if present
        let language = if let Some(lang_extractor) = &self.language_extractor {
            let lang_values = lang_extractor.extract(record)?;
            if lang_values.is_empty() {
                None
            } else {
                Some(lang_values[0].clone())
            }
        } else {
            None
        };

        for value in values {
            let literal = if let Some(lang) = &language {
                Literal::with_language(value, lang.clone())
            } else if let Some(datatype) = &self.datatype_iri {
                Literal::with_datatype(value, datatype.clone())
            } else {
                Literal::new(value)
            };

            terms.push(TermRef::Literal(literal));
        }

        Ok(terms)
    }
}

/// Generates BlankNode terms
///
/// This generator creates blank node terms, either with deterministic IDs
/// (when an extractor is provided) or with unique auto-generated IDs.
///
/// # Examples
///
/// ```
/// use rml_mapper::termgenerator::{BlankNodeGenerator, ReferenceExtractor, TermGenerator};
/// use rml_mapper::records::{Record, RecordValue};
///
/// let mut record = Record::new();
/// record.insert("id".to_string(), RecordValue::String("123".to_string()));
///
/// // Deterministic blank node
/// let extractor = ReferenceExtractor::new("id");
/// let generator = BlankNodeGenerator::new(Some(Box::new(extractor)));
/// let terms = generator.generate(&record).unwrap();
/// assert_eq!(terms.len(), 1);
///
/// // Auto-generated blank node
/// let generator = BlankNodeGenerator::new(None);
/// let terms = generator.generate(&record).unwrap();
/// assert_eq!(terms.len(), 1);
/// ```
pub struct BlankNodeGenerator {
    extractor: Option<Box<dyn ValueExtractor>>,
}

impl BlankNodeGenerator {
    /// Creates a new blank node generator
    ///
    /// # Arguments
    ///
    /// * `extractor` - Optional value extractor for deterministic blank node IDs.
    ///                 If None, unique IDs will be auto-generated.
    pub fn new(extractor: Option<Box<dyn ValueExtractor>>) -> Self {
        Self { extractor }
    }
}

impl TermGenerator for BlankNodeGenerator {
    fn generate(&self, record: &Record) -> Result<Vec<TermRef>> {
        if let Some(extractor) = &self.extractor {
            // Deterministic blank nodes based on extracted values
            let values = extractor.extract(record)?;
            Ok(values
                .into_iter()
                .map(|v| TermRef::BlankNode(BlankNode::new(v)))
                .collect())
        } else {
            // Auto-generated unique blank node
            Ok(vec![TermRef::BlankNode(BlankNode::generate())])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Term;
    use std::collections::HashMap;

    fn create_test_record() -> Record {
        let mut record = Record::new();
        record.insert("id".to_string(), RecordValue::String("123".to_string()));
        record.insert(
            "name".to_string(),
            RecordValue::String("Alice".to_string()),
        );
        record.insert("age".to_string(), RecordValue::Integer(30));
        record.insert(
            "city".to_string(),
            RecordValue::String("Boston".to_string()),
        );
        record.insert(
            "tags".to_string(),
            RecordValue::Array(vec![
                RecordValue::String("rust".to_string()),
                RecordValue::String("rml".to_string()),
            ]),
        );
        record
    }

    #[test]
    fn test_reference_extractor_simple() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("name");

        let values = extractor.extract(&record).unwrap();
        assert_eq!(values, vec!["Alice"]);
    }

    #[test]
    fn test_reference_extractor_integer() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("age");

        let values = extractor.extract(&record).unwrap();
        assert_eq!(values, vec!["30"]);
    }

    #[test]
    fn test_reference_extractor_array() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("tags");

        let values = extractor.extract(&record).unwrap();
        assert_eq!(values, vec!["rust", "rml"]);
    }

    #[test]
    fn test_reference_extractor_not_found() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("missing");

        let result = extractor.extract(&record);
        assert!(result.is_err());
    }

    #[test]
    fn test_constant_extractor() {
        let record = create_test_record();
        let extractor = ConstantExtractor::new("http://example.org/constant");

        let values = extractor.extract(&record).unwrap();
        assert_eq!(values, vec!["http://example.org/constant"]);
    }

    #[test]
    fn test_template_extractor_single_placeholder() {
        let record = create_test_record();
        let extractor = TemplateExtractor::new("http://example.org/person/{id}").unwrap();

        let values = extractor.extract(&record).unwrap();
        assert_eq!(values, vec!["http://example.org/person/123"]);
    }

    #[test]
    fn test_template_extractor_multiple_placeholders() {
        let record = create_test_record();
        let extractor =
            TemplateExtractor::new("http://example.org/{id}/profile/{name}").unwrap();

        let values = extractor.extract(&record).unwrap();
        assert_eq!(values, vec!["http://example.org/123/profile/Alice"]);
    }

    #[test]
    fn test_template_extractor_url_encoding() {
        let mut record = Record::new();
        record.insert(
            "name".to_string(),
            RecordValue::String("Alice Smith".to_string()),
        );

        let extractor = TemplateExtractor::new("http://example.org/person/{name}").unwrap();

        let values = extractor.extract(&record).unwrap();
        assert_eq!(values, vec!["http://example.org/person/Alice%20Smith"]);
    }

    #[test]
    fn test_template_extractor_array_expansion() {
        let record = create_test_record();
        let extractor = TemplateExtractor::new("http://example.org/tag/{tags}").unwrap();

        let values = extractor.extract(&record).unwrap();
        assert_eq!(values.len(), 2);
        assert!(values.contains(&"http://example.org/tag/rust".to_string()));
        assert!(values.contains(&"http://example.org/tag/rml".to_string()));
    }

    #[test]
    fn test_template_extractor_no_placeholders() {
        let result = TemplateExtractor::new("http://example.org/constant");
        assert!(result.is_err());
    }

    #[test]
    fn test_template_extractor_missing_placeholder() {
        let record = create_test_record();
        let extractor = TemplateExtractor::new("http://example.org/{missing}").unwrap();

        let result = extractor.extract(&record);
        assert!(result.is_err());
    }

    #[test]
    fn test_named_node_generator_constant() {
        let record = create_test_record();
        let extractor = ConstantExtractor::new("http://example.org/resource");
        let generator = NamedNodeGenerator::new(Box::new(extractor), None, StrictMode::Strict);

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        assert!(terms[0].is_iri());
    }

    #[test]
    fn test_named_node_generator_template() {
        let record = create_test_record();
        let extractor = TemplateExtractor::new("http://example.org/person/{id}").unwrap();
        let generator = NamedNodeGenerator::new(Box::new(extractor), None, StrictMode::Strict);

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        assert!(terms[0].is_iri());
        assert_eq!(terms[0].value(), "http://example.org/person/123");
    }

    #[test]
    fn test_named_node_generator_with_base_iri() {
        let record = create_test_record();
        let extractor = ConstantExtractor::new("resource");
        let generator = NamedNodeGenerator::new(
            Box::new(extractor),
            Some("http://example.org/".to_string()),
            StrictMode::Strict,
        );

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].value(), "http://example.org/resource");
    }

    #[test]
    fn test_named_node_generator_base_iri_absolute_path() {
        let record = create_test_record();
        let extractor = ConstantExtractor::new("/resource");
        let generator = NamedNodeGenerator::new(
            Box::new(extractor),
            Some("http://example.org/base/".to_string()),
            StrictMode::Strict,
        );

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].value(), "http://example.org/resource");
    }

    #[test]
    fn test_named_node_generator_base_iri_fragment() {
        let record = create_test_record();
        let extractor = ConstantExtractor::new("#fragment");
        let generator = NamedNodeGenerator::new(
            Box::new(extractor),
            Some("http://example.org/resource".to_string()),
            StrictMode::Strict,
        );

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].value(), "http://example.org/resource#fragment");
    }

    #[test]
    fn test_named_node_generator_strict_mode_invalid_iri() {
        let record = create_test_record();
        let extractor = ConstantExtractor::new("invalid iri with spaces");
        let generator = NamedNodeGenerator::new(Box::new(extractor), None, StrictMode::Strict);

        let result = generator.generate(&record);
        assert!(result.is_err());
    }

    #[test]
    fn test_named_node_generator_best_effort_mode() {
        let mut record = Record::new();
        record.insert(
            "iris".to_string(),
            RecordValue::Array(vec![
                RecordValue::String("http://example.org/valid".to_string()),
                RecordValue::String("invalid iri".to_string()),
                RecordValue::String("http://example.org/valid2".to_string()),
            ]),
        );

        let extractor = ReferenceExtractor::new("iris");
        let generator = NamedNodeGenerator::new(Box::new(extractor), None, StrictMode::BestEffort);

        let terms = generator.generate(&record).unwrap();
        // Should skip the invalid IRI and return only the valid ones
        assert_eq!(terms.len(), 2);
    }

    #[test]
    fn test_literal_generator_plain() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("name");
        let generator = LiteralGenerator::new(Box::new(extractor), None, None);

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        assert!(terms[0].is_literal());
        assert_eq!(terms[0].value(), "Alice");
    }

    #[test]
    fn test_literal_generator_with_language() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("name");
        let lang_extractor = ConstantExtractor::new("en");
        let generator = LiteralGenerator::new(Box::new(extractor), Some(Box::new(lang_extractor)), None);

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        assert!(terms[0].is_literal());

        if let TermRef::Literal(lit) = &terms[0] {
            assert_eq!(lit.value(), "Alice");
            assert_eq!(lit.language(), Some("en"));
        } else {
            panic!("Expected literal term");
        }
    }

    #[test]
    fn test_literal_generator_with_datatype() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("age");
        let generator = LiteralGenerator::new(
            Box::new(extractor),
            None,
            Some("http://www.w3.org/2001/XMLSchema#integer".to_string()),
        );

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        assert!(terms[0].is_literal());

        if let TermRef::Literal(lit) = &terms[0] {
            assert_eq!(lit.value(), "30");
            assert_eq!(
                lit.datatype(),
                Some("http://www.w3.org/2001/XMLSchema#integer")
            );
        } else {
            panic!("Expected literal term");
        }
    }

    #[test]
    fn test_literal_generator_array() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("tags");
        let generator = LiteralGenerator::new(Box::new(extractor), None, None);

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 2);
        assert!(terms.iter().all(|t| t.is_literal()));
    }

    #[test]
    fn test_blank_node_generator_deterministic() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("id");
        let generator = BlankNodeGenerator::new(Some(Box::new(extractor)));

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        assert!(terms[0].is_blank_node());
        assert_eq!(terms[0].value(), "123");
    }

    #[test]
    fn test_blank_node_generator_auto() {
        let record = create_test_record();
        let generator = BlankNodeGenerator::new(None);

        let terms1 = generator.generate(&record).unwrap();
        let terms2 = generator.generate(&record).unwrap();

        assert_eq!(terms1.len(), 1);
        assert_eq!(terms2.len(), 1);
        assert!(terms1[0].is_blank_node());
        assert!(terms2[0].is_blank_node());

        // Auto-generated blank nodes should be unique
        assert_ne!(terms1[0].value(), terms2[0].value());
    }

    #[test]
    fn test_blank_node_generator_array() {
        let record = create_test_record();
        let extractor = ReferenceExtractor::new("tags");
        let generator = BlankNodeGenerator::new(Some(Box::new(extractor)));

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 2);
        assert!(terms.iter().all(|t| t.is_blank_node()));
    }

    #[test]
    fn test_template_with_special_characters() {
        let mut record = Record::new();
        record.insert(
            "name".to_string(),
            RecordValue::String("Alice & Bob".to_string()),
        );

        let extractor = TemplateExtractor::new("http://example.org/{name}").unwrap();
        let values = extractor.extract(&record).unwrap();

        // Should be URL-encoded
        assert_eq!(values, vec!["http://example.org/Alice%20%26%20Bob"]);
    }

    #[test]
    fn test_nested_field_extraction() {
        let mut address = HashMap::new();
        address.insert(
            "city".to_string(),
            RecordValue::String("Boston".to_string()),
        );

        let mut record = Record::new();
        record.insert("address".to_string(), RecordValue::Object(address));

        let extractor = ReferenceExtractor::new("address.city");
        let values = extractor.extract(&record).unwrap();
        assert_eq!(values, vec!["Boston"]);
    }

    #[test]
    fn test_template_with_multiple_array_placeholders() {
        let mut record = Record::new();
        record.insert(
            "tags".to_string(),
            RecordValue::Array(vec![
                RecordValue::String("a".to_string()),
                RecordValue::String("b".to_string()),
            ]),
        );
        record.insert(
            "nums".to_string(),
            RecordValue::Array(vec![
                RecordValue::String("1".to_string()),
                RecordValue::String("2".to_string()),
            ]),
        );

        let extractor = TemplateExtractor::new("http://example.org/{tags}/{nums}").unwrap();
        let values = extractor.extract(&record).unwrap();

        // Should generate all combinations: a/1, a/2, b/1, b/2
        assert_eq!(values.len(), 4);
        assert!(values.contains(&"http://example.org/a/1".to_string()));
        assert!(values.contains(&"http://example.org/a/2".to_string()));
        assert!(values.contains(&"http://example.org/b/1".to_string()));
        assert!(values.contains(&"http://example.org/b/2".to_string()));
    }

    #[test]
    fn test_empty_value_handling() {
        let mut record = Record::new();
        record.insert("empty".to_string(), RecordValue::String("".to_string()));

        let extractor = ReferenceExtractor::new("empty");
        let result = extractor.extract(&record);
        assert!(result.is_err());
    }

    #[test]
    fn test_null_value_handling() {
        let mut record = Record::new();
        record.insert("null".to_string(), RecordValue::Null);

        let extractor = ReferenceExtractor::new("null");
        let result = extractor.extract(&record);
        assert!(result.is_err());
    }

    #[test]
    fn test_named_node_generator_already_absolute_iri() {
        let record = create_test_record();
        let extractor = ConstantExtractor::new("http://other.org/resource");
        let generator = NamedNodeGenerator::new(
            Box::new(extractor),
            Some("http://example.org/".to_string()),
            StrictMode::Strict,
        );

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        // Should not modify already absolute IRI
        assert_eq!(terms[0].value(), "http://other.org/resource");
    }

    #[test]
    fn test_named_node_generator_urn() {
        let record = create_test_record();
        let extractor = ConstantExtractor::new("urn:isbn:0451450523");
        let generator = NamedNodeGenerator::new(
            Box::new(extractor),
            Some("http://example.org/".to_string()),
            StrictMode::Strict,
        );

        let terms = generator.generate(&record).unwrap();
        assert_eq!(terms.len(), 1);
        // Should not modify URN
        assert_eq!(terms[0].value(), "urn:isbn:0451450523");
    }
}
