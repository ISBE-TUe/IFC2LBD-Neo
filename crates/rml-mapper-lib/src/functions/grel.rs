//! GREL (General Refine Expression Language) Functions
//!
//! This module implements GREL string manipulation functions commonly used
//! in RML mappings. These functions are based on the OpenRefine GREL language.

use crate::error::{Result, RmlError};
use crate::functions::{FunctionExecutor, get_required_param, get_optional_param, get_all_params};
use std::collections::HashMap;

/// Base URL for GREL function IRIs
pub const GREL_BASE: &str = "http://users.ugent.be/~bjdmeest/function/grel.ttl#";

/// Parameter name for the main value input
pub const VALUE_PARAM: &str = "valueParameter";


/// Parameter name for string parameters
pub const STRING_PARAM: &str = "stringParameter";

/// Parameter name for delimiter parameters
pub const DELIMITER_PARAM: &str = "delimiterParameter";

/// Parameter name for the "from" parameter in replace
pub const FROM_PARAM: &str = "fromParameter";

/// Parameter name for the "to" parameter in replace
pub const TO_PARAM: &str = "toParameter";

/// Parameter name for the start index in substring
pub const START_PARAM: &str = "startIndexParameter";

/// Parameter name for the end index in substring
pub const END_PARAM: &str = "endIndexParameter";

/// GREL toUpperCase function
///
/// Converts a string to uppercase.
///
/// Parameters:
/// - valueParameter: The string to convert
///
/// Returns: The uppercase string
pub struct ToUpperCaseFunction;

impl FunctionExecutor for ToUpperCaseFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        Ok(vec![value.to_uppercase()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "toUpperCase");
        IRI
    }
}

/// GREL toLowerCase function
///
/// Converts a string to lowercase.
///
/// Parameters:
/// - valueParameter: The string to convert
///
/// Returns: The lowercase string
pub struct ToLowerCaseFunction;

impl FunctionExecutor for ToLowerCaseFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        Ok(vec![value.to_lowercase()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "toLowerCase");
        IRI
    }
}

/// GREL trim function
///
/// Removes leading and trailing whitespace from a string.
///
/// Parameters:
/// - valueParameter: The string to trim
///
/// Returns: The trimmed string
pub struct TrimFunction;

impl FunctionExecutor for TrimFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        Ok(vec![value.trim().to_string()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "trim");
        IRI
    }
}

/// GREL replace function
///
/// Replaces all occurrences of a substring with another string.
///
/// Parameters:
/// - valueParameter: The string to perform replacement on
/// - fromParameter: The substring to find
/// - toParameter: The replacement string
///
/// Returns: The string with replacements made
pub struct ReplaceFunction;

impl FunctionExecutor for ReplaceFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        let from = get_required_param(params, FROM_PARAM)?;
        let to = get_required_param(params, TO_PARAM)?;
        
        Ok(vec![value.replace(&from, &to)])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "replace");
        IRI
    }
}

/// GREL split function
///
/// Splits a string by a delimiter.
///
/// Parameters:
/// - valueParameter: The string to split
/// - delimiterParameter: The delimiter to split by
///
/// Returns: A vector of split strings
pub struct SplitFunction;

impl FunctionExecutor for SplitFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        let delimiter = get_required_param(params, DELIMITER_PARAM)?;
        
        let parts: Vec<String> = value
            .split(&delimiter)
            .map(|s| s.to_string())
            .collect();
        
        Ok(parts)
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "split");
        IRI
    }
}

/// GREL concat function
///
/// Concatenates multiple strings together.
///
/// Parameters:
/// - valueParameter: One or more strings to concatenate
///
/// Returns: The concatenated string
pub struct ConcatFunction;

impl FunctionExecutor for ConcatFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let values = get_all_params(params, VALUE_PARAM);
        
        if values.is_empty() {
            return Err(RmlError::Function(
                "concat requires at least one value".to_string()
            ));
        }
        
        Ok(vec![values.join("")])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "concat");
        IRI
    }
}

/// GREL substring function
///
/// Extracts a substring from a string.
///
/// Parameters:
/// - valueParameter: The string to extract from
/// - startIndexParameter: The start index (0-based)
/// - endIndexParameter: The end index (optional, defaults to end of string)
///
/// Returns: The extracted substring
pub struct SubstringFunction;

impl FunctionExecutor for SubstringFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        let start_str = get_required_param(params, START_PARAM)?;
        let end_str = get_optional_param(params, END_PARAM);
        
        let start: usize = start_str.parse().map_err(|_| {
            RmlError::Function(format!("Invalid start index: {}", start_str))
        })?;
        
        let result = if let Some(end_s) = end_str {
            let end: usize = end_s.parse().map_err(|_| {
                RmlError::Function(format!("Invalid end index: {}", end_s))
            })?;
            
            if start > value.len() || end > value.len() || start > end {
                return Err(RmlError::Function(format!(
                    "Invalid substring indices: start={}, end={}, length={}",
                    start, end, value.len()
                )));
            }
            
            value.chars().skip(start).take(end - start).collect()
        } else {
            if start > value.len() {
                return Err(RmlError::Function(format!(
                    "Start index {} exceeds string length {}",
                    start, value.len()
                )));
            }
            
            value.chars().skip(start).collect()
        };
        
        Ok(vec![result])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "substring");
        IRI
    }
}

/// GREL length function
///
/// Returns the length of a string.
///
/// Parameters:
/// - valueParameter: The string to measure
///
/// Returns: The length as a string
pub struct LengthFunction;

impl FunctionExecutor for LengthFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        Ok(vec![value.len().to_string()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "length");
        IRI
    }
}

/// GREL contains function
///
/// Checks if a string contains a substring.
///
/// Parameters:
/// - valueParameter: The string to search in
/// - stringParameter: The substring to search for
///
/// Returns: "true" or "false"
pub struct ContainsFunction;

impl FunctionExecutor for ContainsFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        let search = get_required_param(params, STRING_PARAM)?;
        
        Ok(vec![value.contains(&search).to_string()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "contains");
        IRI
    }
}

/// GREL startsWith function
///
/// Checks if a string starts with a prefix.
///
/// Parameters:
/// - valueParameter: The string to check
/// - stringParameter: The prefix to check for
///
/// Returns: "true" or "false"
pub struct StartsWithFunction;

impl FunctionExecutor for StartsWithFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        let prefix = get_required_param(params, STRING_PARAM)?;
        
        Ok(vec![value.starts_with(&prefix).to_string()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "startsWith");
        IRI
    }
}

/// GREL endsWith function
///
/// Checks if a string ends with a suffix.
///
/// Parameters:
/// - valueParameter: The string to check
/// - stringParameter: The suffix to check for
///
/// Returns: "true" or "false"
pub struct EndsWithFunction;

impl FunctionExecutor for EndsWithFunction {
    fn execute(&self, params: &HashMap<String, Vec<String>>) -> Result<Vec<String>> {
        let value = get_required_param(params, VALUE_PARAM)?;
        let suffix = get_required_param(params, STRING_PARAM)?;
        
        Ok(vec![value.ends_with(&suffix).to_string()])
    }
    
    fn function_iri(&self) -> &str {
        const IRI: &str = const_format::concatcp!(GREL_BASE, "endsWith");
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
    fn test_to_upper_case() {
        let func = ToUpperCaseFunction;
        let params = make_params(VALUE_PARAM, "hello world");
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["HELLO WORLD"]);
    }

    #[test]
    fn test_to_lower_case() {
        let func = ToLowerCaseFunction;
        let params = make_params(VALUE_PARAM, "HELLO WORLD");
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn test_trim() {
        let func = TrimFunction;
        let params = make_params(VALUE_PARAM, "  hello world  ");
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn test_replace() {
        let func = ReplaceFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello world".to_string()]);
        params.insert(FROM_PARAM.to_string(), vec!["world".to_string()]);
        params.insert(TO_PARAM.to_string(), vec!["rust".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["hello rust"]);
    }

    #[test]
    fn test_split() {
        let func = SplitFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["a,b,c".to_string()]);
        params.insert(DELIMITER_PARAM.to_string(), vec![",".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_concat() {
        let func = ConcatFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec![
            "hello".to_string(),
            " ".to_string(),
            "world".to_string(),
        ]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn test_substring_with_end() {
        let func = SubstringFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello world".to_string()]);
        params.insert(START_PARAM.to_string(), vec!["0".to_string()]);
        params.insert(END_PARAM.to_string(), vec!["5".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn test_substring_without_end() {
        let func = SubstringFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello world".to_string()]);
        params.insert(START_PARAM.to_string(), vec!["6".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["world"]);
    }

    #[test]
    fn test_length() {
        let func = LengthFunction;
        let params = make_params(VALUE_PARAM, "hello");
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["5"]);
    }

    #[test]
    fn test_contains_true() {
        let func = ContainsFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello world".to_string()]);
        params.insert(STRING_PARAM.to_string(), vec!["world".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["true"]);
    }

    #[test]
    fn test_contains_false() {
        let func = ContainsFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello world".to_string()]);
        params.insert(STRING_PARAM.to_string(), vec!["rust".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["false"]);
    }

    #[test]
    fn test_starts_with_true() {
        let func = StartsWithFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello world".to_string()]);
        params.insert(STRING_PARAM.to_string(), vec!["hello".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["true"]);
    }

    #[test]
    fn test_starts_with_false() {
        let func = StartsWithFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello world".to_string()]);
        params.insert(STRING_PARAM.to_string(), vec!["world".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["false"]);
    }

    #[test]
    fn test_ends_with_true() {
        let func = EndsWithFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello world".to_string()]);
        params.insert(STRING_PARAM.to_string(), vec!["world".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["true"]);
    }

    #[test]
    fn test_ends_with_false() {
        let func = EndsWithFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello world".to_string()]);
        params.insert(STRING_PARAM.to_string(), vec!["hello".to_string()]);
        
        let result = func.execute(&params).unwrap();
        assert_eq!(result, vec!["false"]);
    }

    #[test]
    fn test_substring_invalid_indices() {
        let func = SubstringFunction;
        let mut params = HashMap::new();
        params.insert(VALUE_PARAM.to_string(), vec!["hello".to_string()]);
        params.insert(START_PARAM.to_string(), vec!["10".to_string()]);
        
        let result = func.execute(&params);
        assert!(result.is_err());
    }

    #[test]
    fn test_concat_empty() {
        let func = ConcatFunction;
        let params = HashMap::new();
        
        let result = func.execute(&params);
        assert!(result.is_err());
    }
}
