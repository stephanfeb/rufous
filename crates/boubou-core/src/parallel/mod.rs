//! Parallel sub-agent dispatch and the ring-0 child sandbox (Mingbird's `parallel_*.py`).
//!
//! - [`config`]: `ParallelConfig` (the `"parallel"` section of `config.json`, env overrides,
//!   kill switch, depth).
//! - [`sandbox`]: `ChildSandbox`, installed in a child process (`AGENT_CHILD_SANDBOX=1`):
//!   default-deny tool face, path boundary, command blacklist, JSONL audit, exit-53 fuse.
//! - [`probe`]: memory / `/api/ps` readers and the capacity formula.
//! - [`trigger`]: todo provider, the deterministic task classifier and `should_dispatch`.
//! - [`dispatch`]: `ParallelDispatcher` (headless `boubou run` children), the judge, the
//!   `ResultIntegrator`, and `summary_for_parent`.
//! - [`hook`]: `DispatchHook::maybe_dispatch`, called by the loop on a fresh task.
//!
//! Factory defaults are all off: `enabled=false`, no child model, strictest sandbox tier.

pub mod config;
pub mod dispatch;
pub mod hook;
pub mod probe;
pub mod sandbox;
pub mod trigger;

pub use config::{current_depth, env_snapshot, hard_disabled, CommandRule, ParallelConfig};
pub use dispatch::{
    artifact_files, claimed_missing, progress_line, summary_for_parent, ChildStatus, ChildTask,
    DispatchResult, ParallelDispatcher, ProcessSpawner, ResultIntegrator, Spawner,
};
pub use hook::{DispatchHook, HookEvent};
pub use probe::{
    estimate_child_cost, format_capacity_table, CapacityPlan, CapacityPlanner, ResourceProbe,
};
pub use sandbox::{
    audit_summary, read_audit, Access, AuditSummary, ChildSandbox, ChildVerdict, ToolCheck,
    SEVERE_EXIT_CODE,
};
pub use trigger::{
    classify_task, safety_precheck, should_dispatch, DispatchItem, DispatchPlan, JsonTodoProvider,
    TodoProvider,
};
