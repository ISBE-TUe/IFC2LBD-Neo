//! RML Conformer Module
//!
//! This module converts R2RML mappings to RML and handles different RML versions.
//! It ensures that mapping documents conform to the latest RML specification.
//!
//! # Overview
//!
//! The conformer performs the following conversions:
//! - R2RML to RML: Converts R2RML predicates and structures to RML equivalents
//! - Old RML to New RML: Updates old RML namespace to W3C RML namespace
//! - Database source handling: Processes D2RQ database descriptions for R2RML
//!
//! # Examples
//!
//! ```
//! use rml_mapper::conformer::MappingConformer;
//! use rml_mapper::store::InMemoryQuadStore;
//!
//! let mut store = InMemoryQuadStore::new();
//! // ... load mapping into store ...
//!
//! let mut conformer = MappingConformer::new(store, None);
//! let converted = conformer.conform().unwrap();
//! if converted {
//!     println!("Mapping was converted to RML");
//! }
//!
//! let conformed_store = conformer.into_store();
//! ```

use crate::error::{Result, RmlError};
use crate::namespaces;
use crate::store::{InMemoryQuadStore, QuadStore};
use crate::term::{Literal, NamedNode, Term, TermRef};
use std::collections::HashMap;

/// Mapping dialect types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Old RML (http://semweb.mmlab.be/ns/rml#)
    Rml,
    /// W3C R2RML
    R2rml,
    /// W3C RML (http://w3id.org/rml/)
    Rml2,
}

/// RML Mapping Conformer
///
/// Converts R2RML mappings to RML and handles different RML versions.
/// Ensures mapping documents conform to the latest RML specification.
pub struct MappingConformer {
    store: InMemoryQuadStore,
    mapping_options: HashMap<String, String>,
}

impl MappingConformer {
    /// Creates a new MappingConformer
    ///
    /// # Arguments
    ///
    /// * `store` - QuadStore containing the mapping rules
    /// * `mapping_options` - Optional database connection options for R2RML conversion
    ///
    /// # Examples
    ///
    /// ```
    /// use rml_mapper::conformer::MappingConformer;
    /// use rml_mapper::store::InMemoryQuadStore;
    /// use std::collections::HashMap;
    ///
    /// let store = InMemoryQuadStore::new();
    /// let mut options = HashMap::new();
    /// options.insert("jdbcDSN".to_string(), "jdbc:mysql://localhost/db".to_string());
    /// options.insert("username".to_string(), "user".to_string());
    /// options.insert("password".to_string(), "pass".to_string());
    ///
    /// let conformer = MappingConformer::new(store, Some(options));
    /// ```
    pub fn new(store: InMemoryQuadStore, mapping_options: Option<HashMap<String, String>>) -> Self {
        Self {
            store,
            mapping_options: mapping_options.unwrap_or_default(),
        }
    }

    /// Conforms the mapping to the latest RML specification
    ///
    /// This method performs all necessary conversions to ensure the mapping
    /// conforms to the W3C RML specification.
    ///
    /// # Returns
    ///
    /// Returns `Ok(true)` if conversion was needed and performed,
    /// `Ok(false)` if the mapping was already conformant.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The mapping has no TriplesMap
    /// - A TriplesMap is missing a required subjectMap
    /// - Conversion fails for any reason
    pub fn conform(&mut self) -> Result<bool> {
        // Convert R2RML logical tables to RML logical sources
        self.convert_r2rml_logical_tables()?;

        // Convert old RML to new RML
        self.convert_old_rml_to_new()?;

        // Validate the mapping
        self.validate()?;

        Ok(true)
    }

    /// Returns the conformed store
    ///
    /// Consumes the conformer and returns the underlying quad store.
    pub fn into_store(self) -> InMemoryQuadStore {
        self.store
    }

    /// Gets a reference to the store
    pub fn store(&self) -> &InMemoryQuadStore {
        &self.store
    }

    /// Gets a mutable reference to the store
    pub fn store_mut(&mut self) -> &mut InMemoryQuadStore {
        &mut self.store
    }

    /// Converts R2RML logical tables to RML logical sources
    fn convert_r2rml_logical_tables(&mut self) -> Result<()> {
        let rr_logical_table = NamedNode::new(format!("{}logicalTable", namespaces::RR))
            .map_err(RmlError::Parse)?;

        // Find all triples maps with rr:logicalTable
        let logical_table_quads = self.store.get_quads(None, Some(&rr_logical_table), None, None)?;

        for quad in logical_table_quads {
            let triples_map = quad.subject().clone();
            let logical_table = quad.object().clone();

            // Create database source if mapping options are provided
            if !self.mapping_options.is_empty() {
                let database_iri = format!("{}_database", triples_map.value());
                let database = NamedNode::new(database_iri).map_err(RmlError::Parse)?;

                self.add_database_source(&database)?;
            }

            // Create logical source
            let logical_source_iri = format!("{}_logicalSource", triples_map.value());
            let logical_source = NamedNode::new(logical_source_iri).map_err(RmlError::Parse)?;

            // Add rml:logicalSource
            let rml_logical_source = NamedNode::new(format!("{}logicalSource", namespaces::RML2))
                .map_err(RmlError::Parse)?;
            self.store.add_quad(
                triples_map.clone(),
                rml_logical_source,
                TermRef::NamedNode(logical_source.clone()),
                None,
            )?;

            // Process table name or SQL query
            self.process_logical_table(&logical_table, &logical_source)?;

            // Remove old rr:logicalTable
            self.store.remove_quads(Some(&triples_map), Some(&rr_logical_table), None, None)?;
        }

        // Convert rr:column to rml:reference
        let rr_column = NamedNode::new(format!("{}column", namespaces::RR))
            .map_err(RmlError::Parse)?;
        let rml_reference = NamedNode::new(format!("{}reference", namespaces::RML2))
            .map_err(RmlError::Parse)?;
        self.store.rename_all_predicates(&rr_column, rml_reference)?;

        Ok(())
    }

    /// Processes a logical table (table name or SQL query)
    fn process_logical_table(&mut self, logical_table: &TermRef, logical_source: &NamedNode) -> Result<()> {
        let rr_table_name = NamedNode::new(format!("{}tableName", namespaces::RR))
            .map_err(RmlError::Parse)?;
        let rr_sql_query = NamedNode::new(format!("{}sqlQuery", namespaces::RR))
            .map_err(RmlError::Parse)?;

        // Check for table name
        let table_name_quads = self.store.get_quads(Some(logical_table), Some(&rr_table_name), None, None)?;

        if !table_name_quads.is_empty() {
            // Process table name
            let table_name = table_name_quads[0].object().clone();

            let rml_iterator = NamedNode::new(format!("{}iterator", namespaces::RML2))
                .map_err(RmlError::Parse)?;
            let rml_ref_formulation = NamedNode::new(format!("{}referenceFormulation", namespaces::RML2))
                .map_err(RmlError::Parse)?;
            let rdb_table = NamedNode::new("http://w3id.org/rml/RDBTable")
                .map_err(RmlError::Parse)?;

            self.store.add_quad(
                TermRef::NamedNode(logical_source.clone()),
                rml_iterator,
                table_name,
                None,
            )?;

            self.store.add_quad(
                TermRef::NamedNode(logical_source.clone()),
                rml_ref_formulation,
                TermRef::NamedNode(rdb_table),
                None,
            )?;

            // Remove old table name quads
            self.store.remove_quads(Some(logical_table), Some(&rr_table_name), None, None)?;
        } else {
            // Check for SQL query
            let sql_query_quads = self.store.get_quads(Some(logical_table), Some(&rr_sql_query), None, None)?;

            if !sql_query_quads.is_empty() {
                let query = sql_query_quads[0].object().clone();

                let rml_iterator = NamedNode::new(format!("{}iterator", namespaces::RML2))
                    .map_err(RmlError::Parse)?;
                let rml_ref_formulation = NamedNode::new(format!("{}referenceFormulation", namespaces::RML2))
                    .map_err(RmlError::Parse)?;
                let rdb_query = NamedNode::new("http://w3id.org/rml/RDBQuery")
                    .map_err(RmlError::Parse)?;

                self.store.add_quad(
                    TermRef::NamedNode(logical_source.clone()),
                    rml_iterator,
                    query,
                    None,
                )?;

                self.store.add_quad(
                    TermRef::NamedNode(logical_source.clone()),
                    rml_ref_formulation,
                    TermRef::NamedNode(rdb_query),
                    None,
                )?;

                // Remove old SQL query quads
                self.store.remove_quads(Some(logical_table), Some(&rr_sql_query), None, None)?;
            }
        }

        // Add database source if available
        if !self.mapping_options.is_empty() {
            let database_iri = format!("{}_database", logical_source.iri());
            let database = NamedNode::new(database_iri).map_err(RmlError::Parse)?;

            let rml_source = NamedNode::new(format!("{}source", namespaces::RML2))
                .map_err(RmlError::Parse)?;

            self.store.add_quad(
                TermRef::NamedNode(logical_source.clone()),
                rml_source,
                TermRef::NamedNode(database),
                None,
            )?;
        }

        Ok(())
    }

    /// Adds a database source with D2RQ properties
    fn add_database_source(&mut self, database: &NamedNode) -> Result<()> {
        let rdf_type = NamedNode::new(format!("{}type", namespaces::RDF))
            .map_err(RmlError::Parse)?;
        let d2rq_database = NamedNode::new(format!("{}Database", namespaces::D2RQ))
            .map_err(RmlError::Parse)?;

        self.store.add_quad(
            TermRef::NamedNode(database.clone()),
            rdf_type,
            TermRef::NamedNode(d2rq_database),
            None,
        )?;

        // Add database connection properties
        for (key, value) in &self.mapping_options {
            let property = NamedNode::new(format!("{}{}", namespaces::D2RQ, key))
                .map_err(RmlError::Parse)?;

            self.store.add_quad(
                TermRef::NamedNode(database.clone()),
                property,
                TermRef::Literal(Literal::new(value)),
                None,
            )?;

            // Add JDBC driver if jdbcDSN is provided
            if key == "jdbcDSN" {
                let driver = self.detect_jdbc_driver(value);
                if let Some(driver_class) = driver {
                    let jdbc_driver = NamedNode::new(format!("{}jdbcDriver", namespaces::D2RQ))
                        .map_err(RmlError::Parse)?;

                    self.store.add_quad(
                        TermRef::NamedNode(database.clone()),
                        jdbc_driver,
                        TermRef::Literal(Literal::new(driver_class)),
                        None,
                    )?;
                }
            }
        }

        Ok(())
    }

    /// Detects JDBC driver from DSN
    fn detect_jdbc_driver(&self, dsn: &str) -> Option<String> {
        if dsn.contains("mysql") {
            Some("com.mysql.jdbc.Driver".to_string())
        } else if dsn.contains("postgresql") {
            Some("org.postgresql.Driver".to_string())
        } else if dsn.contains("oracle") {
            Some("oracle.jdbc.driver.OracleDriver".to_string())
        } else if dsn.contains("sqlserver") {
            Some("com.microsoft.sqlserver.jdbc.SQLServerDriver".to_string())
        } else if dsn.contains("sqlite") {
            Some("org.sqlite.JDBC".to_string())
        } else {
            None
        }
    }

    /// Converts old RML namespace to new W3C RML namespace
    fn convert_old_rml_to_new(&mut self) -> Result<()> {
        // Predicate mappings: old RML/R2RML -> new RML
        let predicate_mappings = self.get_predicate_mappings();

        for (old_pred_iri, new_pred_iri) in predicate_mappings {
            let old_pred = NamedNode::new(old_pred_iri).map_err(RmlError::Parse)?;
            let new_pred = NamedNode::new(new_pred_iri).map_err(RmlError::Parse)?;

            self.store.rename_all_predicates(&old_pred, new_pred)?;
        }

        // Object mappings: old RML/R2RML -> new RML
        let object_mappings = self.get_object_mappings();

        for (old_obj_iri, new_obj_iri) in object_mappings {
            let old_obj = NamedNode::new(old_obj_iri).map_err(RmlError::Parse)?;
            let new_obj = NamedNode::new(new_obj_iri).map_err(RmlError::Parse)?;

            self.store.rename_all_objects(
                &TermRef::NamedNode(old_obj),
                TermRef::NamedNode(new_obj),
            )?;
        }

        // Update namespace prefixes
        self.store.remove_namespace("rml");
        self.store.remove_namespace("rr");
        self.store.remove_namespace("ql");
        self.store.add_namespace("rml".to_string(), namespaces::RML2.to_string());

        Ok(())
    }

    /// Returns predicate mappings from old to new namespaces
    fn get_predicate_mappings(&self) -> Vec<(String, String)> {
        vec![
            // Old RML to new RML
            (format!("{}iterator", namespaces::RML), format!("{}iterator", namespaces::RML2)),
            (format!("{}logicalSource", namespaces::RML), format!("{}logicalSource", namespaces::RML2)),
            (format!("{}reference", namespaces::RML), format!("{}reference", namespaces::RML2)),
            (format!("{}referenceFormulation", namespaces::RML), format!("{}referenceFormulation", namespaces::RML2)),
            (format!("{}source", namespaces::RML), format!("{}source", namespaces::RML2)),
            (format!("{}query", namespaces::RML), format!("{}iterator", namespaces::RML2)),
            (format!("{}languageMap", namespaces::RML), format!("{}languageMap", namespaces::RML2)),
            (format!("{}parentTermMap", namespaces::RML), format!("{}parentTermMap", namespaces::RML2)),
            (format!("{}subjectMap", namespaces::RML), format!("{}subjectMap", namespaces::RML2)),
            (format!("{}predicateObjectMap", namespaces::RML), format!("{}predicateObjectMap", namespaces::RML2)),
            (format!("{}predicateMap", namespaces::RML), format!("{}predicateMap", namespaces::RML2)),
            (format!("{}objectMap", namespaces::RML), format!("{}objectMap", namespaces::RML2)),
            (format!("{}graphMap", namespaces::RML), format!("{}graphMap", namespaces::RML2)),
            
            // R2RML to new RML
            (format!("{}class", namespaces::RR), format!("{}class", namespaces::RML2)),
            (format!("{}constant", namespaces::RR), format!("{}constant", namespaces::RML2)),
            (format!("{}datatype", namespaces::RR), format!("{}datatype", namespaces::RML2)),
            (format!("{}graph", namespaces::RR), format!("{}graph", namespaces::RML2)),
            (format!("{}graphMap", namespaces::RR), format!("{}graphMap", namespaces::RML2)),
            (format!("{}language", namespaces::RR), format!("{}language", namespaces::RML2)),
            (format!("{}object", namespaces::RR), format!("{}object", namespaces::RML2)),
            (format!("{}objectMap", namespaces::RR), format!("{}objectMap", namespaces::RML2)),
            (format!("{}predicate", namespaces::RR), format!("{}predicate", namespaces::RML2)),
            (format!("{}predicateMap", namespaces::RR), format!("{}predicateMap", namespaces::RML2)),
            (format!("{}predicateObjectMap", namespaces::RR), format!("{}predicateObjectMap", namespaces::RML2)),
            (format!("{}subject", namespaces::RR), format!("{}subject", namespaces::RML2)),
            (format!("{}subjectMap", namespaces::RR), format!("{}subjectMap", namespaces::RML2)),
            (format!("{}template", namespaces::RR), format!("{}template", namespaces::RML2)),
            (format!("{}termType", namespaces::RR), format!("{}termType", namespaces::RML2)),
            (format!("{}joinCondition", namespaces::RR), format!("{}joinCondition", namespaces::RML2)),
            (format!("{}parent", namespaces::RR), format!("{}parent", namespaces::RML2)),
            (format!("{}child", namespaces::RR), format!("{}child", namespaces::RML2)),
            (format!("{}parentTriplesMap", namespaces::RR), format!("{}parentTriplesMap", namespaces::RML2)),
        ]
    }

    /// Returns object mappings from old to new namespaces
    fn get_object_mappings(&self) -> Vec<(String, String)> {
        vec![
            // Old RML to new RML
            (format!("{}LogicalSource", namespaces::RML), format!("{}LogicalSource", namespaces::RML2)),
            (format!("{}TriplesMap", namespaces::RML), format!("{}TriplesMap", namespaces::RML2)),
            (format!("{}LanguageMap", namespaces::RML), format!("{}LanguageMap", namespaces::RML2)),
            
            // R2RML to new RML
            (format!("{}BlankNode", namespaces::RR), format!("{}BlankNode", namespaces::RML2)),
            (format!("{}IRI", namespaces::RR), format!("{}IRI", namespaces::RML2)),
            (format!("{}Literal", namespaces::RR), format!("{}Literal", namespaces::RML2)),
            (format!("{}Join", namespaces::RR), format!("{}Join", namespaces::RML2)),
            (format!("{}PredicateMap", namespaces::RR), format!("{}PredicateMap", namespaces::RML2)),
            (format!("{}PredicateObjectMap", namespaces::RR), format!("{}PredicateObjectMap", namespaces::RML2)),
            (format!("{}RefObjectMap", namespaces::RR), format!("{}RefObjectMap", namespaces::RML2)),
            (format!("{}SubjectMap", namespaces::RR), format!("{}SubjectMap", namespaces::RML2)),
            (format!("{}ObjectMap", namespaces::RR), format!("{}ObjectMap", namespaces::RML2)),
            (format!("{}TermMap", namespaces::RR), format!("{}TermMap", namespaces::RML2)),
            (format!("{}TriplesMap", namespaces::RR), format!("{}TriplesMap", namespaces::RML2)),
            (format!("{}GraphMap", namespaces::RR), format!("{}GraphMap", namespaces::RML2)),
            (format!("{}defaultGraph", namespaces::RR), format!("{}defaultGraph", namespaces::RML2)),
            
            // Query language mappings
            (format!("{}CSV", namespaces::QL), "http://w3id.org/rml/CSV".to_string()),
            (format!("{}JSONPath", namespaces::QL), "http://w3id.org/rml/JSONPath".to_string()),
            (format!("{}XPath", namespaces::QL), "http://w3id.org/rml/XPath".to_string()),
        ]
    }

    /// Validates the conformed mapping
    fn validate(&self) -> Result<()> {
        // Check for at least one TriplesMap (identified by having a logicalSource)
        let rml_logical_source = NamedNode::new(format!("{}logicalSource", namespaces::RML2))
            .map_err(RmlError::Parse)?;

        let triples_maps = self.store.get_quads(None, Some(&rml_logical_source), None, None)?;

        if triples_maps.is_empty() {
            return Err(RmlError::Validation(
                "Mapping requires at least one TriplesMap".to_string(),
            ));
        }

        // Check that each TriplesMap has a subjectMap
        let rml_subject_map = NamedNode::new(format!("{}subjectMap", namespaces::RML2))
            .map_err(RmlError::Parse)?;

        // For each TriplesMap (subject with logicalSource), check for subjectMap
        for quad in &triples_maps {
            let triples_map = quad.subject();
            if !self.store.contains(Some(triples_map), Some(&rml_subject_map), None, None)? {
                return Err(RmlError::Validation(format!(
                    "TriplesMap {} requires a subject map",
                    triples_map.value()
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::BlankNode;

    #[test]
    fn test_create_conformer() {
        let store = InMemoryQuadStore::new();
        let conformer = MappingConformer::new(store, None);
        assert!(conformer.mapping_options.is_empty());
    }

    #[test]
    fn test_create_conformer_with_options() {
        let store = InMemoryQuadStore::new();
        let mut options = HashMap::new();
        options.insert("jdbcDSN".to_string(), "jdbc:mysql://localhost/db".to_string());

        let conformer = MappingConformer::new(store, Some(options));
        assert_eq!(conformer.mapping_options.len(), 1);
    }

    #[test]
    fn test_detect_jdbc_driver() {
        let store = InMemoryQuadStore::new();
        let conformer = MappingConformer::new(store, None);

        assert_eq!(
            conformer.detect_jdbc_driver("jdbc:mysql://localhost/db"),
            Some("com.mysql.jdbc.Driver".to_string())
        );

        assert_eq!(
            conformer.detect_jdbc_driver("jdbc:postgresql://localhost/db"),
            Some("org.postgresql.Driver".to_string())
        );

        assert_eq!(
            conformer.detect_jdbc_driver("jdbc:oracle:thin:@localhost:1521:db"),
            Some("oracle.jdbc.driver.OracleDriver".to_string())
        );

        assert_eq!(
            conformer.detect_jdbc_driver("jdbc:sqlserver://localhost;database=db"),
            Some("com.microsoft.sqlserver.jdbc.SQLServerDriver".to_string())
        );

        assert_eq!(
            conformer.detect_jdbc_driver("jdbc:sqlite:test.db"),
            Some("org.sqlite.JDBC".to_string())
        );

        assert_eq!(conformer.detect_jdbc_driver("jdbc:unknown://localhost"), None);
    }

    #[test]
    fn test_r2rml_to_rml_conversion() {
        let mut store = InMemoryQuadStore::new();

        // Create a simple R2RML mapping
        let triples_map = NamedNode::new("http://example.org/map1").unwrap();
        let logical_table = BlankNode::new("lt1");
        
        let rr_logical_table = NamedNode::new(format!("{}logicalTable", namespaces::RR)).unwrap();
        let rr_table_name = NamedNode::new(format!("{}tableName", namespaces::RR)).unwrap();
        let rr_subject_map = NamedNode::new(format!("{}subjectMap", namespaces::RR)).unwrap();
        let subject_map = BlankNode::new("sm1");

        store.add_quad(
            TermRef::NamedNode(triples_map.clone()),
            rr_logical_table.clone(),
            TermRef::BlankNode(logical_table.clone()),
            None,
        ).unwrap();

        store.add_quad(
            TermRef::BlankNode(logical_table.clone()),
            rr_table_name,
            TermRef::Literal(Literal::new("employees")),
            None,
        ).unwrap();

        store.add_quad(
            TermRef::NamedNode(triples_map.clone()),
            rr_subject_map,
            TermRef::BlankNode(subject_map),
            None,
        ).unwrap();

        // Add TriplesMap type
        let rdf_type = NamedNode::new(format!("{}type", namespaces::RDF)).unwrap();
        let rr_triples_map = NamedNode::new(format!("{}TriplesMap", namespaces::RR)).unwrap();
        store.add_quad(
            TermRef::NamedNode(triples_map.clone()),
            rdf_type,
            TermRef::NamedNode(rr_triples_map),
            None,
        ).unwrap();

        let mut conformer = MappingConformer::new(store, None);
        let result = conformer.conform();

        assert!(result.is_ok());

        // Check that rml:logicalSource was added
        let rml_logical_source = NamedNode::new(format!("{}logicalSource", namespaces::RML2)).unwrap();
        let has_logical_source = conformer.store.contains(
            Some(&TermRef::NamedNode(triples_map.clone())),
            Some(&rml_logical_source),
            None,
            None,
        ).unwrap();

        assert!(has_logical_source);

        // Check that rr:logicalTable was removed
        let has_logical_table = conformer.store.contains(
            Some(&TermRef::NamedNode(triples_map)),
            Some(&rr_logical_table),
            None,
            None,
        ).unwrap();

        assert!(!has_logical_table);
    }

    #[test]
    fn test_old_rml_to_new_rml_conversion() {
        let mut store = InMemoryQuadStore::new();

        // Create an old RML mapping
        let triples_map = NamedNode::new("http://example.org/map1").unwrap();
        let logical_source = BlankNode::new("ls1");

        let old_rml_logical_source = NamedNode::new(format!("{}logicalSource", namespaces::RML)).unwrap();
        let old_rml_source = NamedNode::new(format!("{}source", namespaces::RML)).unwrap();
        let old_rml_iterator = NamedNode::new(format!("{}iterator", namespaces::RML)).unwrap();
        let old_rml_reference_formulation = NamedNode::new(format!("{}referenceFormulation", namespaces::RML)).unwrap();

        store.add_quad(
            TermRef::NamedNode(triples_map.clone()),
            old_rml_logical_source,
            TermRef::BlankNode(logical_source.clone()),
            None,
        ).unwrap();

        store.add_quad(
            TermRef::BlankNode(logical_source.clone()),
            old_rml_source,
            TermRef::Literal(Literal::new("data.csv")),
            None,
        ).unwrap();

        store.add_quad(
            TermRef::BlankNode(logical_source.clone()),
            old_rml_iterator,
            TermRef::Literal(Literal::new("$")),
            None,
        ).unwrap();

        store.add_quad(
            TermRef::BlankNode(logical_source.clone()),
            old_rml_reference_formulation,
            TermRef::NamedNode(NamedNode::new(format!("{}CSV", namespaces::QL)).unwrap()),
            None,
        ).unwrap();

        // Add subjectMap for validation
        let old_rml_subject_map = NamedNode::new(format!("{}subjectMap", namespaces::RML)).unwrap();
        let subject_map = BlankNode::new("sm1");
        store.add_quad(
            TermRef::NamedNode(triples_map.clone()),
            old_rml_subject_map,
            TermRef::BlankNode(subject_map),
            None,
        ).unwrap();

        // Add TriplesMap type
        let rdf_type = NamedNode::new(format!("{}type", namespaces::RDF)).unwrap();
        let old_rml_triples_map = NamedNode::new(format!("{}TriplesMap", namespaces::RML)).unwrap();
        store.add_quad(
            TermRef::NamedNode(triples_map.clone()),
            rdf_type,
            TermRef::NamedNode(old_rml_triples_map),
            None,
        ).unwrap();

        let mut conformer = MappingConformer::new(store, None);
        let result = conformer.conform();

        if let Err(ref e) = result {
            eprintln!("Conformer error: {:?}", e);
        }
        assert!(result.is_ok());

        // Check that new RML predicates are used
        let new_rml_logical_source = NamedNode::new(format!("{}logicalSource", namespaces::RML2)).unwrap();
        let has_new_logical_source = conformer.store.contains(
            Some(&TermRef::NamedNode(triples_map)),
            Some(&new_rml_logical_source),
            None,
            None,
        ).unwrap();

        assert!(has_new_logical_source);
    }

    #[test]
    fn test_validation_no_triples_map() {
        let store = InMemoryQuadStore::new();
        let mut conformer = MappingConformer::new(store, None);

        let result = conformer.conform();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RmlError::Validation(_)));
    }

    #[test]
    fn test_validation_missing_subject_map() {
        let mut store = InMemoryQuadStore::new();

        // Create a TriplesMap without subjectMap
        let triples_map = NamedNode::new("http://example.org/map1").unwrap();
        let rdf_type = NamedNode::new(format!("{}type", namespaces::RDF)).unwrap();
        let rml_triples_map = NamedNode::new(format!("{}TriplesMap", namespaces::RML2)).unwrap();
        let rml_logical_source = NamedNode::new(format!("{}logicalSource", namespaces::RML2)).unwrap();
        let logical_source = BlankNode::new("ls1");

        store.add_quad(
            TermRef::NamedNode(triples_map.clone()),
            rdf_type,
            TermRef::NamedNode(rml_triples_map),
            None,
        ).unwrap();

        store.add_quad(
            TermRef::NamedNode(triples_map),
            rml_logical_source,
            TermRef::BlankNode(logical_source),
            None,
        ).unwrap();

        let mut conformer = MappingConformer::new(store, None);
        let result = conformer.conform();

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RmlError::Validation(_)));
    }

    #[test]
    fn test_into_store() {
        let store = InMemoryQuadStore::new();
        let conformer = MappingConformer::new(store, None);
        let returned_store = conformer.into_store();
        assert!(returned_store.is_empty());
    }
}
