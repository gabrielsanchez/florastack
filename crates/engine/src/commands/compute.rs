//! The compute device: which one stacks (`compute.info`) and the preference that chooses it (`compute.set`). See
//! [`crate::compute`].

use schemars::JsonSchema;
use serde::Deserialize;

use crate::compute::{Compute, ComputeInfo, Device};
use crate::error::{CommandError, Invalid};
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params};
use crate::session::Session;

/// The largest GPU memory budget, in MiB: far beyond any GPU's memory.
const MAX_BUDGET_MB: u32 = 1 << 20;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("compute.info", "Compute device", "Which device stacks, the GPU or the CPU, and why.").help(
                "The compute preference (`auto`, `cpu` or `gpu`), the device it chose (`gpu` or `cpu`) with the GPU's \
                 adapter, or the reason the CPU stacks, and the GPU memory budget of a stack's accumulators. The first \
                 call in a process (or the first stack) probes the GPU: a device on the best adapter, every kernel \
                 compiled, and a self-test against the CPU's results; with `auto` a software adapter isn't taken.",
            ),
            info,
        ),
        CommandSpec::instant(
            Meta::new("compute.set", "Set compute device", "Chooses which device stacks: automatic, the CPU, or the GPU.")
                .help(
                    "`device`: `auto` (the default: the GPU when a hardware adapter passes the self-test, the CPU \
                     otherwise), `cpu` (always the CPU, whatever the machine has) or `gpu` (software adapters such as \
                     lavapipe and WARP taken too, which are slower than the CPU). `budgetMb`: the GPU memory a stack's \
                     accumulators may take; beyond it, tiles move to the host between frames. The preference applies to \
                     stacks started from now on, for this engine; it isn't stored in the project. Returns what \
                     `compute.info` does.",
                )
                .effects(Effects::Project)
                .examples(&[r#"{ "device": "cpu" }"#, r#"{ "device": "auto", "budgetMb": 2048 }"#]),
            set,
        ),
    ]
}

fn info(session: &Session, _: NoParams) -> Result<ComputeInfo, CommandError> {
    Ok(ComputeInfo::of(&session.compute()))
}

/// A new compute preference: each field left out stays as it is.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetParams {
    /// `auto`, `cpu` or `gpu`.
    device: Option<Device>,
    /// The GPU memory budget of a stack's accumulators, in MiB, from 64 to 1048576.
    #[schemars(range(min = 64, max = 1_048_576))]
    budget_mb: Option<u32>,
}

impl Params for SetParams {
    fn check(&self) -> Result<(), Invalid> {
        if let Some(mb) = self.budget_mb
            && !(64..=MAX_BUDGET_MB).contains(&mb)
        {
            return Err(Invalid::new("budgetMb", "must be from 64 to 1048576 MiB"));
        }
        Ok(())
    }
}

fn set(session: &mut Session, p: SetParams) -> Result<ComputeInfo, CommandError> {
    let current = session.compute();
    let budget = p.budget_mb.map_or(current.budget, |mb| usize::try_from(mb).unwrap_or(usize::MAX).saturating_mul(1 << 20));
    let compute = Compute { device: p.device.unwrap_or(current.device), budget };
    session.set_compute(compute);
    Ok(ComputeInfo::of(&compute))
}
