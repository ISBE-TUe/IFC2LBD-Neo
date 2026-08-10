//! Unit resolution.
//!
//! Quantities must be emitted in the units the model declares, not in whatever
//! units its geometry happens to use.
//!
//! Two different scales are in play and IFC does not require them to agree:
//!
//!   * **Geometry** is expressed in the project's `LENGTHUNIT`. A model whose
//!     coordinates are in millimetres yields raw areas in mm² and volumes in mm³.
//!   * **Quantities** are expressed in the project's `AREAUNIT` / `VOLUMEUNIT` /
//!     `LENGTHUNIT`, which are declared independently.
//!
//! Declaring `LENGTHUNIT = MILLI.METRE` alongside `AREAUNIT = SQUARE_METRE` and
//! `VOLUMEUNIT = CUBIC_METRE` is legal, common, and the norm in German and Dutch
//! Revit exports. A backend that computes from raw coordinates and emits the
//! number unconverted is then wrong by 10⁶ on areas and 10⁹ on volumes.
//!
//! Measured across the corpus, 45.1% of everything the QTO module computed was
//! emitted in raw geometry units — three of six scoreable models are millimetre
//! geometry with SI quantity units. Such a value is wrong as written no matter
//! how exact the geometry behind it is, so conversion is not optional.
//!
//! Conversion-based (imperial) units are deliberately **not** resolved: doing so
//! needs the `IfcMeasureWithUnit` factor, which `ifc-model` does not expose. A
//! model using them yields `Err`, and the caller must withhold rather than emit
//! against a guessed scale.

use ifc_model::{IfcModel, Unit};

/// Scale factors from a model's declared units to SI (m, m², m³).
#[derive(Debug, Clone, Copy)]
pub struct UnitScales {
    /// Multiply a `LENGTHUNIT` value by this to get metres.
    pub length: f64,
    /// Multiply an `AREAUNIT` value by this to get m².
    pub area: f64,
    /// Multiply a `VOLUMEUNIT` value by this to get m³.
    pub volume: f64,
}

impl Default for UnitScales {
    fn default() -> Self {
        // Absent declarations, IFC's own default is metres.
        Self {
            length: 1.0,
            area: 1.0,
            volume: 1.0,
        }
    }
}

impl UnitScales {
    /// The factor by which raw geometry (in `LENGTHUNIT`ⁿ) differs from the unit
    /// the quantity is declared in. `1.0` means the two agree.
    ///
    /// A backend that ignores units is wrong by exactly this factor.
    pub fn geometry_to_quantity_factor(&self, dim: Dimension) -> f64 {
        match dim {
            Dimension::Length => 1.0,
            Dimension::Area => self.length.powi(2) / self.area,
            Dimension::Volume => self.length.powi(3) / self.volume,
            Dimension::Other => 1.0,
        }
    }

    /// Convert a raw geometric value (in `LENGTHUNIT`ⁿ) to SI.
    pub fn geometry_to_si(&self, value: f64, dim: Dimension) -> f64 {
        match dim {
            Dimension::Length => value * self.length,
            Dimension::Area => value * self.length.powi(2),
            Dimension::Volume => value * self.length.powi(3),
            Dimension::Other => value,
        }
    }

    /// Convert an authored quantity value (in its declared unit) to SI.
    pub fn quantity_to_si(&self, value: f64, dim: Dimension) -> f64 {
        match dim {
            Dimension::Length => value * self.length,
            Dimension::Area => value * self.area,
            Dimension::Volume => value * self.volume,
            Dimension::Other => value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Length,
    Area,
    Volume,
    /// Counts, weights, ratios — not geometric, not normalised.
    Other,
}

impl Dimension {
    /// Derived from the IFC quantity entity, which is authoritative, rather than
    /// guessed from the quantity's name.
    pub fn from_quantity_entity(entity_name: &str) -> Self {
        match entity_name.to_uppercase().as_str() {
            "IFCQUANTITYLENGTH" => Self::Length,
            "IFCQUANTITYAREA" => Self::Area,
            "IFCQUANTITYVOLUME" => Self::Volume,
            _ => Self::Other,
        }
    }
}

/// SI prefix → multiplier.
fn prefix_scale(prefix: Option<&str>) -> f64 {
    match prefix.map(|p| p.to_uppercase()).as_deref() {
        Some("EXA") => 1e18,
        Some("PETA") => 1e15,
        Some("TERA") => 1e12,
        Some("GIGA") => 1e9,
        Some("MEGA") => 1e6,
        Some("KILO") => 1e3,
        Some("HECTO") => 1e2,
        Some("DECA") => 1e1,
        Some("DECI") => 1e-1,
        Some("CENTI") => 1e-2,
        Some("MILLI") => 1e-3,
        Some("MICRO") => 1e-6,
        Some("NANO") => 1e-9,
        Some("PICO") => 1e-12,
        Some("FEMTO") => 1e-15,
        Some("ATTO") => 1e-18,
        _ => 1.0,
    }
}

/// Read `IfcUnitAssignment` and derive SI scale factors.
pub fn scales_for(model: &IfcModel) -> Result<UnitScales, String> {
    let mut scales = UnitScales::default();
    let mut seen_length = false;

    // Declared value per dimension, so a second, *different* declaration is an
    // error rather than a coin toss.
    //
    // A model merged from several source files carries one `IfcUnitAssignment`
    // per source, and those can disagree — one corpus model declares both
    // millimetres and metres. Iteration order over the assignments is not
    // defined, so whichever was written last used to win, and the whole file's
    // quantities came out either right or 10⁶ too small depending on the run.
    // Under the project's rule a scale that cannot be established uniquely is
    // not a scale, and nothing may be written against it.
    let mut declared: [Option<f64>; 3] = [None; 3];
    let mut conflict: Option<String> = None;
    let mut set = |slot: usize, name: &str, value: f64, conflict: &mut Option<String>| -> bool {
        match declared[slot] {
            Some(prev) if (prev - value).abs() > prev.abs() * 1e-12 => {
                if conflict.is_none() {
                    *conflict = Some(format!(
                        "{name} is declared twice with different scales ({prev} and {value}); \
                         the model's unit assignment is ambiguous"
                    ));
                }
                false
            }
            _ => {
                declared[slot] = Some(value);
                true
            }
        }
    };

    for assignment in model.unit_assignments.values() {
        for unit_id in &assignment.units {
            let Some(unit) = model.units.get(unit_id) else {
                continue;
            };
            match unit {
                Unit::Si {
                    unit_type: Some(unit_type),
                    prefix,
                    name,
                    ..
                } => {
                    let scale = prefix_scale(prefix.as_deref());
                    // An area unit prefixed MILLI means (mm)² = 1e-6 m², i.e. the
                    // prefix applies to the underlying length, squared.
                    match unit_type.to_uppercase().as_str() {
                        "LENGTHUNIT" => {
                            if set(0, "LENGTHUNIT", scale, &mut conflict) {
                                scales.length = scale;
                            }
                            seen_length = true;
                        }
                        "AREAUNIT" => {
                            if set(1, "AREAUNIT", scale.powi(2), &mut conflict) {
                                scales.area = scale.powi(2);
                            }
                        }
                        "VOLUMEUNIT" => {
                            if set(2, "VOLUMEUNIT", scale.powi(3), &mut conflict) {
                                scales.volume = scale.powi(3);
                            }
                        }
                        _ => {}
                    }
                    let _ = name;
                }
                Unit::ConversionBased {
                    unit_type: Some(unit_type),
                    name,
                    ..
                } => {
                    let t = unit_type.to_uppercase();
                    if matches!(t.as_str(), "LENGTHUNIT" | "AREAUNIT" | "VOLUMEUNIT") {
                        return Err(format!(
                            "{t} is a conversion-based unit ({}) which this harness cannot resolve; \
                             scoring would be against an unknown scale",
                            name.as_deref().unwrap_or("unnamed")
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(reason) = conflict {
        return Err(reason);
    }

    if !seen_length {
        // IFC's default is metres. Recorded rather than assumed silently.
        tracing::debug!("no LENGTHUNIT declared — assuming metres per IFC default");
    }
    Ok(scales)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millimetre_geometry_with_si_quantities_yields_the_known_factors() {
        // The real model C case: LENGTHUNIT = mm, AREAUNIT = m², VOLUMEUNIT = m³.
        let s = UnitScales {
            length: 1e-3,
            area: 1.0,
            volume: 1.0,
        };
        assert!((s.geometry_to_quantity_factor(Dimension::Area) - 1e-6).abs() < 1e-15);
        assert!((s.geometry_to_quantity_factor(Dimension::Volume) - 1e-9).abs() < 1e-18);
        // A backend ignoring units emits 1e9x too large a volume.
        assert!((s.geometry_to_si(1e9, Dimension::Volume) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn consistent_metre_model_needs_no_correction() {
        let s = UnitScales::default();
        assert_eq!(s.geometry_to_quantity_factor(Dimension::Volume), 1.0);
        assert_eq!(s.geometry_to_si(2.5, Dimension::Volume), 2.5);
        assert_eq!(s.quantity_to_si(2.5, Dimension::Volume), 2.5);
    }

    #[test]
    fn lengths_are_never_affected_because_both_sides_share_the_unit() {
        let s = UnitScales {
            length: 1e-3,
            area: 1.0,
            volume: 1.0,
        };
        assert_eq!(s.geometry_to_quantity_factor(Dimension::Length), 1.0);
    }

    /// A model merged from several files carries one `IfcUnitAssignment` per
    /// source, and those can disagree. Iteration order over them is undefined,
    /// so whichever came last used to win and the whole file's quantities came
    /// out either right or 10⁶ too small depending on the run. An ambiguous
    /// scale is not a scale.
    #[test]
    fn conflicting_length_units_are_an_error_not_a_coin_toss() {
        let model = model_with_units(&[
            ("#4=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);", ""),
            ("#5=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);", ""),
        ]);
        let err = scales_for(&model).expect_err("must refuse");
        assert!(err.contains("LENGTHUNIT"), "{err}");
        assert!(err.contains("ambiguous"), "{err}");
    }

    /// The same unit declared twice is not a conflict — merged files usually
    /// agree, and refusing those would cost everything for nothing.
    #[test]
    fn the_same_unit_declared_twice_is_fine() {
        let model = model_with_units(&[
            ("#4=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);", ""),
            ("#5=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);", ""),
        ]);
        let s = scales_for(&model).expect("agreeing declarations are fine");
        assert_eq!(s.length, 1e-3);
    }

    /// Two `IfcUnitAssignment`s, as a merged model actually writes them.
    fn model_with_units(units: &[(&str, &str)]) -> ifc_model::IfcModel {
        let mut body = String::from(
            "#1=IFCPROJECT(\'p\',$,\'P\',$,$,$,$,$,#2);\n\
             #2=IFCUNITASSIGNMENT((#4));\n\
             #3=IFCUNITASSIGNMENT((#5));\n",
        );
        for (u, _) in units {
            body.push_str(u);
            body.push('\n');
        }
        let src = format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(($),\'2;1\');\n\
             FILE_NAME($,$,($),($),$,$,$);\nFILE_SCHEMA((\'IFC4\'));\nENDSEC;\n\
             DATA;\n{body}ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let step = ifc_step::parse_step_bytes(src.as_bytes()).expect("parse");
        ifc_model::IfcModel::from_step_file(&step).expect("model")
    }
}
