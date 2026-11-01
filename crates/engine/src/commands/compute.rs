//! The compute device and memory: which device stacks and how much memory a stack may use (`compute.info`), the
//! preferences that choose them (`compute.set`), and the GPU's self-test run again on demand (`compute.test`). See
//! [`crate::compute`].

use schemars::JsonSchema;
use serde::Deserialize;

use super::check_path;
use crate::compute::{Compute, ComputeInfo, Device};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params};
use crate::session::Session;

/// The largest GPU memory budget, in MiB: far beyond any GPU's memory.
const MAX_BUDGET_MB: u32 = 1 << 20;
/// The smallest memory budget for stacking, in MiB: below it, tiles would spill all the time.
const MIN_MEMORY_MB: u32 = 64;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("compute.info", "Compute device", "Which device stacks, the GPU or the CPU, and why, and how much memory stacking may use.")
                .help(
                    "The compute preference (`auto`, `cpu` or `gpu`), the device it chose (`gpu` or `cpu`) with the GPU's \
                 adapter, or the reason the CPU stacks, and the GPU memory budget of a stack's accumulators. The first \
                 call in a process (or the first stack) probes the GPU: a device on the best adapter, every kernel \
                 compiled, and a self-test against the CPU's results; with `auto` a software adapter isn't taken. \
                 `memory`: the memory a stack's working data may take on the host (beyond it, tiles go to the tile \
                 folder on disk), the memory installed, and the budget suggested for this machine.",
                ),
            info,
        ),
        CommandSpec::instant(
            Meta::new("compute.set", "Set compute device", "Chooses which device stacks, and how much memory stacking may use.")
                .help(
                    "`device`: `auto` (the default: the GPU when a hardware adapter passes the self-test, the CPU \
                     otherwise), `cpu` (always the CPU, whatever the machine has) or `gpu` (software adapters such as \
                     lavapipe and WARP taken too, which are slower than the CPU). `budgetMb`: the GPU memory a stack's \
                     accumulators may take; beyond it, tiles move to the host between frames. `memoryMb`: the memory a \
                     stack's working data may take on the host; beyond it, tiles move to the tile folder on disk, which \
                     is slower. `tileFolder`: that folder (empty for the system's temporary folder). The preferences \
                     apply to stacks started from now on, for this engine; they aren't stored in the project. Returns \
                     what `compute.info` does.",
                )
                .effects(Effects::Project)
                .examples(&[
                    r#"{ "device": "cpu" }"#,
                    r#"{ "device": "auto", "budgetMb": 2048 }"#,
                    r#"{ "memoryMb": 4096, "tileFolder": "D:/Cache/tiles" }"#,
                ]),
            set,
        ),
        CommandSpec::job(
            Meta::new("compute.test", "Test the GPU", "Probes the GPU again and runs its self-test, now.")
                .help(
                    "What `compute.info` does on its first call, again: a device on the best adapter the preference allows, \
                 every kernel compiled, and a self-test against the CPU's results, after a driver update or a GPU \
                 change, say. `device` is the preference to test: the engine's when left out, and `auto` when that is \
                 `cpu`. Returns what `compute.info` would with that preference; the engine's preference stays as it is.",
                )
                .examples(&["{}", r#"{ "device": "gpu" }"#]),
            Lane::Preview,
            test,
        ),
    ]
}

fn info(session: &Session, _: NoParams) -> Result<ComputeInfo, CommandError> {
    Ok(ComputeInfo::of(&session.compute(), &session.tiling()))
}

/// New compute preferences: each field left out stays as it is.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetParams {
    /// `auto`, `cpu` or `gpu`.
    device: Option<Device>,
    /// The GPU memory budget of a stack's accumulators, in MiB, from 64 to 1048576.
    #[schemars(range(min = 64, max = 1_048_576))]
    budget_mb: Option<u32>,
    /// The memory a stack's working data may take on the host, in MiB, from 64 to 1048576.
    #[schemars(range(min = 64, max = 1_048_576))]
    memory_mb: Option<u32>,
    /// The folder tiles beyond the memory budget go to; empty for the system's temporary folder.
    tile_folder: Option<String>,
}

impl Params for SetParams {
    fn check(&self) -> Result<(), Invalid> {
        if let Some(mb) = self.budget_mb
            && !(64..=MAX_BUDGET_MB).contains(&mb)
        {
            return Err(Invalid::new("budgetMb", "must be from 64 to 1048576 MiB"));
        }
        if let Some(mb) = self.memory_mb
            && !(MIN_MEMORY_MB..=MAX_BUDGET_MB).contains(&mb)
        {
            return Err(Invalid::new("memoryMb", "must be from 64 to 1048576 MiB"));
        }
        match &self.tile_folder {
            Some(folder) if !folder.is_empty() => check_path("tileFolder", folder),
            _ => Ok(()),
        }
    }
}

/// MiB as bytes.
fn mib(mb: u32) -> usize {
    usize::try_from(mb).unwrap_or(usize::MAX).saturating_mul(1 << 20)
}

fn set(session: &mut Session, p: SetParams) -> Result<ComputeInfo, CommandError> {
    let current = session.compute();
    let compute = Compute { device: p.device.unwrap_or(current.device), budget: p.budget_mb.map_or(current.budget, mib) };
    let mut tiling = session.tiling();
    if let Some(mb) = p.memory_mb {
        tiling.budget = mib(mb);
    }
    if let Some(folder) = p.tile_folder {
        tiling.spill_dir = if folder.is_empty() {
            None
        } else {
            let folder = super::absolute(&folder)?;
            std::fs::create_dir_all(&folder).map_err(|e| session.unavailable(format!("the tile folder {} can't be made: {e}", folder.display())))?;
            Some(folder)
        };
    }
    session.set_compute(compute);
    session.set_tiling(tiling.clone());
    Ok(ComputeInfo::of(&compute, &tiling))
}

/// The preference to test.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TestParams {
    /// `auto` (hardware adapters) or `gpu` (software ones too); the engine's preference when left out, `auto` when
    /// that is `cpu`.
    device: Option<Device>,
}

impl Params for TestParams {}

fn test(session: &Session, p: TestParams) -> Result<JobPlan<ComputeInfo>, CommandError> {
    let (mut compute, tiling) = (session.compute(), session.tiling());
    compute.device = match p.device.unwrap_or(compute.device) {
        Device::Cpu => Device::Auto,
        device => device,
    };
    Ok(JobPlan::new(move |_ctx| {
        crate::compute::reprobe(compute.device);
        let info = ComputeInfo::of(&compute, &tiling);
        Ok(commit(move |_: &mut Session| Ok(info)))
    })
    .stages(&[("Testing the GPU", 1.0)]))
}
