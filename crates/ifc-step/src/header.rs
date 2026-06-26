use crate::error::StepError;

/// IFC schema version detected from the STEP file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StepSchema {
    #[default]
    Ifc2x3,
    Ifc4,
    Ifc4x1,
    Ifc4x3Rc1,
    Ifc4x3Add2,
}

impl StepSchema {
    /// Parse a schema string from the FILE_SCHEMA header.
    pub fn from_header_str(s: &str) -> Result<Self, StepError> {
        let upper = s.trim().to_uppercase();
        // Normalize various schema strings to our supported versions
        if upper.contains("IFC2X3") {
            Ok(StepSchema::Ifc2x3)
        } else if upper.contains("IFC4X3_ADD2") {
            Ok(StepSchema::Ifc4x3Add2)
        } else if upper.contains("IFC4X3_RC1") {
            Ok(StepSchema::Ifc4x3Rc1)
        } else if upper.contains("IFC4X3") {
            // Unsuffixed IFC4X3 is treated as the modern ADD2 line.
            Ok(StepSchema::Ifc4x3Add2)
        } else if upper.contains("IFC4X2") {
            // IFC4x2 maps to IFC4x3 (closest supported)
            Ok(StepSchema::Ifc4x3Rc1)
        } else if upper.contains("IFC4X1") {
            Ok(StepSchema::Ifc4x1)
        } else if upper.contains("IFC4") {
            Ok(StepSchema::Ifc4)
        } else {
            Err(StepError::UnsupportedSchema(s.to_string()))
        }
    }
}

impl std::fmt::Display for StepSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepSchema::Ifc2x3 => write!(f, "IFC2X3"),
            StepSchema::Ifc4 => write!(f, "IFC4"),
            StepSchema::Ifc4x1 => write!(f, "IFC4X1"),
            StepSchema::Ifc4x3Rc1 => write!(f, "IFC4X3_RC1"),
            StepSchema::Ifc4x3Add2 => write!(f, "IFC4X3_ADD2"),
        }
    }
}

/// Parsed STEP file header information.
#[derive(Debug, Clone, Default)]
pub struct StepHeader {
    /// The IFC schema version.
    pub schema: StepSchema,
    /// File description strings.
    pub description: Vec<String>,
    /// FILE_NAME quoted strings in source order.
    pub file_name: Vec<String>,
    /// Original schema string from the file.
    pub schema_raw: String,
}

/// Parse the STEP file header to extract the schema version.
pub fn parse_header(data: &[u8]) -> Result<StepHeader, StepError> {
    let search_text = String::from_utf8_lossy(data).into_owned();

    // Find FILE_SCHEMA(('...'))
    let schema_raw = extract_file_schema(&search_text)?;
    let schema = StepSchema::from_header_str(&schema_raw)?;
    let description = extract_file_description(&search_text).unwrap_or_default();
    let file_name = extract_file_name(&search_text).unwrap_or_default();

    Ok(StepHeader {
        schema,
        description,
        file_name,
        schema_raw,
    })
}

fn extract_file_schema(text: &str) -> Result<String, StepError> {
    let invocation = find_header_invocation(text, "FILE_SCHEMA")?;
    extract_quoted_strings(invocation)
        .into_iter()
        .next()
        .ok_or_else(|| StepError::InvalidHeader("No quoted string in FILE_SCHEMA".to_string()))
}

fn extract_file_description(text: &str) -> Result<Vec<String>, StepError> {
    let invocation = find_header_invocation(text, "FILE_DESCRIPTION")?;
    Ok(extract_quoted_strings(invocation))
}

fn extract_file_name(text: &str) -> Result<Vec<String>, StepError> {
    let invocation = find_header_invocation(text, "FILE_NAME")?;
    Ok(extract_quoted_strings(invocation))
}

fn find_header_invocation<'a>(text: &'a str, keyword: &str) -> Result<&'a str, StepError> {
    let header_end = text.to_uppercase().find("ENDSEC;").unwrap_or(text.len());
    let header_text = &text[..header_end];
    let upper = header_text.to_uppercase();
    let pos = upper
        .find(keyword)
        .ok_or_else(|| StepError::InvalidHeader(format!("{keyword} not found")))?;
    let after_keyword = &header_text[pos + keyword.len()..];
    let paren_offset = after_keyword
        .find('(')
        .ok_or_else(|| StepError::InvalidHeader(format!("No '(' found after {keyword}")))?;
    let invocation = &after_keyword[paren_offset..];
    let end = find_statement_end(invocation)
        .ok_or_else(|| StepError::InvalidHeader(format!("Unterminated {keyword} statement")))?;
    Ok(&invocation[..end])
}

fn find_statement_end(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                if in_string && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 1;
                } else {
                    in_string = !in_string;
                }
            }
            b'(' if !in_string => depth += 1,
            b')' if !in_string => depth = depth.saturating_sub(1),
            b';' if !in_string && depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }

    None
}

fn find_matching_paren(text: &str, open_index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = open_index;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                if in_string && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 1;
                } else {
                    in_string = !in_string;
                }
            }
            b'(' if !in_string => depth += 1,
            b')' if !in_string => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }

    None
}

fn extract_quoted_strings(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut values = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if in_string => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    current.push('\'');
                    i += 1;
                } else {
                    values.push(std::mem::take(&mut current));
                    in_string = false;
                }
            }
            b'\'' => in_string = true,
            _ if in_string => current.push(bytes[i] as char),
            _ => {}
        }
        i += 1;
    }

    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ifc2x3() {
        let schema = StepSchema::from_header_str("IFC2X3").unwrap();
        assert_eq!(schema, StepSchema::Ifc2x3);
    }

    #[test]
    fn test_parse_ifc2x3_tc1() {
        let schema = StepSchema::from_header_str("IFC2X3_TC1").unwrap();
        assert_eq!(schema, StepSchema::Ifc2x3);
    }

    #[test]
    fn test_parse_ifc4() {
        let schema = StepSchema::from_header_str("IFC4").unwrap();
        assert_eq!(schema, StepSchema::Ifc4);
    }

    #[test]
    fn test_parse_ifc4_add2() {
        let schema = StepSchema::from_header_str("IFC4_ADD2").unwrap();
        assert_eq!(schema, StepSchema::Ifc4);
    }

    #[test]
    fn test_parse_ifc4x3() {
        let schema = StepSchema::from_header_str("IFC4X3_RC1").unwrap();
        assert_eq!(schema, StepSchema::Ifc4x3Rc1);
    }

    #[test]
    fn test_parse_ifc4x3_add2() {
        let schema = StepSchema::from_header_str("IFC4X3_ADD2").unwrap();
        assert_eq!(schema, StepSchema::Ifc4x3Add2);
    }

    #[test]
    fn test_parse_ifc4x3_unsuffixed_defaults_to_add2() {
        let schema = StepSchema::from_header_str("IFC4X3").unwrap();
        assert_eq!(schema, StepSchema::Ifc4x3Add2);
    }

    #[test]
    fn test_extract_file_schema() {
        let text = "FILE_SCHEMA(('IFC2X3'));";
        let result = extract_file_schema(text).unwrap();
        assert_eq!(result, "IFC2X3");
    }

    #[test]
    fn test_extract_file_schema_with_spaces() {
        let text = "FILE_SCHEMA (( 'IFC4' ));";
        let result = extract_file_schema(text).unwrap();
        assert_eq!(result, "IFC4");
    }

    #[test]
    fn test_parse_header_extracts_description() {
        let data = br"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView_V2.0]','Example ''quoted'' value'),'2;1');
FILE_SCHEMA(('IFC4X3_ADD2'));
ENDSEC;
DATA;
ENDSEC;
END-ISO-10303-21;";

        let header = parse_header(data).unwrap();
        assert_eq!(header.schema, StepSchema::Ifc4x3Add2);
        assert_eq!(
            header.description,
            vec![
                "ViewDefinition [CoordinationView_V2.0]".to_string(),
                "Example 'quoted' value".to_string()
            ]
        );
    }
}
