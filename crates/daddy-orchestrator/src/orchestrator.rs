use crate::models::{JobRequest, PlannedJob};
use crate::planner::Planner;
use crate::scheduler::Scheduler;

pub struct Orchestrator<P, S> {
    planner: P,
    scheduler: S,
}

impl<P, S> Orchestrator<P, S>
where
    P: Planner,
    S: Scheduler,
{
    // Create an orchestrator from one planner and one scheduler implementation.
    pub fn new(planner: P, scheduler: S) -> Self {
        Self { planner, scheduler }
    }

    // Plan a job into a task graph and then compute its execution stages and assignments.
    pub fn plan_job(&self, request: &JobRequest) -> anyhow::Result<PlannedJob> {
        let graph = self.planner.plan(request)?;
        let execution = self.scheduler.schedule(&graph, &request.provider_order)?;
        Ok(PlannedJob { graph, execution })
    }

    // Expose the planner name for CLI inspection output.
    pub fn planner_name(&self) -> &'static str {
        self.planner.name()
    }

    // Expose the scheduler name for CLI inspection output.
    pub fn scheduler_name(&self) -> &'static str {
        self.scheduler.name()
    }
}
