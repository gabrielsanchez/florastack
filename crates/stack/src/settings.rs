//! The stack's settings as a project stores them (the `stack` preference group): [`StackSettings`], one table per
//! method, so that a preset can set `stack.average.radius` and leave everything else as it was. The pyramid and
//! depth-map methods of roadmap 3.2 and 3.3 add their own tables.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::average::AverageSettings;
use crate::error::StackError;

/// Every stacking method's settings. Every field has a default, so that a preset can name only some.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StackSettings {
    /// The weighted average's: `radius`, `smoothing` and `power`.
    pub average: AverageSettings,
}

impl StackSettings {
    /// Checks every method's settings.
    pub fn validate(&self) -> Result<(), StackError> {
        self.average.validate()
    }
}
