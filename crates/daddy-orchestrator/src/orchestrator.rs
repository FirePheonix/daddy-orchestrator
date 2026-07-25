use crate::context::ContextRouter;
use crate::models::{Job, JobRequest, PlannedJob, Task};
use crate::planner::Planner;
use crate::scheduler::Scheduler;

pub trait WorkspaceManager: Send + Sync {
    fn name(&self) -> &'static str;
    fn prepare(&self, job: &Job, tasks: &[Task]) -> anyhow::Result<()>;
}

pub trait MergeEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn merge(&self, job: &Job) -> anyhow::Result<()>;
}

pub trait MemoryStore: Send + Sync {
    fn name(&self) -> &'static str;
    fn record_planned_job(&self, planned: &PlannedJob) -> anyhow::Result<()>;
}

pub struct Orchestrator<P, S, R> {
    planner: P,
    scheduler: S,
    router: R,
}

impl<P, S, R> Orchestrator<P, S, R>
where
    P: Planner,
    S: Scheduler,
    R: ContextRouter,
{
    // Create an orchestrator from one planner, one scheduler, and one context router.
    pub fn new(planner: P, scheduler: S, router: R) -> Self {
        Self {
            planner,
            scheduler,
            router,
        }
    }

    // Plan a job into a task graph, build task-scoped context bundles, and then compute execution stages and assignments.
    pub fn plan_job(&self, request: &JobRequest) -> anyhow::Result<PlannedJob> {
        let graph = self.planner.plan(request)?;
        let contexts = self.router.route(&graph)?;
        let execution = self.scheduler.schedule(&graph, &request.provider_order)?;
        Ok(PlannedJob {
            graph,
            execution,
            contexts,
        })
    }

    // Expose the planner name for CLI inspection output.
    pub fn planner_name(&self) -> &'static str {
        self.planner.name()
    }

    // Expose the scheduler name for CLI inspection output.
    pub fn scheduler_name(&self) -> &'static str {
        self.scheduler.name()
    }

    // Expose the router name for CLI inspection output.
    pub fn router_name(&self) -> &'static str {
        self.router.name()
    }
}
