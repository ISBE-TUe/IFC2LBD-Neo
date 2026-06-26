//! IDLab Functions
//!
//! This module implements IDLab utility functions commonly used in RML mappings.
//! These functions provide utilities for equality checks, null checks, random values,
//! UUIDs, and URL slugification.

use crate::error::{Result, RmlError};
use crate::functions::{FunctionExecutor, get_required_param, get_optional_param};
use std::collections::HashMap;
use regex::Regex;

/// Base URL for IDLab function IRIs
pub const IDLAB_BASE: &str = "http://example.com/idlab/function/";

/// Parameter names
pub const STR1_PARAM: &str = "str1";
pub const STR2_PARAM: &str = "str2";
pub const VALUE_PARAM: &str = "value";
pub const TEXT_PARAM: &str = "text";

/// IDLab equal function
///
/// Checks if two strings are equal. Used for join conditions.
///
/// Parameters:
/// - str1: First string
/// - str2: Second string
///
/// Returns: "true" or "false"
pub struct EqualFunction;

impl FunctionExecutor for EqualFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let str1 = get_required_param(params, STR1_PARAM)?;
        let str2 = get_required_param(params, STR2_PARAM)?;
        
        Ok(vec![(str1 == str2).to_string()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(IDLAB_BASE, "equal");
        IRI
    }
}

/// IDLab notEqual function
///
/// Checks if two strings are not equal.
///
/// Parameters:
/// - str1: First string
/// - str2: Second string
///
/// Returns: "true" or "false"
pub struct NotEqualFunction;

impl FunctionExecutor for NotEqualFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let str1 = get_required_param(params, STR1_PARAM)?;
        let str2 = get_required_param(params, STR2_PARAM)?;
        
        Ok(vec![(str1 != str2).to_string()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(IDLAB_BASE, "notEqual");
        IRI
    }
}

/// IDLab isNull function
///
/// Checks if a value is null or empty.
///
/// Parameters:
/// - value: The value to check
///
/// Returns: "true" or "false"
pub struct IsNullFunction;

impl FunctionExecutor for IsNullFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_optional_param(params, VALUE_PARAM);
        
        let is_null = match value {
            None => true,
            Some(v) => v.is_empty(),
        };
        
        Ok(vec![is_null.to_string()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(IDLAB_BASE, "isNull");
        IRI
    }
}

/// IDLab random function
///
/// Generates a random number between 0 and 1.
///
/// Parameters: None
///
/// Returns: A random number as a string
pub struct RandomFunction;

impl FunctionExecutor for RandomFunction {
    fn execute(&self, _params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        use std::time::{SystemTime, UNIX_EPOCH};
        
        // Use current time as a simple random source
        // For production use, consider using the `rand` crate
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| RmlError::Function(format!("Time error: {}", e)))?;
        
        let nanos = now.subsec_nanos();
        let random = (nanos % 1_000_000) as f64 / 1_000_000.0;
        
        Ok(vec![random.to_string()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(IDLAB_BASE, "random");
        IRI
    }
}

/// IDLab uuid function
///
/// Generates a UUID (Universally Unique Identifier).
///
/// Parameters: None
///
/// Returns: A UUID string in the format "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
pub struct UuidFunction;

impl FunctionExecutor for UuidFunction {
    fn execute(&self, _params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        // Generate a simple UUID v4 using random bytes
        use std::time::{SystemTime, UNIX_EPOCH};
        
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| RmlError::Function(format!("Time error: {}", e)))?;
        
        let nanos = now.as_nanos();
        
        // Simple UUID generation (not cryptographically secure)
        // For production use, consider using the `uuid` crate
        let uuid = format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            (nanos & 0xFFFFFFFF) as u32,
            ((nanos >> 32) & 0xFFFF) as u16,
            ((nanos >> 48) & 0xFFF) as u16,
            ((nanos >> 60) & 0x3FFF | 0x8000) as u16,
            (nanos >> 76) as u64 & 0xFFFFFFFFFFFF,
        );
        
        Ok(vec![uuid])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(IDLAB_BASE, "uuid");
        IRI
    }
}

/// IDLab slugify function
///
/// Creates a URL-safe slug from a string by:
/// - Converting to lowercase
/// - Replacing spaces and special characters with hyphens
/// - Removing consecutive hyphens
/// - Trimming leading/trailing hyphens
///
/// Parameters:
/// - text: The text to slugify
///
/// Returns: A URL-safe slug
pub struct SlugifyFunction;

impl FunctionExecutor for SlugifyFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let text = get_required_param(params, TEXT_PARAM)?;
        
        // Convert to lowercase
        let mut slug = text.to_lowercase();
        
        // Replace spaces with hyphens
        slug = slug.replace(' ', "-");
        
        // Remove non-alphanumeric characters (except hyphens)
        let re = Regex::new(r"[^a-z0-9\-]").unwrap();
        slug = re.replace_all(&slug, "").to_string();
        
        // Replace multiple consecutive hyphens with a single hyphen
        let re = Regex::new(r"-+").unwrap();
        slug = re.replace_all(&slug, "-").to_string();
        
        // Trim leading and trailing hyphens
        slug = slug.trim_matches('-').to_string();
        
        Ok(vec![slug])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(IDLAB_BASE, "slugify");
        IRI
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_params(key: &str, value: &str) -> HashMap<String, Vec<String>> {
        let mut params = HashMap::new();
        params.insert(key.to_string(), vec![value.to_string()]);
        params
    }

    #[test]
    fn test_equal_true() {
        let func = EqualFunction;
        let mut params = HashMap::new();
        params.insert(STR1_PARAM.to_string(), vec!["hello".to_string()]);
        params.insert(STR2_PARAM.to_string(), vec!["hello".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["true"]);
    }

    #[test]
    fn test_equal_false() {
        let func = EqualFunction;
        let mut params = HashMap::new();
        params.insert(STR1_PARAM.to_string(), vec!["hello".to_string()]);
        params.insert(STR2_PARAM.to_string(), vec!["world".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["false"]);
    }

    #[test]
    fn test_not_equal_true() {
        let func = NotEqualFunction;
        let mut params = HashMap::new();
        params.insert(STR1_PARAM.to_string(), vec!["hello".to_string()]);
        params.insert(STR2_PARAM.to_string(), vec!["world".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["true"]);
    }

    #[test]
    fn test_not_equal_false() {
        let func = NotEqualFunction;
        let mut params = HashMap::new();
        params.insert(STR1_PARAM.to_string(), vec!["hello".to_string()]);
        params.insert(STR2_PARAM.to_string(), vec!["hello".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["false"]);
    }

    #[test]
    fn test_is_null_true_missing() {
        let func = IsNullFunction;
        let params = HashMap::new();
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["true"]);
    }

    #[test]
    fn test_is_null_true_empty() {
        let func = IsNullFunction;
        let params = make_params(VALUE_PARAM, "");
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["true"]);
    }

    #[test]
    fn test_is_null_false() {
        let func = IsNullFunction;
        let params = make_params(VALUE_PARAM, "hello");
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["false"]);
    }

    #[test]
    fn test_random() {
        let func = RandomFunction;
        let params = HashMap::new();
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result.len(), 1);
        
        // Parse as float to verify it's a valid number
        let value: f64 = result[0].parse().unwrap();
        assert!(value >= 0.0 && value <= 1.0);
    }

    #[test]
    fn test_uuid() {
        let func = UuidFunction;
        let params = HashMap::new();
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result.len(), 1);
        
        // Check UUID format (8-4-4-4-12 hex digits)
        let uuid = &result[0];
        let parts: Vec<&str> = uuid.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
    }

    #[test]
    fn test_slugify_simple() {
        let func = SlugifyFunction;
        let params = make_params(TEXT_PARAM, "Hello World");
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["hello-world"]);
    }

    #[test]
    fn test_slugify_special_chars() {
        let func = SlugifyFunction;
        let params = make_params(TEXT_PARAM, "Hello, World!");
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["hello-world"]);
    }

    #[test]
    fn test_slugify_multiple_spaces() {
        let func = SlugifyFunction;
        let params = make_params(TEXT_PARAM, "Hello   World");
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["hello-world"]);
    }

    #[test]
    fn test_slugify_leading_trailing() {
        let func = SlugifyFunction;
        let params = make_params(TEXT_PARAM, "  Hello World  ");
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["hello-world"]);
    }

    #[test]
    fn test_slugify_complex() {
        let func = SlugifyFunction;
        let params = make_params(TEXT_PARAM, "The Quick Brown Fox!");
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["the-quick-brown-fox"]);
    }

    #[test]
    fn test_slugify_numbers() {
        let func = SlugifyFunction;
        let params = make_params(TEXT_PARAM, "Test 123");
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["test-123"]);
    }

    #[test]
    fn test_slugify_unicode() {
        let func = SlugifyFunction;
        let params = make_params(TEXT_PARAM, "Café Münchën");
        
        let result = func.execute(&params).unwrap();
        // Non-ASCII characters are removed
        assert_eq!(result, vec!["caf-mnchn"]);
    }
}
