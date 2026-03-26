use smol_str::SmolStr;

/// Entity identifier — the `#NNN` line number in the STEP file.
pub type EntityId = u64;

/// A raw (untyped) STEP entity as parsed from the file.
#[derive(Debug, Clone)]
pub struct RawEntity {
    /// The entity's line number (e.g., 123 for `#123`).
    pub id: EntityId,
    /// The IFC type name (e.g., "IFCWALL", "IFCPROPERTYSINGLEVALUE").
    pub entity_name: SmolStr,
    /// The argument list.
    pub args: Vec<StepValue>,
}

/// A value in a STEP entity's argument list.
#[derive(Debug, Clone, PartialEq)]
pub enum StepValue {
    /// Reference to another entity: `#123`
    Ref(EntityId),
    /// String literal: `'some text'`
    String(SmolStr),
    /// Real number: `3.14`
    Real(f64),
    /// Integer: `42`
    Int(i64),
    /// Enumeration: `.NOTDEFINED.`
    Enum(SmolStr),
    /// Boolean: `.T.` or `.F.`
    Bool(bool),
    /// Null/omitted: `$`
    Null,
    /// Derived: `*`
    Derived,
    /// List: `(item1, item2, ...)`
    List(Vec<StepValue>),
    /// Typed value: `IFCLENGTHMEASURE(3.14)`
    Typed {
        type_name: SmolStr,
        value: Box<StepValue>,
    },
}

impl StepValue {
    /// Try to extract a reference ID from this value.
    pub fn as_ref(&self) -> Option<EntityId> {
        match self {
            StepValue::Ref(id) => Some(*id),
            _ => None,
        }
    }

    /// Try to extract a string from this value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            StepValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Try to extract a real number from this value.
    pub fn as_real(&self) -> Option<f64> {
        match self {
            StepValue::Real(v) => Some(*v),
            StepValue::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// Try to extract an integer from this value.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            StepValue::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// Try to extract a boolean from this value.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            StepValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// Try to extract a list from this value.
    pub fn as_list(&self) -> Option<&[StepValue]> {
        match self {
            StepValue::List(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// Try to extract an enum value from this value.
    pub fn as_enum(&self) -> Option<&str> {
        match self {
            StepValue::Enum(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Returns true if this value is null (`$`).
    pub fn is_null(&self) -> bool {
        matches!(self, StepValue::Null)
    }

    /// Unwrap a Typed value, returning the inner value.
    pub fn unwrap_typed(&self) -> &StepValue {
        match self {
            StepValue::Typed { value, .. } => value.as_ref(),
            other => other,
        }
    }
}
