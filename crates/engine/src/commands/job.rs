//! The jobs: listing and cancelling them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::CommandError;
use crate::jobs::{JobId, JobInfo};
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("job.list", "List jobs", "Lists the jobs queued and running, then the last ones that ended, with their progress and results."),
            list,
        ),
        CommandSpec::instant(
            Meta::new("job.cancel", "Cancel job", "Cancels a job.")
                .help(
                    "A queued job ends at once; a running one stops at its next checkpoint, and its result is \
                     discarded, so the project stays as it was. A job that is writing a file can't be cancelled any \
                     more (`cancelled` is then false).",
                )
                .effects(Effects::Project)
                .examples(&[r#"{ "job": 3 }"#]),
            cancel,
        ),
    ]
}

/// The jobs.
#[derive(Debug, Serialize, JsonSchema)]
struct Jobs {
    jobs: Vec<JobInfo>,
}

fn list(session: &Session, _: NoParams) -> Result<Jobs, CommandError> {
    Ok(Jobs { jobs: session.jobs.list() })
}

/// A job.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct JobParams {
    /// The job's id, as dispatching its command returned it.
    job: JobId,
}

impl Params for JobParams {}

/// Whether the job will stop.
#[derive(Debug, Serialize, JsonSchema)]
struct Cancelled {
    job: JobId,
    /// False when no such job is queued or running, or when it can't be stopped any more.
    cancelled: bool,
}

fn cancel(session: &mut Session, p: JobParams) -> Result<Cancelled, CommandError> {
    Ok(Cancelled { job: p.job, cancelled: session.jobs.cancel(p.job) })
}
