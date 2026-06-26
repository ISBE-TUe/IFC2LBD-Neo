//! Mapping Executor
//!
//! This module executes RML mappings by processing data sources according to
//! the mapping rules and generating RDF triples.
//!
//! # Architecture
//!
//! The Executor is the main engine that:
//! 1. Takes a MappingDocument and data sources
//! 2. Iterates over records from each logical source
//! 3. Applies term generators to create RDF triples
//! 4. Handles joins between triples maps
//! 5. Streams output quads through a `crossbeam::channel::Sender<Vec<Quad>>`
//!
//! # Examples
//!
//! ```no_run
//! use rml_mapper::executor::Executor;
//! use rml_mapper::mapping::{MappingFactory, StrictMode};
//! use rml_mapper::store::InMemoryQuadStore;
//! use std::path::PathBuf;
//!
//! let mut mapping_store = InMemoryQuadStore::new();
//! // Load mapping rules into mapping_store...
//!
//! let factory = MappingFactory::new(None, StrictMode::Strict);
//! let mapping = factory.create_mapping(&mapping_store).unwrap();
//!
//! let (tx, rx) = crossbeam::channel::unbounded();
//! let mut executor = Executor::new(mapping, PathBuf::from("."), StrictMode::Strict)
//!     .with_output_sender(tx);
//! executor.execute().unwrap();
//! // Collect quads from rx...
//! ```

use crate::access::{Access, LocalFileAccess};
use crate::error::{Result, RmlError};
use crate::mapping::{MappingDocument, ObjectMap, StrictMode};
use crate::namespaces::RDF;
use crate::records::{Record, RecordsFactory};
use crate::term::{NamedNode, Quad, Term, TermRef};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;

/// Batch size for streaming quads through the output channel.
const BATCH_SIZE: usize = 1000;

/// Pre-computed constants to avoid repeated allocations in hot loops
struct PrecomputedConstants {
    rdf_type: NamedNode,
}

impl PrecomputedConstants {
    fn new() -> Self {
        Self {
            rdf_type: NamedNode::new_unchecked(format!("{}type", RDF)),
        }
    }
}

/// Executes RML mappings
///
/// The Executor processes a MappingDocument and generates RDF quads by:
/// - Accessing data sources via the logical source
/// - Iterating over records using RecordsFactory
/// - Generating subjects, predicates, and objects using TermGenerators
/// - Handling joins between triples maps
/// - Streaming generated quads through an output channel
pub struct Executor {
    /// The mapping document to execute
    mapping: MappingDocument,
    /// Factory for creating record iterators
    records_factory: RecordsFactory,
    /// Optional streaming output sender. When set, the executor streams
    /// batches of quads through this channel instead of buffering them.
    output_sender: Option<crossbeam::channel::Sender<Vec<Quad>>>,
    /// Base path for resolving relative file paths
    base_path: PathBuf,
    /// Strict mode for IRI validation
    _strict_mode: StrictMode,
    /// Execution context for caching
    context: ExecutionContext,
    /// Pre-computed constants
    constants: PrecomputedConstants,
}

/// Execution context for tracking state during mapping execution
struct ExecutionContext {
    /// Cache of records by triples map ID
    records_cache: HashMap<String, Vec<Record>>,
    /// Cache of generated subjects by triples map ID and record index
    subjects_cache: HashMap<String, HashMap<usize, Vec<TermRef>>>,
}

impl ExecutionContext {
    fn new() -> Self {
        Self {
            records_cache: HashMap::new(),
            subjects_cache: HashMap::new(),
        }
    }

    /// Gets cached records for a triples map
    fn get_records(&self, triples_map_id: &str) -> Option<&Vec<Record>> {
        self.records_cache.get(triples_map_id)
    }

    /// Caches records for a triples map
    fn cache_records(&mut self, triples_map_id: String, records: Vec<Record>) {
        self.records_cache.insert(triples_map_id, records);
    }

    /// Gets cached subjects for a triples map and record index
    fn get_subjects(&self, triples_map_id: &str, record_index: usize) -> Option<&Vec<TermRef>> {
        self.subjects_cache
            .get(triples_map_id)
            .and_then(|map| map.get(&record_index))
    }

    /// Caches subjects for a triples map and record index
    fn cache_subjects(
        &mut self,
        triples_map_id: String,
        record_index: usize,
        subjects: Vec<TermRef>,
    ) {
        self.subjects_cache
            .entry(triples_map_id)
            .or_default()
            .insert(record_index, subjects);
    }
}

impl Executor {
    /// Creates a new executor
    ///
    /// # Arguments
    ///
    /// * `mapping` - The mapping document to execute
    /// * `base_path` - Base path for resolving relative file paths
    /// * `strict_mode` - Whether to fail or skip invalid IRIs
    pub fn new(mapping: MappingDocument, base_path: PathBuf, strict_mode: StrictMode) -> Self {
        Self {
            mapping,
            records_factory: RecordsFactory::new(),
            output_sender: None,
            base_path,
            _strict_mode: strict_mode,
            context: ExecutionContext::new(),
            constants: PrecomputedConstants::new(),
        }
    }

    /// Set the streaming output sender. When set, the executor streams
    /// batches of quads through this channel instead of buffering them.
    pub fn with_output_sender(mut self, sender: crossbeam::channel::Sender<Vec<Quad>>) -> Self {
        self.output_sender = Some(sender);
        self
    }

    /// Executes all triples maps, streaming output quads through the
    /// output channel (if set).
    pub fn execute(&mut self) -> Result<()> {
        let num_triples_maps = self.mapping.triples_maps.len();

        for i in 0..num_triples_maps {
            self.execute_triples_map_by_index(i)?;
        }

        Ok(())
    }

    /// Sends a batch of quads through the output channel.
    /// No-op if no output sender is set.
    fn send_quads(&mut self, quads: Vec<Quad>) -> Result<()> {
        if let Some(sender) = &self.output_sender {
            if !quads.is_empty() {
                sender
                    .send(quads)
                    .map_err(|_| RmlError::Execution("output channel closed".into()))?;
            }
        }
        Ok(())
    }

    /// Executes a triples map by index
    fn execute_triples_map_by_index(&mut self, index: usize) -> Result<()> {
        // Get records from the logical source
        let records = self.get_records_by_index(index)?;
        let has_joins = self.mapping.triples_maps[index]
            .predicate_object_maps
            .iter()
            .any(|pom| {
                pom.object_maps
                    .iter()
                    .any(|om| matches!(om, ObjectMap::RefObjectMap { .. }))
            });

        let mut batch = Vec::with_capacity(BATCH_SIZE);

        if has_joins {
            // Sequential path for joins (needs mutable self for caching)
            for (record_index, record) in records.iter().enumerate() {
                let quads = self.process_record_by_index(record, index, record_index)?;
                batch.extend(quads);
                if batch.len() >= BATCH_SIZE {
                    self.send_quads(std::mem::take(&mut batch))?;
                }
            }
        } else {
            // Parallel path for non-join triples maps
            let all_quads: Vec<Vec<Quad>> = {
                let mapping = &self.mapping;
                let constants = &self.constants;
                let triples_map = &mapping.triples_maps[index];

                // Process records in parallel using rayon
                records
                    .par_iter()
                    .map(|record| Self::process_record_parallel(record, triples_map, constants))
                    .collect::<Result<Vec<_>>>()?
            };

            // Stream all quads through the output channel
            for quads in all_quads {
                batch.extend(quads);
                if batch.len() >= BATCH_SIZE {
                    self.send_quads(std::mem::take(&mut batch))?;
                }
            }

            // Cache subjects for potential downstream joins
            let triples_map_id = self.mapping.triples_maps[index].id.value().to_string();
            for (record_index, record) in records.iter().enumerate() {
                let subjects = self.mapping.triples_maps[index]
                    .subject_map
                    .term_generator
                    .generate(record)?;
                self.context
                    .cache_subjects(triples_map_id.clone(), record_index, subjects);
            }
        }

        // Flush remaining quads
        self.send_quads(batch)?;

        Ok(())
    }

    /// Process a record without needing &mut self — used for parallel execution
    fn process_record_parallel(
        record: &Record,
        triples_map: &crate::mapping::TriplesMap,
        constants: &PrecomputedConstants,
    ) -> Result<Vec<Quad>> {
        let mut quads = Vec::new();

        // Generate subject(s)
        let subjects = triples_map.subject_map.term_generator.generate(record)?;

        // Get graph from subject map
        let subject_graph = if triples_map.subject_map.graph_maps.is_empty() {
            None
        } else {
            let graph_terms = triples_map.subject_map.graph_maps[0]
                .term_generator
                .generate(record)?;
            if graph_terms.is_empty() {
                None
            } else if let TermRef::NamedNode(node) = &graph_terms[0] {
                Some(node.clone())
            } else {
                return Err(RmlError::Validation(
                    "Graph name must be a named node".to_string(),
                ));
            }
        };

        // Generate rdf:type triples for classes (using pre-computed constant)
        if !triples_map.subject_map.classes.is_empty() {
            for subject in &subjects {
                for class in &triples_map.subject_map.classes {
                    let quad = Quad::new(
                        subject.clone(),
                        constants.rdf_type.clone(),
                        TermRef::NamedNode(class.clone()),
                        subject_graph.clone(),
                    )
                    .map_err(RmlError::Validation)?;
                    quads.push(quad);
                }
            }
        }

        // Process each predicate-object map
        for pom in &triples_map.predicate_object_maps {
            // Generate predicates
            let mut all_predicates = Vec::new();
            for predicate_map in &pom.predicate_maps {
                let predicates = predicate_map.term_generator.generate(record)?;
                all_predicates.extend(predicates);
            }

            // Generate objects
            let mut all_objects = Vec::new();
            for object_map in &pom.object_maps {
                match object_map {
                    ObjectMap::TermMap { term_generator } => {
                        let objects = term_generator.generate(record)?;
                        all_objects.extend(objects);
                    }
                    ObjectMap::RefObjectMap { .. } => {
                        // Should not happen in parallel path
                        unreachable!("RefObjectMap in parallel path");
                    }
                }
            }

            // Get graph
            let graph = if pom.graph_maps.is_empty() {
                subject_graph.clone()
            } else {
                let graph_terms = pom.graph_maps[0].term_generator.generate(record)?;
                if graph_terms.is_empty() {
                    None
                } else if let TermRef::NamedNode(node) = &graph_terms[0] {
                    Some(node.clone())
                } else {
                    return Err(RmlError::Validation(
                        "Graph name must be a named node".to_string(),
                    ));
                }
            };

            // Create quads — avoid redundant clones where possible
            if subjects.len() == 1 && all_predicates.len() == 1 && all_objects.len() == 1 {
                // Fast path: single subject, single predicate, single object — no cloning needed
                if let TermRef::NamedNode(pred_node) = all_predicates.into_iter().next().unwrap() {
                    let quad = Quad::new(
                        subjects[0].clone(),
                        pred_node,
                        all_objects.into_iter().next().unwrap(),
                        graph,
                    )
                    .map_err(RmlError::Validation)?;
                    quads.push(quad);
                } else {
                    return Err(RmlError::Validation(
                        "Predicate must be a named node".to_string(),
                    ));
                }
            } else {
                // General path with cartesian product
                for subject in &subjects {
                    for predicate in &all_predicates {
                        if let TermRef::NamedNode(pred_node) = predicate {
                            for object in &all_objects {
                                let quad = Quad::new(
                                    subject.clone(),
                                    pred_node.clone(),
                                    object.clone(),
                                    graph.clone(),
                                )
                                .map_err(RmlError::Validation)?;
                                quads.push(quad);
                            }
                        } else {
                            return Err(RmlError::Validation(
                                "Predicate must be a named node".to_string(),
                            ));
                        }
                    }
                }
            }
        }

        Ok(quads)
    }

    /// Generates triples for a single record by triples map index (sequential path for joins)
    fn process_record_by_index(
        &mut self,
        record: &Record,
        triples_map_index: usize,
        record_index: usize,
    ) -> Result<Vec<Quad>> {
        let mut quads = Vec::new();

        // Generate subject(s)
        let subjects = self.mapping.triples_maps[triples_map_index]
            .subject_map
            .term_generator
            .generate(record)?;

        // Cache subjects for potential joins
        let triples_map_id = self.mapping.triples_maps[triples_map_index]
            .id
            .value()
            .to_string();
        self.context
            .cache_subjects(triples_map_id.clone(), record_index, subjects.clone());

        // Get graph from subject map (used as default for all triples from this triples map)
        let subject_graph_maps = &self.mapping.triples_maps[triples_map_index]
            .subject_map
            .graph_maps;
        let subject_graph = self.get_graph(subject_graph_maps, record)?;

        // Generate rdf:type triples for classes (using pre-computed constant)
        if !self.mapping.triples_maps[triples_map_index]
            .subject_map
            .classes
            .is_empty()
        {
            for subject in &subjects {
                let classes = &self.mapping.triples_maps[triples_map_index]
                    .subject_map
                    .classes;
                for class in classes {
                    let quad = Quad::new(
                        subject.clone(),
                        self.constants.rdf_type.clone(),
                        TermRef::NamedNode(class.clone()),
                        subject_graph.clone(),
                    )
                    .map_err(RmlError::Validation)?;

                    quads.push(quad);
                }
            }
        }

        // Process each predicate-object map
        let num_poms = self.mapping.triples_maps[triples_map_index]
            .predicate_object_maps
            .len();
        for pom_index in 0..num_poms {
            let pom_quads = self.process_predicate_object_map_by_index(
                record,
                &subjects,
                triples_map_index,
                pom_index,
                subject_graph.as_ref(),
            )?;
            quads.extend(pom_quads);
        }

        Ok(quads)
    }

    /// Processes a predicate-object map by index
    fn process_predicate_object_map_by_index(
        &mut self,
        record: &Record,
        subjects: &[TermRef],
        triples_map_index: usize,
        pom_index: usize,
        subject_graph: Option<&NamedNode>,
    ) -> Result<Vec<Quad>> {
        let mut quads = Vec::new();

        // Generate predicate(s)
        let mut all_predicates = Vec::new();
        let predicate_maps = &self.mapping.triples_maps[triples_map_index].predicate_object_maps
            [pom_index]
            .predicate_maps;
        for predicate_map in predicate_maps {
            let predicates = predicate_map.term_generator.generate(record)?;
            all_predicates.extend(predicates);
        }

        // Generate object(s)
        let mut all_objects = Vec::new();
        let num_object_maps = self.mapping.triples_maps[triples_map_index].predicate_object_maps
            [pom_index]
            .object_maps
            .len();

        for obj_map_index in 0..num_object_maps {
            let object_map = &self.mapping.triples_maps[triples_map_index].predicate_object_maps
                [pom_index]
                .object_maps[obj_map_index];

            match object_map {
                ObjectMap::TermMap { term_generator } => {
                    let objects = term_generator.generate(record)?;
                    all_objects.extend(objects);
                }
                ObjectMap::RefObjectMap {
                    parent_triples_map,
                    join_conditions,
                } => {
                    // Clone the data we need for the join to avoid borrowing issues
                    let parent_tm_ref = parent_triples_map.clone();
                    let join_conds = join_conditions.clone();

                    // Handle join
                    let join_objects = self.resolve_join(record, &parent_tm_ref, &join_conds)?;
                    all_objects.extend(join_objects);
                }
            }
        }

        // Get graph: use POM's graph if present, otherwise fall back to subject map's graph
        let pom_graph_maps = &self.mapping.triples_maps[triples_map_index].predicate_object_maps
            [pom_index]
            .graph_maps;
        let graph = if pom_graph_maps.is_empty() {
            subject_graph.cloned()
        } else {
            self.get_graph(pom_graph_maps, record)?
        };

        // Create quads for all combinations of subject, predicate, object
        for subject in subjects {
            for predicate in &all_predicates {
                // Predicates must be named nodes
                if let TermRef::NamedNode(pred_node) = predicate {
                    for object in &all_objects {
                        let quad = Quad::new(
                            subject.clone(),
                            pred_node.clone(),
                            object.clone(),
                            graph.clone(),
                        )
                        .map_err(RmlError::Validation)?;

                        quads.push(quad);
                    }
                } else {
                    return Err(RmlError::Validation(
                        "Predicate must be a named node".to_string(),
                    ));
                }
            }
        }

        Ok(quads)
    }

    /// Handles referencing object maps (joins)
    fn resolve_join(
        &mut self,
        child_record: &Record,
        parent_triples_map_ref: &TermRef,
        join_conditions: &[crate::mapping::JoinCondition],
    ) -> Result<Vec<TermRef>> {
        let mut objects = Vec::new();

        // First, find the parent triples map ID
        let parent_triples_map_id = self
            .mapping
            .triples_maps
            .iter()
            .find(|tm| &tm.id == parent_triples_map_ref)
            .ok_or_else(|| {
                RmlError::Mapping(format!(
                    "Parent triples map '{}' not found",
                    parent_triples_map_ref
                ))
            })?
            .id
            .value()
            .to_string();

        // Get parent records (this needs mutable borrow, so we do it separately)
        let parent_triples_map_index = self
            .mapping
            .triples_maps
            .iter()
            .position(|tm| tm.id.value() == parent_triples_map_id)
            .unwrap();

        let parent_records = self.get_records_by_index(parent_triples_map_index)?;

        // Find matching parent records based on join conditions
        for (parent_index, parent_record) in parent_records.iter().enumerate() {
            if self.evaluate_join_conditions(child_record, parent_record, join_conditions)? {
                // Get or generate subjects from the parent record
                let parent_subjects = if let Some(cached) = self
                    .context
                    .get_subjects(&parent_triples_map_id, parent_index)
                {
                    cached.clone()
                } else {
                    // Generate subjects for the parent record
                    let parent_tm = self
                        .mapping
                        .triples_maps
                        .iter()
                        .find(|tm| tm.id.value() == parent_triples_map_id)
                        .unwrap();

                    let subjects = parent_tm
                        .subject_map
                        .term_generator
                        .generate(parent_record)?;
                    self.context.cache_subjects(
                        parent_triples_map_id.clone(),
                        parent_index,
                        subjects.clone(),
                    );
                    subjects
                };

                objects.extend(parent_subjects);
            }
        }

        if objects.is_empty() {
            return Err(RmlError::Mapping(
                "No matching parent records found for join".to_string(),
            ));
        }

        Ok(objects)
    }

    /// Evaluates join conditions between child and parent records
    fn evaluate_join_conditions(
        &self,
        child_record: &Record,
        parent_record: &Record,
        join_conditions: &[crate::mapping::JoinCondition],
    ) -> Result<bool> {
        for condition in join_conditions {
            let child_value = crate::records::extract_value(child_record, &condition.child);
            let parent_value = crate::records::extract_value(parent_record, &condition.parent);

            match (child_value, parent_value) {
                (Some(child_val), Some(parent_val)) => {
                    // Compare values as strings
                    if child_val.as_string() != parent_val.as_string() {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        }

        Ok(true)
    }

    /// Gets records from a logical source by triples map index
    fn get_records_by_index(&mut self, triples_map_index: usize) -> Result<Vec<Record>> {
        let triples_map_id = self.mapping.triples_maps[triples_map_index]
            .id
            .value()
            .to_string();

        // Check cache first — return a reference-counted clone instead of deep clone
        if let Some(cached) = self.context.get_records(&triples_map_id) {
            return Ok(cached.clone());
        }

        // Create access to the data source
        let source = &self.mapping.triples_maps[triples_map_index]
            .logical_source
            .source;
        let access = self.create_access(source)?;

        // Create record iterator
        let reference_formulation = &self.mapping.triples_maps[triples_map_index]
            .logical_source
            .reference_formulation;
        let iterator_expr = self.mapping.triples_maps[triples_map_index]
            .logical_source
            .iterator
            .as_deref();

        let mut iterator =
            self.records_factory
                .create_iterator(access, reference_formulation, iterator_expr)?;

        // Collect all records
        let records = iterator.collect_all()?;

        // Cache the records
        self.context.cache_records(triples_map_id, records.clone());

        Ok(records)
    }

    /// Creates an Access instance for a data source
    fn create_access(&self, source: &TermRef) -> Result<Box<dyn Access>> {
        let source_str = source.value();

        // Check if it's a URL
        if source_str.starts_with("http://") || source_str.starts_with("https://") {
            return Err(RmlError::Access(
                "Remote file access not yet implemented".to_string(),
            ));
        }

        // Treat as local file path
        let path = PathBuf::from(source_str);
        Ok(Box::new(LocalFileAccess::new(
            path,
            Some(self.base_path.clone()),
            None,
        )))
    }

    /// Gets the graph name from graph maps
    fn get_graph(
        &self,
        graph_maps: &[crate::mapping::GraphMap],
        record: &Record,
    ) -> Result<Option<NamedNode>> {
        if graph_maps.is_empty() {
            return Ok(None);
        }

        // Use the first graph map
        let graph_terms = graph_maps[0].term_generator.generate(record)?;

        if graph_terms.is_empty() {
            return Ok(None);
        }

        // Graph must be a named node
        if let TermRef::NamedNode(node) = &graph_terms[0] {
            Ok(Some(node.clone()))
        } else {
            Err(RmlError::Validation(
                "Graph name must be a named node".to_string(),
            ))
        }
    }
}

/// Configuration options for the executor
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Maximum number of records to process (None = unlimited)
    pub max_records: Option<usize>,

    /// Whether to continue on errors
    pub continue_on_error: bool,

    /// Whether to validate generated triples
    pub validate_triples: bool,

    /// Number of parallel workers (1 = sequential)
    pub parallel_workers: usize,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            max_records: None,
            continue_on_error: false,
            validate_triples: true,
            parallel_workers: 1,
        }
    }
}

/// Statistics about mapping execution
#[derive(Debug, Default, Clone)]
pub struct ExecutionStats {
    /// Number of records processed
    pub records_processed: usize,

    /// Number of quads generated
    pub quads_generated: usize,

    /// Number of errors encountered
    pub errors: usize,

    /// Execution time in milliseconds
    pub execution_time_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{
        JoinCondition, LogicalSource, ObjectMap, PredicateMap, PredicateObjectMap, SubjectMap,
        TriplesMap,
    };
    use crate::namespaces::QL;
    use crate::records::RecordValue;
    use crate::term::{Literal, NamedNode};
    use crate::termgenerator::{
        ConstantExtractor, LiteralGenerator, NamedNodeGenerator, ReferenceExtractor,
        TemplateExtractor,
    };
    use std::collections::HashSet;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Helper: execute an executor that has no output sender and collect nothing.
    /// Used for tests that only check execution doesn't error.
    fn execute_collect(executor: &mut Executor) -> Vec<Quad> {
        let (tx, rx) = crossbeam::channel::unbounded();
        executor.output_sender = Some(tx);
        executor.execute().unwrap();
        executor.output_sender = None; // drop tx to close the channel
        rx.into_iter().flatten().collect()
    }

    fn create_test_csv_file() -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "id,name,age").unwrap();
        writeln!(file, "1,Alice,30").unwrap();
        writeln!(file, "2,Bob,25").unwrap();
        file.flush().unwrap();
        file
    }

    fn create_simple_mapping(csv_path: &str) -> MappingDocument {
        let mut mapping = MappingDocument::new(Some("http://example.org/".to_string()));

        // Create logical source
        let logical_source = LogicalSource {
            source: TermRef::Literal(Literal::new(csv_path)),
            reference_formulation: format!("{}CSV", QL),
            iterator: None,
        };

        // Create subject map
        let subject_template = TemplateExtractor::new("http://example.org/person/{id}").unwrap();
        let subject_generator = NamedNodeGenerator::new(
            Box::new(subject_template),
            Some("http://example.org/".to_string()),
            crate::termgenerator::StrictMode::Strict,
        );

        let subject_map = SubjectMap {
            term_generator: Box::new(subject_generator),
            classes: vec![NamedNode::new("http://xmlns.com/foaf/0.1/Person").unwrap()],
            graph_maps: vec![],
        };

        // Create predicate-object map for name
        let name_predicate = NamedNodeGenerator::new(
            Box::new(ConstantExtractor::new("http://xmlns.com/foaf/0.1/name")),
            None,
            crate::termgenerator::StrictMode::Strict,
        );

        let name_object =
            LiteralGenerator::new(Box::new(ReferenceExtractor::new("name")), None, None);

        let name_pom = PredicateObjectMap {
            predicate_maps: vec![PredicateMap {
                term_generator: Box::new(name_predicate),
            }],
            object_maps: vec![ObjectMap::TermMap {
                term_generator: Box::new(name_object),
            }],
            graph_maps: vec![],
        };

        // Create predicate-object map for age
        let age_predicate = NamedNodeGenerator::new(
            Box::new(ConstantExtractor::new("http://xmlns.com/foaf/0.1/age")),
            None,
            crate::termgenerator::StrictMode::Strict,
        );

        let age_object = LiteralGenerator::new(
            Box::new(ReferenceExtractor::new("age")),
            None,
            Some("http://www.w3.org/2001/XMLSchema#integer".to_string()),
        );

        let age_pom = PredicateObjectMap {
            predicate_maps: vec![PredicateMap {
                term_generator: Box::new(age_predicate),
            }],
            object_maps: vec![ObjectMap::TermMap {
                term_generator: Box::new(age_object),
            }],
            graph_maps: vec![],
        };

        let triples_map = TriplesMap {
            id: TermRef::NamedNode(NamedNode::new("http://example.org/map1").unwrap()),
            logical_source,
            subject_map,
            predicate_object_maps: vec![name_pom, age_pom],
        };

        mapping.add_triples_map(triples_map);
        mapping
    }

    #[test]
    fn test_executor_simple_mapping() {
        let csv_file = create_test_csv_file();
        let csv_path = csv_file.path().to_str().unwrap();

        let mapping = create_simple_mapping(csv_path);
        let base_path = PathBuf::from(".");

        let mut executor = Executor::new(mapping, base_path, StrictMode::Strict);
        let quads = execute_collect(&mut executor);

        // Should have generated quads for 2 records
        // Each record generates: 1 rdf:type + 1 name + 1 age = 3 quads
        // Total: 2 * 3 = 6 quads
        assert_eq!(quads.len(), 6);

        // Check that we have the expected subjects
        let subjects: HashSet<_> = quads.iter().map(|q| q.subject().clone()).collect();
        assert_eq!(subjects.len(), 2);
    }

    #[test]
    fn test_executor_with_classes() {
        let csv_file = create_test_csv_file();
        let csv_path = csv_file.path().to_str().unwrap();

        let mapping = create_simple_mapping(csv_path);
        let base_path = PathBuf::from(".");

        let mut executor = Executor::new(mapping, base_path, StrictMode::Strict);
        let quads = execute_collect(&mut executor);

        // Check for rdf:type triples
        let rdf_type = NamedNode::new(format!("{}type", RDF)).unwrap();
        let type_quads: Vec<_> = quads
            .iter()
            .filter(|q| q.predicate() == &rdf_type)
            .collect();

        // Should have 2 rdf:type triples (one for each person)
        assert_eq!(type_quads.len(), 2);

        // Check that the class is foaf:Person
        let foaf_person =
            TermRef::NamedNode(NamedNode::new("http://xmlns.com/foaf/0.1/Person").unwrap());
        for quad in type_quads {
            assert_eq!(quad.object(), &foaf_person);
        }
    }

    #[test]
    fn test_executor_predicate_object_maps() {
        let csv_file = create_test_csv_file();
        let csv_path = csv_file.path().to_str().unwrap();

        let mapping = create_simple_mapping(csv_path);
        let base_path = PathBuf::from(".");

        let mut executor = Executor::new(mapping, base_path, StrictMode::Strict);
        let quads = execute_collect(&mut executor);

        // Check for name predicates
        let name_pred = NamedNode::new("http://xmlns.com/foaf/0.1/name").unwrap();
        let name_quads: Vec<_> = quads
            .iter()
            .filter(|q| q.predicate() == &name_pred)
            .collect();
        assert_eq!(name_quads.len(), 2);

        // Check for age predicates
        let age_pred = NamedNode::new("http://xmlns.com/foaf/0.1/age").unwrap();
        let age_quads: Vec<_> = quads
            .iter()
            .filter(|q| q.predicate() == &age_pred)
            .collect();
        assert_eq!(age_quads.len(), 2);
    }

    #[test]
    fn test_evaluate_join_conditions() {
        let mut child_record = Record::new();
        child_record.insert("dept_id".to_string(), RecordValue::String("D1".to_string()));

        let mut parent_record = Record::new();
        parent_record.insert("id".to_string(), RecordValue::String("D1".to_string()));

        let join_condition = JoinCondition {
            child: "dept_id".to_string(),
            parent: "id".to_string(),
        };

        let mapping = MappingDocument::new(None);
        let executor = Executor::new(mapping, PathBuf::from("."), StrictMode::Strict);

        let result = executor
            .evaluate_join_conditions(&child_record, &parent_record, &[join_condition])
            .unwrap();

        assert!(result);
    }

    #[test]
    fn test_evaluate_join_conditions_no_match() {
        let mut child_record = Record::new();
        child_record.insert("dept_id".to_string(), RecordValue::String("D1".to_string()));

        let mut parent_record = Record::new();
        parent_record.insert("id".to_string(), RecordValue::String("D2".to_string()));

        let join_condition = JoinCondition {
            child: "dept_id".to_string(),
            parent: "id".to_string(),
        };

        let mapping = MappingDocument::new(None);
        let executor = Executor::new(mapping, PathBuf::from("."), StrictMode::Strict);

        let result = executor
            .evaluate_join_conditions(&child_record, &parent_record, &[join_condition])
            .unwrap();

        assert!(!result);
    }

    #[test]
    fn test_records_caching() {
        let csv_file = create_test_csv_file();
        let csv_path = csv_file.path().to_str().unwrap();

        let mapping = create_simple_mapping(csv_path);
        let base_path = PathBuf::from(".");

        let mut executor = Executor::new(mapping, base_path, StrictMode::Strict);

        // Execute the mapping
        let _ = execute_collect(&mut executor);

        // Check that records were cached
        let triples_map_id = "http://example.org/map1";
        assert!(executor.context.get_records(triples_map_id).is_some());

        let cached_records = executor.context.get_records(triples_map_id).unwrap();
        assert_eq!(cached_records.len(), 2);
    }

    #[test]
    fn test_subjects_caching() {
        let csv_file = create_test_csv_file();
        let csv_path = csv_file.path().to_str().unwrap();

        let mapping = create_simple_mapping(csv_path);
        let base_path = PathBuf::from(".");

        let mut executor = Executor::new(mapping, base_path, StrictMode::Strict);

        // Execute the mapping
        let _ = execute_collect(&mut executor);

        // Check that subjects were cached
        let triples_map_id = "http://example.org/map1";
        assert!(executor.context.get_subjects(triples_map_id, 0).is_some());
        assert!(executor.context.get_subjects(triples_map_id, 1).is_some());
    }

    #[test]
    fn test_empty_mapping() {
        let mapping = MappingDocument::new(None);
        let base_path = PathBuf::from(".");

        let mut executor = Executor::new(mapping, base_path, StrictMode::Strict);
        let quads = execute_collect(&mut executor);

        assert_eq!(quads.len(), 0);
    }

    #[test]
    fn test_multiple_values_from_array() {
        // Create CSV with correct columns matching the mapping
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "id,name,age").unwrap();
        writeln!(file, "1,Alice,30").unwrap();
        writeln!(file, "2,Bob,25").unwrap();
        writeln!(file, "3,Charlie,35").unwrap();
        file.flush().unwrap();

        let csv_path = file.path().to_str().unwrap();
        let mapping = create_simple_mapping(csv_path);
        let base_path = PathBuf::from(".");

        let mut executor = Executor::new(mapping, base_path, StrictMode::Strict);
        let quads = execute_collect(&mut executor);

        // Should generate quads for all 3 records
        // Each record: 1 rdf:type + 1 name + 1 age = 3 quads
        // Total: 3 * 3 = 9 quads
        assert_eq!(quads.len(), 9);
    }
}
