//! Aggregation and printing.
//!
//! Two things are reported separately and must never be merged into one score:
//!
//!   * **Coverage** — did the backend attempt this quantity at all? A miss here
//!     is a gap, not an error, and under the project's "omit rather than
//!     approximate" rule it is the *correct* outcome when exactness is not
//!     reachable.
//!   * **Accuracy** — of the values it did produce, how close are they? This is
//!     the number that decides whether a backend may be trusted.
//!
//! A backend that computes nothing scores 100% accuracy. That is why coverage is
//! always printed alongside it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The backend produced a value, in the model's declared quantity unit.
    ///
    /// `looks_like_unit_error` flags a disagreement that lands on the
    /// geometry-to-quantity conversion factor — i.e. the backend skipped the
    /// conversion. Kept as a regression detector: it must stay at zero.
    Computed {
        value: f64,
        relative_error: f64,
        looks_like_unit_error: bool,
        unit_factor: f64,
    },
    /// The backend produced nothing for this quantity.
    NotComputed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub file: String,
    pub guid: String,
    pub ifc_type: String,
    pub representation: String,
    pub set_name: String,
    pub quantity: String,
    /// Whether this is a standard IFC base quantity (per bSDD) or an
    /// exporter-specific one. Vendor quantities like ArchiCAD's
    /// "Oberkante zu Meereshöhe" are not geometry a backend should compute, and
    /// counting them as coverage misses would make the headline meaningless.
    pub standard: bool,
    pub authored: f64,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Default)]
struct Bucket {
    attempted: usize,
    not_computed: usize,
    errors: Vec<f64>,
}

impl Bucket {
    fn add(&mut self, c: &Comparison) {
        self.attempted += 1;
        match c.outcome {
            Outcome::NotComputed => self.not_computed += 1,
            Outcome::Computed { relative_error, .. } => self.errors.push(relative_error),
        }
    }

    fn computed(&self) -> usize {
        self.errors.len()
    }

    fn percentile(&self, p: f64) -> Option<f64> {
        if self.errors.is_empty() {
            return None;
        }
        let mut v = self.errors.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((v.len() - 1) as f64 * p).round() as usize;
        Some(v[idx])
    }

    fn within(&self, tol: f64) -> usize {
        self.errors.iter().filter(|e| **e <= tol).count()
    }
}

pub struct Report {
    tolerance: f64,
    all: Vec<Comparison>,
}

impl Report {
    pub fn new(tolerance: f64) -> Self {
        Self {
            tolerance,
            all: Vec::new(),
        }
    }

    pub fn add(&mut self, _file: String, comparisons: Vec<Comparison>) {
        self.all.extend(comparisons);
    }

    pub fn print(&self, top: usize) {
        if self.all.is_empty() {
            println!("\nNo comparable quantities found.");
            return;
        }

        let vendor_total = self.all.iter().filter(|c| !c.standard).count();
        let standard: Vec<&Comparison> = self.all.iter().filter(|c| c.standard).collect();

        if vendor_total > 0 {
            println!(
                "\n{} exporter-specific quantities excluded from scoring \
                 (ArchiCAD/Revit custom sets — not geometry any backend should compute).",
                vendor_total
            );
        }

        let total = standard.len();
        if total == 0 {
            println!("\nNo standard IFC quantities found to score.");
            return;
        }
        let computed = standard
            .iter()
            .filter(|c| matches!(c.outcome, Outcome::Computed { .. }))
            .count();
        let matching = standard
            .iter()
            .filter(|c| match c.outcome {
                Outcome::Computed { relative_error, .. } => relative_error <= self.tolerance,
                _ => false,
            })
            .count();

        println!("\n{:=<96}", "");
        println!("QTO VALIDATION — computed vs. authored");
        println!("{:=<96}", "");
        println!(
            "\nauthored quantities compared : {total}\n\
             recomputed by backend        : {computed}  ({:.1}% coverage)\n\
             matching within {:.3}%        : {matching}  ({:.1}% of those computed)\n\
             NOT recomputed               : {}  ({:.1}%)",
            pct(computed, total),
            self.tolerance * 100.0,
            pct(matching, computed.max(1)),
            total - computed,
            pct(total - computed, total),
        );

        // Unit errors and geometry errors have different fixes, so attribute
        // them separately. A value can be geometrically exact and still unusable
        // because the backend never converted it out of raw geometry units.
        let unit_scaled = standard
            .iter()
            .filter(|c| match c.outcome {
                Outcome::Computed { unit_factor, .. } => (unit_factor - 1.0).abs() > 1e-9,
                _ => false,
            })
            .count();
        let unit_errors = standard
            .iter()
            .filter(|c| match c.outcome {
                Outcome::Computed {
                    looks_like_unit_error,
                    ..
                } => looks_like_unit_error,
                _ => false,
            })
            .count();
        println!(
            "\nUNIT HANDLING\n\
             needing conversion (mm/imperial geometry) : {unit_scaled}  ({:.1}% of computed)\n\
             disagreements matching the unit factor    : {unit_errors}{}",
            pct(unit_scaled, computed.max(1)),
            if unit_errors == 0 {
                "  \u{2713} conversion is being applied"
            } else {
                "  \u{26a0} backend is skipping unit conversion"
            },
        );

        self.print_by(
            "BY QUANTITY KIND",
            |c| c.quantity.clone(),
            top,
        );
        self.print_by(
            "BY REPRESENTATION TYPE",
            |c| c.representation.clone(),
            0,
        );
        self.print_by("BY IFC TYPE", |c| c.ifc_type.clone(), 0);

        self.print_outliers(top);
    }

    fn print_by<F>(&self, title: &str, key: F, top: usize)
    where
        F: Fn(&Comparison) -> String,
    {
        let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
        for c in self.all.iter().filter(|c| c.standard) {
            buckets.entry(key(c)).or_default().add(c);
        }

        println!("\n{:-<96}", "");
        println!("{title}");
        println!("{:-<96}", "");
        println!(
            "{:<34} {:>7} {:>9} {:>10} {:>11} {:>10} {:>9}",
            "", "n", "computed", "coverage", "match", "median err", "p95 err"
        );

        let mut rows: Vec<(&String, &Bucket)> = buckets.iter().collect();
        rows.sort_by(|a, b| b.1.attempted.cmp(&a.1.attempted));

        for (name, b) in rows {
            let median = b.percentile(0.5);
            let p95 = b.percentile(0.95);
            println!(
                "{:<34} {:>7} {:>9} {:>9.1}% {:>10.1}% {:>10} {:>9}",
                truncate(name, 34),
                b.attempted,
                b.computed(),
                pct(b.computed(), b.attempted),
                pct(b.within(self.tolerance), b.computed().max(1)),
                fmt_err(median),
                fmt_err(p95),
            );
        }
        let _ = top;
    }

    fn print_outliers(&self, top: usize) {
        if top == 0 {
            return;
        }
        let mut worst: Vec<&Comparison> = self
            .all
            .iter()
            .filter(|c| c.standard && matches!(c.outcome, Outcome::Computed { .. }))
            .collect();
        worst.sort_by(|a, b| {
            err_of(b)
                .partial_cmp(&err_of(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        println!("\n{:-<96}", "");
        println!("WORST {top} DISAGREEMENTS");
        println!("{:-<96}", "");
        for c in worst.iter().take(top) {
            let Outcome::Computed {
                value, relative_error, ..
            } = c.outcome
            else {
                continue;
            };
            println!(
                "{:>9.1}%  {:<22} {:<26} authored {:>14.4}  computed {:>14.4}\n\
                 {:>11}{}  {}",
                relative_error * 100.0,
                truncate(&c.quantity, 22),
                truncate(&c.representation, 26),
                c.authored,
                value,
                "",
                c.ifc_type,
                c.guid,
            );
        }
    }

    pub fn write_json(&self, path: &Path) -> std::io::Result<()> {
        let f = File::create(path)?;
        let mut w = BufWriter::new(f);
        serde_json::to_writer_pretty(&mut w, &self.all)?;
        w.flush()
    }
}

fn err_of(c: &Comparison) -> f64 {
    match c.outcome {
        Outcome::Computed { relative_error, .. } => relative_error,
        Outcome::NotComputed => -1.0,
    }
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 * 100.0 / d as f64
    }
}

fn fmt_err(v: Option<f64>) -> String {
    match v {
        None => "—".to_string(),
        Some(e) if e < 0.00001 => "0".to_string(),
        Some(e) => format!("{:.2}%", e * 100.0),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n - 1])
    }
}
