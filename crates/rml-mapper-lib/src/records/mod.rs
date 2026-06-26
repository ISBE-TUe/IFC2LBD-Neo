//! Record Processing
//!
//! This module handles the extraction and iteration of records from various data formats
//! (CSV, JSON, XML, etc.). It provides a unified interface for accessing structured data.
//!
//! # Architecture
//!
//! The module follows the Java RML Mapper's RecordsFactory design:
//! - `Record`: A map from field names to values
//! - `RecordValue`: Enum representing different value types
//! - `RecordIterator`: Trait for iterating over records
//! - `CsvRecordIterator`: Iterator for CSV data
//! - `JsonRecordIterator`: Iterator for JSON data with JSONPath
//! - `XmlRecordIterator`: Iterator for XML data with XPath
//! - `RecordsFactory`: Factory for creating appropriate iterators
//!
//! # Examples
//!
//! ```
//! use rml_mapper::records::{RecordsFactory, reference_formulation};
//! use rml_mapper::access::LocalFileAccess;
//! use std::path::PathBuf;
//!
//! // Create a CSV record iterator
//! let access = LocalFileAccess::new(PathBuf::from("data.csv"), None, None);
//! // let factory = RecordsFactory::new();
//! // let mut iterator = factory.create_iterator(Box::new(access), reference_formulation::CSV, None).unwrap();
//! ```

use crate::error::{Result, RmlError};
use crate::access::Access;
use std::collections::HashMap;
use std::io::Read;
use csv::ReaderBuilder;
use serde_json::Value as JsonValue;
use sxd_document::parser;
use sxd_xpath::{Factory, Value as XPathValue, nodeset::Node};

/// Represents a single record from a data source
pub type Record = HashMap<String, RecordValue>;

/// Represents a value in a record
#[derive(Debug, Clone, PartialEq)]
pub enum RecordValue {
    /// A string value
    String(String),
    
    /// A floating-point numeric value
    Number(f64),
    
    /// An integer numeric value
    Integer(i64),
    
    /// A boolean value
    Boolean(bool),
    
    /// A null value
    Null,
    
    /// An array of values
    Array(Vec<RecordValue>),
    
    /// A nested object
    Object(HashMap<String, RecordValue>),
}

impl RecordValue {
    /// Converts the value to a string representation
    pub fn as_string(&self) -> String {
        match self {
            RecordValue::String(s) => s.clone(),
            RecordValue::Number(n) => {
                // Format numbers nicely, removing unnecessary decimals
                if n.fract() == 0.0 {
                    format!("{:.0}", n)
                } else {
                    n.to_string()
                }
            }
            RecordValue::Integer(i) => i.to_string(),
            RecordValue::Boolean(b) => b.to_string(),
            RecordValue::Null => String::new(),
            RecordValue::Array(arr) => {
                let items: Vec<String> = arr.iter().map(|v| v.as_string()).collect();
                format!("[{}]", items.join(", "))
            }
            RecordValue::Object(_) => "[Object]".to_string(),
        }
    }

    /// Checks if the value is null or empty
    pub fn is_empty(&self) -> bool {
        match self {
            RecordValue::Null => true,
            RecordValue::String(s) => s.is_empty(),
            RecordValue::Array(arr) => arr.is_empty(),
            RecordValue::Object(obj) => obj.is_empty(),
            _ => false,
        }
    }

    /// Converts a JSON value to a RecordValue
    pub fn from_json(value: &JsonValue) -> Self {
        match value {
            JsonValue::Null => RecordValue::Null,
            JsonValue::Bool(b) => RecordValue::Boolean(*b),
            JsonValue::Number(n) => {
                if let Some(i) = n.as_i64() {
                    RecordValue::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    RecordValue::Number(f)
                } else {
                    RecordValue::String(n.to_string())
                }
            }
            JsonValue::String(s) => RecordValue::String(s.clone()),
            JsonValue::Array(arr) => {
                RecordValue::Array(arr.iter().map(RecordValue::from_json).collect())
            }
            JsonValue::Object(obj) => {
                let mut map = HashMap::new();
                for (k, v) in obj {
                    map.insert(k.clone(), RecordValue::from_json(v));
                }
                RecordValue::Object(map)
            }
        }
    }

    /// Converts a RecordValue to a JSON value
    pub fn to_json(&self) -> JsonValue {
        match self {
            RecordValue::Null => JsonValue::Null,
            RecordValue::Boolean(b) => JsonValue::Bool(*b),
            RecordValue::Integer(i) => JsonValue::Number((*i).into()),
            RecordValue::Number(n) => {
                serde_json::Number::from_f64(*n)
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null)
            }
            RecordValue::String(s) => JsonValue::String(s.clone()),
            RecordValue::Array(arr) => {
                JsonValue::Array(arr.iter().map(|v| v.to_json()).collect())
            }
            RecordValue::Object(obj) => {
                let mut map = serde_json::Map::new();
                for (k, v) in obj {
                    map.insert(k.clone(), v.to_json());
                }
                JsonValue::Object(map)
            }
        }
    }
}

/// Trait for iterating over records from a data source
pub trait RecordIterator: Send {
    /// Returns the next record, or None if there are no more records
    fn next_record(&mut self) -> Result<Option<Record>>;
    
    /// Collects all remaining records into a vector
    fn collect_all(&mut self) -> Result<Vec<Record>> {
        let mut records = Vec::new();
        while let Some(record) = self.next_record()? {
            records.push(record);
        }
        Ok(records)
    }
}

/// CSV record iterator
///
/// Iterates over CSV data, treating each row as a record with column names as keys.
pub struct CsvRecordIterator {
    reader: csv::Reader<Box<dyn Read + Send>>,
    headers: Vec<String>,
}

impl CsvRecordIterator {
    /// Creates a new CSV record iterator
    ///
    /// # Arguments
    ///
    /// * `reader` - A reader providing CSV data
    /// * `delimiter` - Optional custom delimiter (default: comma)
    pub fn new(reader: Box<dyn Read + Send>, delimiter: Option<u8>) -> Result<Self> {
        let mut csv_reader = ReaderBuilder::new()
            .delimiter(delimiter.unwrap_or(b','))
            .has_headers(true)
            .flexible(true)
            .from_reader(reader);

        // Read headers
        let headers = csv_reader
            .headers()
            .map_err(|e| RmlError::Parse(format!("Failed to read CSV headers: {}", e)))?
            .iter()
            .map(|s| s.to_string())
            .collect();

        Ok(Self {
            reader: csv_reader,
            headers,
        })
    }

    /// Creates a CSV iterator from a string
    pub fn from_string(data: String, delimiter: Option<u8>) -> Result<Self> {
        Self::new(Box::new(std::io::Cursor::new(data)), delimiter)
    }
}

impl RecordIterator for CsvRecordIterator {
    fn next_record(&mut self) -> Result<Option<Record>> {
        let mut record_data = csv::StringRecord::new();
        
        match self.reader.read_record(&mut record_data) {
            Ok(true) => {
                let mut record = Record::new();
                
                for (i, value) in record_data.iter().enumerate() {
                    if let Some(header) = self.headers.get(i) {
                        // Per RML spec, CSV values are always strings.
                        // Type conversion happens later based on datatype in the mapping.
                        // We preserve the original string value to maintain formats like "30.0E0"
                        let record_value = if value.is_empty() {
                            RecordValue::Null
                        } else {
                            RecordValue::String(value.to_string())
                        };
                        
                        record.insert(header.clone(), record_value);
                    }
                }
                
                Ok(Some(record))
            }
            Ok(false) => Ok(None),
            Err(e) => Err(RmlError::Parse(format!("Failed to read CSV record: {}", e))),
        }
    }
}

/// JSON record iterator
///
/// Iterates over JSON data using JSONPath expressions to extract records.
pub struct JsonRecordIterator {
    records: Vec<JsonValue>,
    current_index: usize,
}

impl JsonRecordIterator {
    /// Creates a new JSON record iterator
    ///
    /// # Arguments
    ///
    /// * `reader` - A reader providing JSON data
    /// * `jsonpath` - Optional JSONPath expression to extract records (default: "$" for root)
    pub fn new(mut reader: Box<dyn Read + Send>, jsonpath: Option<&str>) -> Result<Self> {
        let mut data = String::new();
        reader
            .read_to_string(&mut data)
            .map_err(|e| RmlError::Parse(format!("Failed to read JSON data: {}", e)))?;

        let json: JsonValue = serde_json::from_str(&data)
            .map_err(|e| RmlError::Parse(format!("Failed to parse JSON: {}", e)))?;

        let records = if let Some(path) = jsonpath {
            // Use JSONPath to extract records
            use jsonpath_rust::JsonPath;
            use std::str::FromStr;
            
            let json_path = JsonPath::from_str(path)
                .map_err(|e| RmlError::Parse(format!("Invalid JSONPath expression '{}': {}", path, e)))?;
            
            let result = json_path.find(&json);
            
            // Convert the result to a vector of JsonValue
            match result {
                JsonValue::Array(arr) => arr,
                JsonValue::Null => vec![],
                other => vec![other],
            }
        } else {
            // No JSONPath, treat the whole document as a single record or array
            match json {
                JsonValue::Array(arr) => arr,
                other => vec![other],
            }
        };

        Ok(Self {
            records,
            current_index: 0,
        })
    }

    /// Creates a JSON iterator from a string
    pub fn from_string(data: String, jsonpath: Option<&str>) -> Result<Self> {
        Self::new(Box::new(std::io::Cursor::new(data)), jsonpath)
    }
}

impl RecordIterator for JsonRecordIterator {
    fn next_record(&mut self) -> Result<Option<Record>> {
        if self.current_index >= self.records.len() {
            return Ok(None);
        }

        let json_value = &self.records[self.current_index];
        self.current_index += 1;

        // Convert JSON object to Record
        match json_value {
            JsonValue::Object(obj) => {
                let mut record = Record::new();
                for (key, value) in obj {
                    record.insert(key.clone(), RecordValue::from_json(value));
                }
                Ok(Some(record))
            }
            // If it's not an object, create a record with a single "value" field
            other => {
                let mut record = Record::new();
                record.insert("value".to_string(), RecordValue::from_json(other));
                Ok(Some(record))
            }
        }
    }
}

/// XML record iterator
///
/// Iterates over XML data using XPath expressions to extract records.
pub struct XmlRecordIterator {
    records: Vec<Record>,
    current_index: usize,
}

impl XmlRecordIterator {
    /// Creates a new XML record iterator
    ///
    /// # Arguments
    ///
    /// * `reader` - A reader providing XML data
    /// * `xpath` - Optional XPath expression to extract records (default: "/*" for root element)
    pub fn new(mut reader: Box<dyn Read + Send>, xpath: Option<&str>) -> Result<Self> {
        let mut data = String::new();
        reader
            .read_to_string(&mut data)
            .map_err(|e| RmlError::Parse(format!("Failed to read XML data: {}", e)))?;

        let package = parser::parse(&data)
            .map_err(|e| RmlError::Parse(format!("Failed to parse XML: {}", e)))?;
        
        let document = package.as_document();
        
        let xpath_expr = xpath.unwrap_or("/*");
        let factory = Factory::new();
        let xpath = factory
            .build(xpath_expr)
            .map_err(|e| RmlError::Parse(format!("Invalid XPath expression '{}': {:?}", xpath_expr, e)))?
            .ok_or_else(|| RmlError::Parse(format!("Failed to compile XPath expression '{}'", xpath_expr)))?;

        let value = xpath
            .evaluate(&sxd_xpath::context::Context::new(), document.root())
            .map_err(|e| RmlError::Parse(format!("Failed to evaluate XPath: {:?}", e)))?;

        let records = match value {
            XPathValue::Nodeset(nodeset) => {
                nodeset
                    .document_order()
                    .iter()
                    .map(|node| Self::node_to_record(node))
                    .collect()
            }
            _ => vec![],
        };

        Ok(Self {
            records,
            current_index: 0,
        })
    }

    /// Creates an XML iterator from a string
    pub fn from_string(data: String, xpath: Option<&str>) -> Result<Self> {
        Self::new(Box::new(std::io::Cursor::new(data)), xpath)
    }

    /// Converts an XML node to a Record
    fn node_to_record(node: &Node) -> Record {
        use sxd_document::dom::ChildOfElement;
        
        let mut record = Record::new();

        match node {
            Node::Element(elem) => {
                let name = elem.name().local_part();
                
                // Add attributes
                for attr in elem.attributes() {
                    let attr_name = format!("@{}", attr.name().local_part());
                    record.insert(attr_name, RecordValue::String(attr.value().to_string()));
                }

                // Process child elements and text
                let mut text_content = String::new();
                let mut child_elements: HashMap<String, Vec<RecordValue>> = HashMap::new();

                for child in elem.children() {
                    match child {
                        ChildOfElement::Text(text) => {
                            text_content.push_str(text.text());
                        }
                        ChildOfElement::Element(child_elem) => {
                            let child_name = child_elem.name().local_part();
                            let child_node = Node::Element(child_elem);
                            let child_record = Self::node_to_record(&child_node);
                            
                            // Check if child element is a simple text element (only has _text and _name)
                            // If so, store just the text value directly for easier access
                            let child_value = if child_record.len() == 2 
                                && child_record.contains_key("_text") 
                                && child_record.contains_key("_name") 
                            {
                                // Simple text element - store text directly
                                child_record.get("_text").cloned().unwrap_or(RecordValue::Null)
                            } else if child_record.len() == 1 && child_record.contains_key("_name") {
                                // Empty element - store empty string
                                RecordValue::String(String::new())
                            } else {
                                // Complex element - store as object
                                RecordValue::Object(child_record)
                            };
                            
                            child_elements
                                .entry(child_name.to_string())
                                .or_default()
                                .push(child_value);
                        }
                        _ => {}
                    }
                }

                // Add text content if present
                if !text_content.trim().is_empty() {
                    record.insert("_text".to_string(), RecordValue::String(text_content.trim().to_string()));
                }

                // Add child elements
                for (child_name, children) in child_elements {
                    if children.len() == 1 {
                        // Single child - store directly
                        record.insert(child_name, children.into_iter().next().unwrap());
                    } else {
                        record.insert(child_name, RecordValue::Array(children));
                    }
                }

                // Add element name
                record.insert("_name".to_string(), RecordValue::String(name.to_string()));
            }
            Node::Text(text) => {
                record.insert("_text".to_string(), RecordValue::String(text.text().to_string()));
            }
            Node::Attribute(attr) => {
                record.insert(
                    attr.name().local_part().to_string(),
                    RecordValue::String(attr.value().to_string()),
                );
            }
            _ => {}
        }

        record
    }
}

impl RecordIterator for XmlRecordIterator {
    fn next_record(&mut self) -> Result<Option<Record>> {
        if self.current_index >= self.records.len() {
            return Ok(None);
        }

        let record = self.records[self.current_index].clone();
        self.current_index += 1;
        Ok(Some(record))
    }
}

/// Reference formulation constants
///
/// These constants define the different reference formulations supported by RML.
pub mod reference_formulation {
    /// CSV reference formulation
    pub const CSV: &str = "http://w3id.org/rml/CSV";
    
    /// JSONPath reference formulation
    pub const JSON_PATH: &str = "http://w3id.org/rml/JSONPath";
    
    /// XPath reference formulation
    pub const XPATH: &str = "http://w3id.org/rml/XPath";
    
    /// SPARQL results XML format
    pub const SPARQL_RESULTS_XML: &str = "http://www.w3.org/ns/formats/SPARQL_Results_XML";
    
    /// SPARQL results JSON format
    pub const SPARQL_RESULTS_JSON: &str = "http://www.w3.org/ns/formats/SPARQL_Results_JSON";
    
    /// SPARQL results CSV format
    pub const SPARQL_RESULTS_CSV: &str = "http://www.w3.org/ns/formats/SPARQL_Results_CSV";
}

/// Factory for creating record iterators
///
/// This factory creates the appropriate RecordIterator based on the reference formulation
/// and data source, matching the Java RML Mapper's RecordsFactory design.
pub struct RecordsFactory {
    /// Cache of records for reuse
    cache: HashMap<String, Vec<Record>>,
}

impl RecordsFactory {
    /// Creates a new records factory
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Creates a record iterator for the given access and reference formulation
    ///
    /// # Arguments
    ///
    /// * `access` - The data access to read from
    /// * `reference_formulation` - The reference formulation URI
    /// * `iterator_expr` - Optional iterator expression (JSONPath or XPath)
    pub fn create_iterator(
        &mut self,
        access: Box<dyn Access>,
        reference_formulation: &str,
        iterator_expr: Option<&str>,
    ) -> Result<Box<dyn RecordIterator>> {
        let reader = access.get_reader()?;

        match reference_formulation {
            reference_formulation::CSV | _ if reference_formulation.contains("CSV") => {
                Ok(Box::new(CsvRecordIterator::new(reader, None)?))
            }
            reference_formulation::JSON_PATH | _ if reference_formulation.contains("JSON") => {
                Ok(Box::new(JsonRecordIterator::new(reader, iterator_expr)?))
            }
            reference_formulation::XPATH | _ if reference_formulation.contains("XPath") => {
                Ok(Box::new(XmlRecordIterator::new(reader, iterator_expr)?))
            }
            reference_formulation::SPARQL_RESULTS_CSV => {
                Ok(Box::new(CsvRecordIterator::new(reader, None)?))
            }
            reference_formulation::SPARQL_RESULTS_JSON => {
                // SPARQL JSON results have a specific structure
                Ok(Box::new(JsonRecordIterator::new(reader, Some("$.results.bindings[*]"))?))
            }
            reference_formulation::SPARQL_RESULTS_XML => {
                // SPARQL XML results have a specific structure
                Ok(Box::new(XmlRecordIterator::new(reader, Some("//sparql:result"))?))
            }
            _ => Err(RmlError::Parse(format!(
                "Unsupported reference formulation: {}",
                reference_formulation
            ))),
        }
    }

    /// Gets cached records for a given cache key
    pub fn get_cached(&self, cache_key: &str) -> Option<&Vec<Record>> {
        self.cache.get(cache_key)
    }

    /// Caches records for a given cache key
    pub fn cache_records(&mut self, cache_key: String, records: Vec<Record>) {
        self.cache.insert(cache_key, records);
    }

    /// Clears the cache
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
}

impl Default for RecordsFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// Extracts a value from a record using a reference path
///
/// # Arguments
///
/// * `record` - The record to extract from
/// * `reference` - The reference path (e.g., "name", "address.city", etc.)
///
/// # Returns
///
/// The extracted value, or None if not found
pub fn extract_value(record: &Record, reference: &str) -> Option<RecordValue> {
    // Handle simple field access
    if !reference.contains('.') && !reference.contains('[') {
        return record.get(reference).cloned();
    }

    // Handle nested field access (e.g., "address.city")
    let parts: Vec<&str> = reference.split('.').collect();
    let mut current_value = record.get(parts[0])?.clone();

    for part in &parts[1..] {
        match current_value {
            RecordValue::Object(obj) => {
                current_value = obj.get(*part)?.clone();
            }
            _ => return None,
        }
    }

    Some(current_value)
}

/// Extracts a value from a record using JSONPath
///
/// # Arguments
///
/// * `record` - The record to extract from
/// * `jsonpath` - The JSONPath expression
///
/// # Returns
///
/// The extracted value, or None if not found
pub fn extract_value_jsonpath(record: &Record, jsonpath: &str) -> Result<Option<RecordValue>> {
    // Convert record to JSON
    let json_obj: serde_json::Map<String, JsonValue> = record
        .iter()
        .map(|(k, v)| (k.clone(), v.to_json()))
        .collect();
    let json = JsonValue::Object(json_obj);
    let _json_str = serde_json::to_string(&json)
        .map_err(|e| RmlError::Parse(format!("Failed to serialize record: {}", e)))?;

    // Apply JSONPath
    use jsonpath_rust::JsonPath;
    use std::str::FromStr;
    
    let json_path = JsonPath::from_str(jsonpath)
        .map_err(|e| RmlError::Parse(format!("Invalid JSONPath expression '{}': {}", jsonpath, e)))?;
    
    let result = json_path.find(&json);

    match result {
        JsonValue::Null => Ok(None),
        JsonValue::Array(arr) if arr.is_empty() => Ok(None),
        JsonValue::Array(arr) if arr.len() == 1 => Ok(Some(RecordValue::from_json(&arr[0]))),
        JsonValue::Array(arr) => {
            let values: Vec<RecordValue> = arr.iter().map(RecordValue::from_json).collect();
            Ok(Some(RecordValue::Array(values)))
        }
        other => Ok(Some(RecordValue::from_json(&other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_value_as_string() {
        assert_eq!(RecordValue::String("test".to_string()).as_string(), "test");
        assert_eq!(RecordValue::Number(42.0).as_string(), "42");
        assert_eq!(RecordValue::Number(3.14).as_string(), "3.14");
        assert_eq!(RecordValue::Integer(42).as_string(), "42");
        assert_eq!(RecordValue::Boolean(true).as_string(), "true");
        assert_eq!(RecordValue::Null.as_string(), "");
    }

    #[test]
    fn test_record_value_is_empty() {
        assert!(RecordValue::Null.is_empty());
        assert!(RecordValue::String("".to_string()).is_empty());
        assert!(!RecordValue::String("test".to_string()).is_empty());
        assert!(RecordValue::Array(vec![]).is_empty());
        assert!(!RecordValue::Array(vec![RecordValue::Null]).is_empty());
    }

    #[test]
    fn test_record_value_from_json() {
        let json = serde_json::json!({
            "name": "John",
            "age": 30,
            "active": true,
            "score": 3.14,
            "tags": ["rust", "rml"],
            "address": {
                "city": "Boston"
            }
        });

        let value = RecordValue::from_json(&json);
        
        if let RecordValue::Object(obj) = value {
            assert_eq!(obj.get("name"), Some(&RecordValue::String("John".to_string())));
            assert_eq!(obj.get("age"), Some(&RecordValue::Integer(30)));
            assert_eq!(obj.get("active"), Some(&RecordValue::Boolean(true)));
            assert_eq!(obj.get("score"), Some(&RecordValue::Number(3.14)));
        } else {
            panic!("Expected Object");
        }
    }

    #[test]
    fn test_csv_record_iterator() {
        let csv_data = "name,age,city\nAlice,30,Boston\nBob,25,NYC\n";
        let mut iterator = CsvRecordIterator::from_string(csv_data.to_string(), None).unwrap();

        let record1 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record1.get("name"), Some(&RecordValue::String("Alice".to_string())));
        assert_eq!(record1.get("age"), Some(&RecordValue::String("30".to_string())));
        assert_eq!(record1.get("city"), Some(&RecordValue::String("Boston".to_string())));

        let record2 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record2.get("name"), Some(&RecordValue::String("Bob".to_string())));
        assert_eq!(record2.get("age"), Some(&RecordValue::String("25".to_string())));

        assert!(iterator.next_record().unwrap().is_none());
    }

    #[test]
    fn test_csv_record_iterator_custom_delimiter() {
        let csv_data = "name;age;city\nAlice;30;Boston\n";
        let mut iterator = CsvRecordIterator::from_string(csv_data.to_string(), Some(b';')).unwrap();

        let record = iterator.next_record().unwrap().unwrap();
        assert_eq!(record.get("name"), Some(&RecordValue::String("Alice".to_string())));
        assert_eq!(record.get("age"), Some(&RecordValue::String("30".to_string())));
    }

    #[test]
    fn test_csv_record_iterator_empty_values() {
        let csv_data = "name,age,city\nAlice,,Boston\n";
        let mut iterator = CsvRecordIterator::from_string(csv_data.to_string(), None).unwrap();

        let record = iterator.next_record().unwrap().unwrap();
        assert_eq!(record.get("name"), Some(&RecordValue::String("Alice".to_string())));
        assert_eq!(record.get("age"), Some(&RecordValue::Null));
        assert_eq!(record.get("city"), Some(&RecordValue::String("Boston".to_string())));
    }

    #[test]
    fn test_json_record_iterator_array() {
        let json_data = r#"[
            {"name": "Alice", "age": 30},
            {"name": "Bob", "age": 25}
        ]"#;
        
        let mut iterator = JsonRecordIterator::from_string(json_data.to_string(), None).unwrap();

        let record1 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record1.get("name"), Some(&RecordValue::String("Alice".to_string())));
        assert_eq!(record1.get("age"), Some(&RecordValue::Integer(30)));

        let record2 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record2.get("name"), Some(&RecordValue::String("Bob".to_string())));

        assert!(iterator.next_record().unwrap().is_none());
    }

    #[test]
    fn test_json_record_iterator_with_jsonpath() {
        let json_data = r#"{
            "users": [
                {"name": "Alice", "age": 30},
                {"name": "Bob", "age": 25}
            ]
        }"#;
        
        let mut iterator = JsonRecordIterator::from_string(
            json_data.to_string(),
            Some("$.users[*]")
        ).unwrap();

        let record1 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record1.get("name"), Some(&RecordValue::String("Alice".to_string())));

        let record2 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record2.get("name"), Some(&RecordValue::String("Bob".to_string())));

        assert!(iterator.next_record().unwrap().is_none());
    }

    #[test]
    fn test_json_record_iterator_single_object() {
        let json_data = r#"{"name": "Alice", "age": 30}"#;
        
        let mut iterator = JsonRecordIterator::from_string(json_data.to_string(), None).unwrap();

        let record = iterator.next_record().unwrap().unwrap();
        assert_eq!(record.get("name"), Some(&RecordValue::String("Alice".to_string())));
        assert_eq!(record.get("age"), Some(&RecordValue::Integer(30)));

        assert!(iterator.next_record().unwrap().is_none());
    }

    #[test]
    fn test_xml_record_iterator() {
        let xml_data = r#"
            <root>
                <person>
                    <name>Alice</name>
                    <age>30</age>
                </person>
                <person>
                    <name>Bob</name>
                    <age>25</age>
                </person>
            </root>
        "#;
        
        let mut iterator = XmlRecordIterator::from_string(
            xml_data.to_string(),
            Some("//person")
        ).unwrap();

        let record1 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record1.get("_name"), Some(&RecordValue::String("person".to_string())));
        
        let record2 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record2.get("_name"), Some(&RecordValue::String("person".to_string())));

        assert!(iterator.next_record().unwrap().is_none());
    }

    #[test]
    fn test_xml_record_iterator_with_attributes() {
        let xml_data = r#"<person id="1" name="Alice">30</person>"#;
        
        let mut iterator = XmlRecordIterator::from_string(
            xml_data.to_string(),
            Some("/*")
        ).unwrap();

        let record = iterator.next_record().unwrap().unwrap();
        assert_eq!(record.get("@id"), Some(&RecordValue::String("1".to_string())));
        assert_eq!(record.get("@name"), Some(&RecordValue::String("Alice".to_string())));
        assert_eq!(record.get("_text"), Some(&RecordValue::String("30".to_string())));
    }

    #[test]
    fn test_extract_value_simple() {
        let mut record = Record::new();
        record.insert("name".to_string(), RecordValue::String("Alice".to_string()));
        record.insert("age".to_string(), RecordValue::Integer(30));

        assert_eq!(
            extract_value(&record, "name"),
            Some(RecordValue::String("Alice".to_string()))
        );
        assert_eq!(extract_value(&record, "age"), Some(RecordValue::Integer(30)));
        assert_eq!(extract_value(&record, "missing"), None);
    }

    #[test]
    fn test_extract_value_nested() {
        let mut address = HashMap::new();
        address.insert("city".to_string(), RecordValue::String("Boston".to_string()));
        address.insert("zip".to_string(), RecordValue::String("02101".to_string()));

        let mut record = Record::new();
        record.insert("name".to_string(), RecordValue::String("Alice".to_string()));
        record.insert("address".to_string(), RecordValue::Object(address));

        assert_eq!(
            extract_value(&record, "address.city"),
            Some(RecordValue::String("Boston".to_string()))
        );
        assert_eq!(
            extract_value(&record, "address.zip"),
            Some(RecordValue::String("02101".to_string()))
        );
        assert_eq!(extract_value(&record, "address.missing"), None);
    }

    #[test]
    fn test_extract_value_jsonpath() {
        let mut record = Record::new();
        record.insert("name".to_string(), RecordValue::String("Alice".to_string()));
        record.insert("age".to_string(), RecordValue::Integer(30));

        let result = extract_value_jsonpath(&record, "$.name").unwrap();
        assert_eq!(result, Some(RecordValue::String("Alice".to_string())));

        let result = extract_value_jsonpath(&record, "$.age").unwrap();
        assert_eq!(result, Some(RecordValue::Integer(30)));
    }

    #[test]
    fn test_records_factory_csv() {
        use crate::access::LocalFileAccess;
        use tempfile::NamedTempFile;
        use std::io::Write;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "name,age").unwrap();
        writeln!(temp_file, "Alice,30").unwrap();
        writeln!(temp_file, "Bob,25").unwrap();
        temp_file.flush().unwrap();

        let access = LocalFileAccess::new(
            temp_file.path().to_path_buf(),
            None,
            Some("text/csv".to_string()),
        );

        let mut factory = RecordsFactory::new();
        let mut iterator = factory
            .create_iterator(Box::new(access), reference_formulation::CSV, None)
            .unwrap();

        let records = iterator.collect_all().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].get("name"), Some(&RecordValue::String("Alice".to_string())));
        assert_eq!(records[1].get("name"), Some(&RecordValue::String("Bob".to_string())));
    }

    #[test]
    fn test_records_factory_json() {
        use crate::access::LocalFileAccess;
        use tempfile::NamedTempFile;
        use std::io::Write;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, r#"[{{"name":"Alice","age":30}},{{"name":"Bob","age":25}}]"#).unwrap();
        temp_file.flush().unwrap();

        let access = LocalFileAccess::new(
            temp_file.path().to_path_buf(),
            None,
            Some("application/json".to_string()),
        );

        let mut factory = RecordsFactory::new();
        let mut iterator = factory
            .create_iterator(Box::new(access), reference_formulation::JSON_PATH, None)
            .unwrap();

        let records = iterator.collect_all().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].get("name"), Some(&RecordValue::String("Alice".to_string())));
    }

    #[test]
    fn test_records_factory_cache() {
        let mut factory = RecordsFactory::new();
        
        let mut record = Record::new();
        record.insert("name".to_string(), RecordValue::String("Alice".to_string()));
        
        let records = vec![record];
        factory.cache_records("test_key".to_string(), records.clone());

        assert_eq!(factory.get_cached("test_key"), Some(&records));
        assert_eq!(factory.get_cached("missing_key"), None);

        factory.clear_cache();
        assert_eq!(factory.get_cached("test_key"), None);
    }

    #[test]
    fn test_collect_all() {
        let csv_data = "name,age\nAlice,30\nBob,25\n";
        let mut iterator = CsvRecordIterator::from_string(csv_data.to_string(), None).unwrap();

        let records = iterator.collect_all().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].get("name"), Some(&RecordValue::String("Alice".to_string())));
        assert_eq!(records[1].get("name"), Some(&RecordValue::String("Bob".to_string())));
    }

    #[test]
    fn test_csv_with_boolean_values() {
        let csv_data = "name,active\nAlice,true\nBob,false\n";
        let mut iterator = CsvRecordIterator::from_string(csv_data.to_string(), None).unwrap();

        let record1 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record1.get("active"), Some(&RecordValue::String("true".to_string())));

        let record2 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record2.get("active"), Some(&RecordValue::String("false".to_string())));
    }

    #[test]
    fn test_csv_with_float_values() {
        let csv_data = "name,score\nAlice,3.14\nBob,2.71\n";
        let mut iterator = CsvRecordIterator::from_string(csv_data.to_string(), None).unwrap();

        let record1 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record1.get("score"), Some(&RecordValue::String("3.14".to_string())));

        let record2 = iterator.next_record().unwrap().unwrap();
        assert_eq!(record2.get("score"), Some(&RecordValue::String("2.71".to_string())));
    }

    #[test]
    fn test_record_value_array_as_string() {
        let arr = RecordValue::Array(vec![
            RecordValue::String("a".to_string()),
            RecordValue::String("b".to_string()),
        ]);
        assert_eq!(arr.as_string(), "[a, b]");
    }

    #[test]
    fn test_json_nested_objects() {
        let json_data = r#"{
            "person": {
                "name": "Alice",
                "address": {
                    "city": "Boston"
                }
            }
        }"#;
        
        let mut iterator = JsonRecordIterator::from_string(json_data.to_string(), None).unwrap();
        let record = iterator.next_record().unwrap().unwrap();
        
        if let Some(RecordValue::Object(person)) = record.get("person") {
            assert_eq!(person.get("name"), Some(&RecordValue::String("Alice".to_string())));
            
            if let Some(RecordValue::Object(address)) = person.get("address") {
                assert_eq!(address.get("city"), Some(&RecordValue::String("Boston".to_string())));
            } else {
                panic!("Expected nested address object");
            }
        } else {
            panic!("Expected person object");
        }
    }
}
