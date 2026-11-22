//! Materials: a name from the table, or the constants themselves.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A material: a name (`steel`, `wood`, ...) or `{young, density, poisson, loss}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum MaterialSpec {
    Name(String),
    Props(MaterialProps),
}

impl Default for MaterialSpec {
    fn default() -> Self {
        MaterialSpec::Name("steel".into())
    }
}

/// The constants the mode tables need.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaterialProps {
    /// Young's modulus, Pa.
    pub young: f64,
    /// Density, kg/m3.
    pub density: f64,
    /// Poisson's ratio, 0..0.5.
    #[serde(default = "default_poisson")]
    pub poisson: f64,
    /// Loss factor: steel about 0.002, wood about 0.02.
    #[serde(default = "default_loss")]
    pub loss: f64,
}

fn default_poisson() -> f64 {
    0.3
}

fn default_loss() -> f64 {
    0.005
}

/// name, Young's modulus (Pa), density (kg/m3), Poisson's ratio, loss factor.
/// The loss factors are round figures for audible decay, not measurements.
const TABLE: &[(&str, f64, f64, f64, f64)] = &[
    ("steel", 2.0e11, 7850.0, 0.29, 0.002),
    ("aluminium", 7.0e10, 2700.0, 0.33, 0.002),
    ("brass", 1.0e11, 8500.0, 0.34, 0.002),
    ("bronze", 1.1e11, 8800.0, 0.34, 0.001),
    ("glass", 7.0e10, 2500.0, 0.22, 0.003),
    ("wood", 1.0e10, 450.0, 0.30, 0.020),
    ("hardwood", 1.6e10, 850.0, 0.30, 0.012),
    ("stone", 5.0e10, 2700.0, 0.25, 0.010),
    ("ice", 9.0e9, 917.0, 0.33, 0.020),
    ("plastic", 3.0e9, 1200.0, 0.35, 0.050),
    ("nylon", 3.0e9, 1150.0, 0.40, 0.020),
    ("mylar", 3.5e9, 1390.0, 0.38, 0.020),
];

/// Every material name, for error messages and catalogs.
pub fn names() -> Vec<&'static str> {
    let mut v: Vec<_> = TABLE.iter().map(|t| t.0).collect();
    v.push("metal");
    v.push("aluminum");
    v
}

impl MaterialSpec {
    pub fn props(&self) -> Result<MaterialProps, String> {
        match self {
            MaterialSpec::Name(n) => {
                let key = match n.to_ascii_lowercase().as_str() {
                    "metal" => "steel".to_string(),
                    "aluminum" => "aluminium".to_string(),
                    other => other.to_string(),
                };
                TABLE
                    .iter()
                    .find(|t| t.0 == key)
                    .map(|&(_, young, density, poisson, loss)| MaterialProps {
                        young,
                        density,
                        poisson,
                        loss,
                    })
                    .ok_or_else(|| {
                        format!(
                            "material: unknown material `{n}`: use {} or {{young, density, poisson, loss}}",
                            names().join(", ")
                        )
                    })
            }
            MaterialSpec::Props(p) => {
                if !(p.young.is_finite() && p.young > 0.0) {
                    return Err(format!("material.young: {}: give Pa > 0", p.young));
                }
                if !(p.density.is_finite() && p.density > 0.0) {
                    return Err(format!("material.density: {}: give kg/m3 > 0", p.density));
                }
                if !(0.0..0.5).contains(&p.poisson) {
                    return Err(format!("material.poisson: {}: give 0..0.5", p.poisson));
                }
                if !(p.loss.is_finite() && p.loss >= 0.0) {
                    return Err(format!("material.loss: {}: give a factor >= 0", p.loss));
                }
                Ok(*p)
            }
        }
    }
}
