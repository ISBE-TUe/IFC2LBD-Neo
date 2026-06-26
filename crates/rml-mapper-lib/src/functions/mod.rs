//! Function Execution
//!
//! This module handles the execution of FnO (Function Ontology) functions
//! in RML mappings. Functions can transform data during the mapping process.
//!
//! # Architecture
//!
//! The module follows the Java RML Mapper's FunctionAgent design:
//! - `FunctionExecutor` trait: Common interface for all function executors
//! - `FunctionRegistry`: Registry for managing available functions
//! - `FunctionValueExecutor`: Executes fnml:functionValue expressions
//! - Built-in GREL functions: String manipulation functions from General Refine Expression Language
//! - Built-in IDLab functions: Utility functions for joins, UUIDs, etc.
//!
//! # Examples
//!
//! ```
//! use rml_mapper::functions::{FunctionRegistry, FunctionExecutor};
//! use std::collections::HashMap;
//!
//! let mut registry = FunctionRegistry::new();
//! registry.load_defaults();
//!
//! let function = registry.get("http://users.ugent.be/~bjdmeest/function/grel.ttl#toUpperCase").unwrap();
//! let mut params = HashMap::new();
//! params.insert("valueParameter".to_string(), vec!["hello".to_string()]);
//! let result = function.execute(&params).unwrap();
//! assert_eq!(result, vec!["HELLO"]);
//! ```

use crate::error::{Result, RmlError};
use crate::records::Record;
use crate::term::{Literal, TermRef};
use crate::termgenerator::{TermGenerator, ValueExtractor};
use std::collections::HashMap;
use std::sync::Arc;

mod grel;
mod idlab;

// Re-export GREL functions
pub use grel::{
    ToUpperCaseFunction, ToLowerCaseFunction, TrimFunction, ReplaceFunction,
    SplitFunction, ConcatFunction, SubstringFunction, LengthFunction,
    ContainsFunction, StartsWithFunction, EndsWithFunction,
    GREL_BASE,
};

// Re-export IDLab functions
pub use idlab::{
    EqualFunction, NotEqualFunction, IsNullFunction, RandomFunction,
    UuidFunction, SlugifyFunction,
    IDLAB_BASE,
};

/// Trait for executable functions
///
/// This trait defines the interface that all function executors must implement.
/// Functions take a map of parameter names to values and return a vector of results.
pub trait FunctionExecutor: Send + Sync {
    /// Execute the function with given parameters
    ///
    /// # Arguments
    ///
    /// * `params` - Map of parameter names to their values
    ///
    /// # Returns
    ///
    /// A vector of result strings. Multiple results may be returned if the
    /// function operates on arrays or generates multiple outputs.
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>>;
    
    /// Get the function IRI
    fn function_iri(&self) -> &str;
}

/// Registry for managing available functions
///
/// The registry maintains a collection of available functions and provides
/// methods to register, retrieve, and manage them.
pub struct FunctionRegistry {
    functions: HashMap<String, Box<dyn FunctionExecutor>>,
}

impl FunctionRegistry {
    /// Creates a new empty function registry
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
        }
    }
    
    /// Register a function
    ///
    /// # Arguments
    ///
    /// * `function` - The function executor to register
    pub fn register(&mut self, function: Box<dyn FunctionExecutor>) {
        let iri = function.function_iri().to_string();
        self.functions.insert(iri, function);
    }
    
    /// Get a function by IRI
    ///
    /// # Arguments
    ///
    /// * `iri` - The IRI of the function to retrieve
    ///
    /// # Returns
    ///
    /// A reference to the function executor, or None if not found
    pub fn get(&self, iri: &str) -> Option<&dyn FunctionExecutor> {
        self.functions.get(iri).map(|f| f.as_ref())
    }
    
    /// Load default functions (GREL, IDLab)
    ///
    /// This method registers all built-in functions including:
    /// - GREL string manipulation functions
    /// - IDLab utility functions
    pub fn load_defaults(&mut self) {
        // Register GREL functions
        self.register(Box::new(ToUpperCaseFunction));
        self.register(Box::new(ToLowerCaseFunction));
        self.register(Box::new(TrimFunction));
        self.register(Box::new(ReplaceFunction));
        self.register(Box::new(SplitFunction));
        self.register(Box::new(ConcatFunction));
        self.register(Box::new(SubstringFunction));
        self.register(Box::new(LengthFunction));
        self.register(Box::new(ContainsFunction));
        self.register(Box::new(StartsWithFunction));
        self.register(Box::new(EndsWithFunction));
        
        // Register IDLab functions
        self.register(Box::new(EqualFunction));
        self.register(Box::new(NotEqualFunction));
        self.register(Box::new(IsNullFunction));
        self.register(Box::new(RandomFunction));
        self.register(Box::new(UuidFunction));
        self.register(Box::new(SlugifyFunction));
    }
    
    /// Returns all registered function IRIs
    pub fn list_functions(&self) -> Vec<&str> {
        self.functions.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for FunctionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Parameter mapping for function value execution
///
/// Maps a function parameter to a value extractor that retrieves the
/// parameter value from a record.
pub struct ParameterMapping {
    /// The parameter IRI
    pub parameter_iri: String,
    
    /// The value extractor for this parameter
    pub value_extractor: Box<dyn ValueExtractor>,
}

/// Executor for fnml:functionValue expressions
///
/// This executor evaluates function calls within RML mappings, extracting
/// parameter values from records and passing them to the appropriate function.
pub struct FunctionValueExecutor {
    /// Reference to the function registry
    registry: Arc<FunctionRegistry>,
    
    /// The IRI of the function to execute
    function_iri: String,
    
    /// Mappings from function parameters to value extractors
    parameter_mappings: Vec<ParameterMapping>,
}

impl FunctionValueExecutor {
    /// Creates a new function value executor
    ///
    /// # Arguments
    ///
    /// * `registry` - The function registry to use
    /// * `function_iri` - The IRI of the function to execute
    /// * `parameter_mappings` - Mappings from parameters to value extractors
    pub fn new(
        registry: Arc<FunctionRegistry>,
        function_iri: String,
        parameter_mappings: Vec<ParameterMapping>,
    ) -> Self {
        Self {
            registry,
            function_iri,
            parameter_mappings,
        }
    }
    
    /// Execute the function with values extracted from a record
    ///
    /// # Arguments
    ///
    /// * `record` - The record to extract parameter values from
    ///
    /// # Returns
    ///
    /// A vector of result strings
    pub fn execute(&self, record: &Record) -> Result<Vec<String>> {
        // Get the function from the registry
        let function = self.registry.get(&self.function_iri).ok_or_else(|| {
            RmlError::Function(format!("Function '{}' not found in registry", self.function_iri))
        })?;
        
        // Extract parameter values from the record
        let mut params: HashMap<String, Vec<String>> = HashMap::new();
        
        for mapping in &self.parameter_mappings {
            let values = mapping.value_extractor.extract(record)?;
            params.insert(mapping.parameter_iri.clone(), values);
        }
        
        // Execute the function
        function.execute(&params)
    }
}

impl ValueExtractor for FunctionValueExecutor {
    fn extract(&self, record: &Record) -> Result<Vec<String>> {
        self.execute(record)
    }
}

/// Helper function to get a single required parameter value
///
/// # Arguments
///
/// * `params` - The parameter map
/// * `param_name` - The name of the parameter to retrieve
///
/// # Returns
///
/// The first value of the parameter, or an error if not found
pub(crate) fn get_required_param(
    params: &HashMap<String, Vec<String>>,
    param_name: &str,
) -> Result<String> {
    params
        .get(param_name)
        .and_then(|v| v.first())
        .cloned()
        .ok_or_else(|| {
            RmlError::Function(format!("Required parameter '{}' not found", param_name))
        })
}

/// Helper function to get an optional parameter value
///
/// # Arguments
///
/// * `params` - The parameter map
/// * `param_name` - The name of the parameter to retrieve
///
/// # Returns
///
/// The first value of the parameter, or None if not found
pub(crate) fn get_optional_param(
    params: &HashMap<String, Vec<String>>,
    param_name: &str,
) -> Option<String> {
    params.get(param_name).and_then(|v| v.first()).cloned()
}

/// Helper function to get all values of a parameter
///
/// # Arguments
///
/// * `params` - The parameter map
/// * `param_name` - The name of the parameter to retrieve
///
/// # Returns
///
/// All values of the parameter, or an empty vector if not found
pub(crate) fn get_all_params(
    params: &HashMap<String, Vec<String>>,
    param_name: &str,
) -> Vec<String> {
    params.get(param_name).cloned().unwrap_or_default()
}

/// Term generator that uses function execution
///
/// This generator executes a function and generates literal terms from the results.
/// It's used to integrate fnml:functionValue into the term generation pipeline.
pub struct FunctionTermGenerator {
    /// The function value executor
    executor: FunctionValueExecutor,
    
    /// Optional language tag for generated literals
    language: Option<String>,
    
    /// Optional datatype IRI for generated literals
    datatype: Option<String>,
}

impl FunctionTermGenerator {
    /// Creates a new function term generator
    ///
    /// # Arguments
    ///
    /// * `executor` - The function value executor to use
    /// * `language` - Optional language tag for generated literals
    /// * `datatype` - Optional datatype IRI for generated literals
    pub fn new(
        executor: FunctionValueExecutor,
        language: Option<String>,
        datatype: Option<String>,
    ) -> Self {
        Self {
            executor,
            language,
            datatype,
        }
    }
}

impl TermGenerator for FunctionTermGenerator {
    fn generate(&self, record: &Record) -> Result<Vec<TermRef>> {
        let values = self.executor.execute(record)?;
        let mut terms = Vec::new();
        
        for value in values {
            let literal = if let Some(lang) = &self.language {
                Literal::with_language(value, lang.clone())
            } else if let Some(datatype) = &self.datatype {
                Literal::with_datatype(value, datatype.clone())
            } else {
                Literal::new(value)
            };
            
            terms.push(TermRef::Literal(literal));
        }
        
        Ok(terms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::RecordValue;
    use crate::term::Term;
    use crate::termgenerator::ConstantExtractor;

    #[test]
    fn test_function_registry_new() {
        let registry = FunctionRegistry::new();
        assert_eq!(registry.list_functions().len(), 0);
    }

    #[test]
    fn test_function_registry_load_defaults() {
        let mut registry = FunctionRegistry::new();
        registry.load_defaults();
        
        let functions = registry.list_functions();
        assert!(functions.len() > 0);
        
        // Check for some GREL functions
        assert!(registry.get("http://users.ugent.be/~bjdmeest/function/grel.ttl#toUpperCase").is_some());
        assert!(registry.get("http://users.ugent.be/~bjdmeest/function/grel.ttl#toLowerCase").is_some());
        assert!(registry.get("http://users.ugent.be/~bjdmeest/function/grel.ttl#concat").is_some());
        
        // Check for some IDLab functions
        assert!(registry.get("http://example.com/idlab/function/equal").is_some());
        assert!(registry.get("http://example.com/idlab/function/uuid").is_some());
    }

    #[test]
    fn test_get_required_param() {
        let mut params = HashMap::new();
        params.insert("test".to_string(), vec!["value".to_string()]);
        
        let result = get_required_param(&params, "test").unwrap();
        assert_eq!(result, "value");
        
        let result = get_required_param(&params, "missing");
        assert!(result.is_err());
    }

    #[test]
    fn test_get_optional_param() {
        let mut params = HashMap::new();
        params.insert("test".to_string(), vec!["value".to_string()]);
        
        let result = get_optional_param(&params, "test");
        assert_eq!(result, Some("value".to_string()));
        
        let result = get_optional_param(&params, "missing");
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_all_params() {
        let mut params = HashMap::new();
        params.insert("test".to_string(), vec!["value1".to_string(), "value2".to_string()]);
        
        let result = get_all_params(&params, "test");
        assert_eq!(result, vec!["value1", "value2"]);
        
        let result = get_all_params(&params, "missing");
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn test_function_value_executor() {
        let mut registry = FunctionRegistry::new();
        registry.load_defaults();
        let registry = Arc::new(registry);
        
        let mut record = Record::new();
        record.insert("name".to_string(), RecordValue::String("hello".to_string()));
        
        let executor = FunctionValueExecutor::new(
            registry,
            "http://users.ugent.be/~bjdmeest/function/grel.ttl#toUpperCase".to_string(),
            vec![ParameterMapping {
                parameter_iri: "valueParameter".to_string(),
                value_extractor: Box::new(crate::termgenerator::ReferenceExtractor::new("name")),
            }],
        );
        
        let result = executor.execute(&record).unwrap();
        assert_eq!(result, vec!["HELLO"]);
    }

    #[test]
    fn test_function_value_executor_not_found() {
        let registry = Arc::new(FunctionRegistry::new());
        
        let record = Record::new();
        
        let executor = FunctionValueExecutor::new(
            registry,
            "http://example.com/nonexistent".to_string(),
            vec![],
        );
        
        let result = executor.execute(&record);
        assert!(result.is_err());
    }

    #[test]
    fn test_function_term_generator() {
        let mut registry = FunctionRegistry::new();
        registry.load_defaults();
        let registry = Arc::new(registry);
        
        let mut record = Record::new();
        record.insert("text".to_string(), RecordValue::String("Hello World".to_string()));
        
        let executor = FunctionValueExecutor::new(
            registry,
            "http://users.ugent.be/~bjdmeest/function/grel.ttl#toLowerCase".to_string(),
            vec![ParameterMapping {
                parameter_iri: "valueParameter".to_string(),
                value_extractor: Box::new(crate::termgenerator::ReferenceExtractor::new("text")),
            }],
        );
        
        let generator = FunctionTermGenerator::new(executor, None, None);
        let terms = generator.generate(&record).unwrap();
        
        assert_eq!(terms.len(), 1);
        assert!(terms[0].is_literal());
        assert_eq!(terms[0].value(), "hello world");
    }

    #[test]
    fn test_function_term_generator_with_language() {
        let mut registry = FunctionRegistry::new();
        registry.load_defaults();
        let registry = Arc::new(registry);
        
        let mut record = Record::new();
        record.insert("text".to_string(), RecordValue::String("hello".to_string()));
        
        let executor = FunctionValueExecutor::new(
            registry,
            "http://users.ugent.be/~bjdmeest/function/grel.ttl#toUpperCase".to_string(),
            vec![ParameterMapping {
                parameter_iri: "valueParameter".to_string(),
                value_extractor: Box::new(crate::termgenerator::ReferenceExtractor::new("text")),
            }],
        );
        
        let generator = FunctionTermGenerator::new(executor, Some("en".to_string()), None);
        let terms = generator.generate(&record).unwrap();
        
        assert_eq!(terms.len(), 1);
        assert!(terms[0].is_literal());
        
        if let TermRef::Literal(lit) = &terms[0] {
            assert_eq!(lit.value(), "HELLO");
            assert_eq!(lit.language(), Some("en"));
        } else {
            panic!("Expected literal term");
        }
    }

    #[test]
    fn test_function_term_generator_with_datatype() {
        let mut registry = FunctionRegistry::new();
        registry.load_defaults();
        let registry = Arc::new(registry);
        
        let mut record = Record::new();
        record.insert("text".to_string(), RecordValue::String("hello".to_string()));
        
        let executor = FunctionValueExecutor::new(
            registry,
            "http://users.ugent.be/~bjdmeest/function/grel.ttl#length".to_string(),
            vec![ParameterMapping {
                parameter_iri: "valueParameter".to_string(),
                value_extractor: Box::new(crate::termgenerator::ReferenceExtractor::new("text")),
            }],
        );
        
        let generator = FunctionTermGenerator::new(
            executor,
            None,
            Some("http://www.w3.org/2001/XMLSchema#integer".to_string()),
        );
        let terms = generator.generate(&record).unwrap();
        
        assert_eq!(terms.len(), 1);
        assert!(terms[0].is_literal());
        
        if let TermRef::Literal(lit) = &terms[0] {
            assert_eq!(lit.value(), "5");
            assert_eq!(lit.datatype(), Some("http://www.w3.org/2001/XMLSchema#integer"));
        } else {
            panic!("Expected literal term");
        }
    }
}
