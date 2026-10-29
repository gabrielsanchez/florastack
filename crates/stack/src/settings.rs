//! The stack's settings as a project stores them (the `stack` preference group): [`StackSettings`], one table per
//! method, so that a preset can set `stack.average.radius` and leave everything else as it was. The depth-map method of
//! roadmap 3.3 adds its own table.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::average::AverageSettings;
use crate::error::StackError;
use crate::pyramid::PyramidSettings;

/// Every stacking method's settings. Every field has a default, so that a preset can name only some.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StackSettings {
    /// The weighted average's: `radius`, `smoothing` and `power`.
    pub average: AverageSettings,
    /// The pyramid method's: `levels`, `grit`, `decisions`, `range_limit`, `tone` and `coarse_limit`.
    pub pyramid: PyramidSettings,
}

impl StackSettings {
    /// Checks every method's settings.
    pub fn validate(&self) -> Result<(), StackError> {
        self.average.validate()?;
        self.pyramid.validate()
    }
}
