use std::collections::HashMap;

use rayon::prelude::*;
use smol_str::SmolStr;

use crate::error::StepError;
use crate::types::{EntityId, RawEntity, StepValue};
use crate::unicode::decode_ifc_unicode;

/// Parse all STEP entities from the DATA section of a STEP file.
pub fn parse_entities(data: &[u8]) -> Result<HashMap<EntityId, RawEntity>, StepError> {
    let text = String::from_utf8_lossy(data);
    let data_start = find_data_section(&text)?;
    let entity_ranges = scan_entity_ranges(&text, data_start)?;
    let parsed_entities: Vec<RawEntity> = entity_ranges
        .par_iter()
        .map(|&(start, end)| parse_entity(&text[start..end]))
        .collect::<Result<_, _>>()?;

    Ok(parsed_entities
        .into_iter()
        .map(|entity| (entity.id, entity))
        .collect())
}

fn find_data_section(text: &str) -> Result<usize, StepError> {
    let mut offset = 0usize;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        let statement = trimmed.strip_suffix(';').unwrap_or(trimmed).trim();
        if statement.eq_ignore_ascii_case("DATA") {
            return Ok(offset + line.len());
        }
        offset += line.len();
    }

    if offset < text.len() {
        let tail = &text[offset..];
        let trimmed = tail.trim();
        let statement = trimmed.strip_suffix(';').unwrap_or(trimmed).trim();
        if statement.eq_ignore_ascii_case("DATA") {
            return Ok(text.len());
        }
    }

    Err(StepError::InvalidHeader(
        "DATA section not found".to_string(),
    ))
}

fn scan_entity_ranges(text: &str, data_start: usize) -> Result<Vec<(usize, usize)>, StepError> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut in_string = false;
    let mut entity_start = None;
    let mut i = data_start;

    while i < bytes.len() {
        let byte = bytes[i];

        if byte == b'\'' {
            if in_string && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                i += 2;
                continue;
            }
            in_string = !in_string;
            i += 1;
            continue;
        }

        if in_string {
            i += 1;
            continue;
        }

        if entity_start.is_none() {
            if is_statement_at(text, i, "ENDSEC") {
                break;
            }
            if byte == b'#' {
                entity_start = Some(i);
            }
            i += 1;
            continue;
        }

        if byte == b';' {
            let start = entity_start.take().expect("entity start set");
            ranges.push((start, i + 1));
        }

        i += 1;
    }

    Ok(ranges)
}

fn is_statement_at(text: &str, index: usize, keyword: &str) -> bool {
    let bytes = text.as_bytes();
    let keyword_bytes = keyword.as_bytes();
    if index + keyword_bytes.len() > bytes.len() {
        return false;
    }
    if !bytes[index..index + keyword_bytes.len()].eq_ignore_ascii_case(keyword_bytes) {
        return false;
    }

    let before_ok = index == 0 || !is_identifier_byte(bytes[index.saturating_sub(1)]);
    let after_index = index + keyword_bytes.len();
    let after_ok = after_index >= bytes.len() || !is_identifier_byte(bytes[after_index]);
    before_ok && after_ok
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn parse_entity(line: &str) -> Result<RawEntity, StepError> {
    let line = line.trim();
    if !line.starts_with('#') {
        return Err(StepError::Parse {
            line: 0,
            message: "Entity does not start with '#'".to_string(),
        });
    }

    // Remove trailing semicolons
    let line = line.trim_end_matches(';').trim();

    // Parse: #NNN=ENTITY_NAME(args...)
    let eq_pos = line.find('=').ok_or_else(|| StepError::Parse {
        line: 0,
        message: format!("No '=' found in: {}", &line[..line.len().min(80)]),
    })?;

    let id_str = &line[1..eq_pos].trim();
    let id: EntityId = id_str.parse().map_err(|_| StepError::Parse {
        line: 0,
        message: format!("Invalid entity ID: {}", id_str),
    })?;

    let rest = line[eq_pos + 1..].trim();

    // Find entity name and opening paren
    let paren_pos = rest.find('(').ok_or_else(|| StepError::Parse {
        line: id,
        message: format!("No '(' found in: {}", &rest[..rest.len().min(80)]),
    })?;

    let entity_name = SmolStr::new(rest[..paren_pos].trim().to_uppercase());

    // Parse argument list (everything between outer parens)
    let args_str = &rest[paren_pos + 1..];
    // Remove the final closing paren
    let args_str = if let Some(stripped) = args_str.strip_suffix(')') {
        stripped
    } else {
        args_str.trim_end_matches(')')
    };

    let args = parse_arg_list(args_str, id)?;

    Ok(RawEntity {
        id,
        entity_name,
        args,
    })
}

/// Parse a comma-separated argument list.
fn parse_arg_list(input: &str, entity_id: EntityId) -> Result<Vec<StepValue>, StepError> {
    let mut args = Vec::new();
    let mut chars = input.chars().peekable();

    loop {
        skip_whitespace(&mut chars);
        if chars.peek().is_none() {
            break;
        }

        let value = parse_value(&mut chars, entity_id)?;
        args.push(value);

        skip_whitespace(&mut chars);
        match chars.peek() {
            Some(',') => {
                chars.next();
            }
            Some(')') | None => break,
            Some(c) => {
                // Tolerate unexpected characters — skip to next comma
                tracing::warn!(entity_id, "Unexpected character '{}' in argument list", c);
                // Skip to next comma or end
                while let Some(&c) = chars.peek() {
                    if c == ',' || c == ')' {
                        break;
                    }
                    chars.next();
                }
                if chars.peek() == Some(&',') {
                    chars.next();
                }
            }
        }
    }

    Ok(args)
}

fn parse_value(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    entity_id: EntityId,
) -> Result<StepValue, StepError> {
    skip_whitespace(chars);

    match chars.peek() {
        Some('#') => {
            // Reference: #123
            chars.next();
            let num = parse_integer_chars(chars);
            Ok(StepValue::Ref(num as u64))
        }
        Some('\'') => {
            // String literal: 'text'
            chars.next();
            let s = parse_string_literal(chars);
            let decoded = decode_ifc_unicode(&s);
            Ok(StepValue::String(SmolStr::new(decoded)))
        }
        Some('$') => {
            // Null
            chars.next();
            Ok(StepValue::Null)
        }
        Some('*') => {
            // Derived
            chars.next();
            Ok(StepValue::Derived)
        }
        Some('.') => {
            // Enum or boolean: .NOTDEFINED. or .T. or .F.
            chars.next();
            let mut name = String::new();
            while let Some(&c) = chars.peek() {
                if c == '.' {
                    chars.next();
                    break;
                }
                name.push(c);
                chars.next();
            }
            match name.as_str() {
                "T" => Ok(StepValue::Bool(true)),
                "F" => Ok(StepValue::Bool(false)),
                _ => Ok(StepValue::Enum(SmolStr::new(name))),
            }
        }
        Some('(') => {
            // List: (item1, item2, ...)
            chars.next();
            let items = parse_arg_list_inner(chars, entity_id)?;
            // Consume closing paren
            if chars.peek() == Some(&')') {
                chars.next();
            }
            Ok(StepValue::List(items))
        }
        Some(c) if c.is_ascii_digit() || *c == '-' || *c == '+' => {
            // Number: integer or real
            parse_number(chars)
        }
        Some(c) if c.is_ascii_alphabetic() => {
            // Typed value: IFCLENGTHMEASURE(3.14) or entity name
            let name = parse_identifier(chars);
            skip_whitespace(chars);
            if chars.peek() == Some(&'(') {
                chars.next();
                let inner = parse_value(chars, entity_id)?;
                // Consume closing paren
                skip_whitespace(chars);
                if chars.peek() == Some(&')') {
                    chars.next();
                }
                Ok(StepValue::Typed {
                    type_name: SmolStr::new(name.to_uppercase()),
                    value: Box::new(inner),
                })
            } else {
                // Bare identifier — treat as enum
                Ok(StepValue::Enum(SmolStr::new(name.to_uppercase())))
            }
        }
        _ => {
            // Unknown — skip character and return Null
            chars.next();
            Ok(StepValue::Null)
        }
    }
}

fn parse_arg_list_inner(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    entity_id: EntityId,
) -> Result<Vec<StepValue>, StepError> {
    let mut items = Vec::new();

    loop {
        skip_whitespace(chars);
        match chars.peek() {
            None | Some(')') => break,
            _ => {}
        }

        let value = parse_value(chars, entity_id)?;
        items.push(value);

        skip_whitespace(chars);
        match chars.peek() {
            Some(',') => {
                chars.next();
            }
            _ => break,
        }
    }

    Ok(items)
}

fn parse_number(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<StepValue, StepError> {
    let mut num_str = String::new();
    let mut is_real = false;

    // Sign
    if let Some(&c) = chars.peek() {
        if c == '-' || c == '+' {
            num_str.push(c);
            chars.next();
        }
    }

    // Digits
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            num_str.push(c);
            chars.next();
        } else if c == '.' {
            is_real = true;
            num_str.push(c);
            chars.next();
        } else if c == 'E' || c == 'e' {
            is_real = true;
            num_str.push(c);
            chars.next();
            // Optional sign after E
            if let Some(&c2) = chars.peek() {
                if c2 == '+' || c2 == '-' {
                    num_str.push(c2);
                    chars.next();
                }
            }
        } else {
            break;
        }
    }

    if is_real {
        let v: f64 = num_str.parse().unwrap_or(0.0);
        Ok(StepValue::Real(v))
    } else {
        let v: i64 = num_str.parse().unwrap_or(0);
        Ok(StepValue::Int(v))
    }
}

fn parse_string_literal(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut s = String::new();
    while let Some(c) = chars.next() {
        if c == '\'' {
            // Check for escaped quote ('')
            if chars.peek() == Some(&'\'') {
                s.push('\'');
                chars.next();
            } else {
                break;
            }
        } else {
            s.push(c);
        }
    }
    s
}

fn parse_identifier(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '_' {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }
    name
}

fn parse_integer_chars(chars: &mut std::iter::Peekable<std::str::Chars>) -> i64 {
    let mut num_str = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            num_str.push(c);
            chars.next();
        } else {
            break;
        }
    }
    num_str.parse().unwrap_or(0)
}

fn skip_whitespace(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_entity() {
        let data = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC2X3'));\nENDSEC;\nDATA;\n#1=IFCPROJECT('0001',$,$,$,$,$,$,$,$);\nENDSEC;\n";
        let entities = parse_entities(data).unwrap();
        assert!(entities.contains_key(&1));
        let e = &entities[&1];
        assert_eq!(e.entity_name.as_str(), "IFCPROJECT");
        assert_eq!(e.args[0], StepValue::String(SmolStr::new("0001")));
    }

    #[test]
    fn test_parse_references() {
        let data = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n#10=IFCRELAGGREGATES('guid',$,$,$,#20,(#30,#40));\nENDSEC;\n";
        let entities = parse_entities(data).unwrap();
        let e = &entities[&10];
        assert_eq!(e.entity_name.as_str(), "IFCRELAGGREGATES");
        // arg[4] should be Ref(20)
        assert_eq!(e.args[4], StepValue::Ref(20));
        // arg[5] should be List([Ref(30), Ref(40)])
        assert_eq!(
            e.args[5],
            StepValue::List(vec![StepValue::Ref(30), StepValue::Ref(40)])
        );
    }

    #[test]
    fn test_parse_typed_value() {
        let data = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n#1=IFCPROPERTYSINGLEVALUE('Width',$,IFCLENGTHMEASURE(3.5),$);\nENDSEC;\n";
        let entities = parse_entities(data).unwrap();
        let e = &entities[&1];
        assert_eq!(e.entity_name.as_str(), "IFCPROPERTYSINGLEVALUE");
        match &e.args[2] {
            StepValue::Typed { type_name, value } => {
                assert_eq!(type_name.as_str(), "IFCLENGTHMEASURE");
                assert_eq!(**value, StepValue::Real(3.5));
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_enum_and_bool() {
        let data = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n#1=IFCWALL('guid',$,'name',$,$,$,$,$,.NOTDEFINED.);\nENDSEC;\n";
        let entities = parse_entities(data).unwrap();
        let e = &entities[&1];
        // Last arg should be Enum("NOTDEFINED")
        assert_eq!(
            *e.args.last().unwrap(),
            StepValue::Enum(SmolStr::new("NOTDEFINED"))
        );
    }

    #[test]
    fn test_parse_real_and_int() {
        let data = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n#2=IFCCARTESIANPOINT((0.,0.,0.));\nENDSEC;\n";
        let entities = parse_entities(data).unwrap();
        assert!(entities.contains_key(&1));
        assert!(entities.contains_key(&2));
    }

    #[test]
    fn test_parse_multiline_entity() {
        let data = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDaTa ;\n#1=IFCPROPERTYSINGLEVALUE(\n  'Name',\n  $,\n  IFCTEXT('Hello'),\n  $);\nENDSEC;\n";
        let entities = parse_entities(data).unwrap();
        let e = &entities[&1];
        assert_eq!(e.entity_name.as_str(), "IFCPROPERTYSINGLEVALUE");
        assert_eq!(e.args.len(), 4);
        match &e.args[2] {
            StepValue::Typed { type_name, value } => {
                assert_eq!(type_name.as_str(), "IFCTEXT");
                assert_eq!(**value, StepValue::String(SmolStr::new("Hello")));
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_string_with_semicolon() {
        let data = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n#1=IFCPROPERTYSINGLEVALUE('Note',$,IFCTEXT('A;B;C'),$);\nENDSEC;\n";
        let entities = parse_entities(data).unwrap();
        let e = &entities[&1];
        match &e.args[2] {
            StepValue::Typed { type_name, value } => {
                assert_eq!(type_name.as_str(), "IFCTEXT");
                assert_eq!(**value, StepValue::String(SmolStr::new("A;B;C")));
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }
}
