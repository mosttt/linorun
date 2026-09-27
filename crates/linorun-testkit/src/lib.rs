#![forbid(unsafe_code)]

//! Deterministic fixtures for Linorun conformance tests.

mod control;
mod executor;
mod model;
mod snapshot;
mod trace;

pub use control::{Control, Controlled, StepSender, StepStream, controlled, step_stream};
pub use executor::{DeterministicExecutor, DriveError};
pub use model::{
    ModelBinding, ModelCommand, ModelEffect, ModelError, ModelFiber, ModelLifecycle, ModelSnapshot,
    ModelUnloadOutcome, ReferenceModel,
};
pub use snapshot::{
    NormalizedBinding, NormalizedEffect, NormalizedFiber, NormalizedSnapshot, SnapshotViolation,
    check_snapshot, normalize_snapshot,
};
pub use trace::{EventTag, TraceRecorder, event_tag};

use linorun_core::{Runtime, RuntimeSnapshot};

/// A runtime, deterministic executor, and event recorder wired together.
pub struct Scenario {
    executor: DeterministicExecutor,
    runtime: Runtime,
    trace: TraceRecorder,
}

impl Scenario {
    #[must_use]
    pub fn new() -> Self {
        let executor = DeterministicExecutor::new();
        let trace = TraceRecorder::new();
        let runtime = Runtime::builder(executor.clone())
            .event_sender(trace.sender())
            .build();
        Self {
            executor,
            runtime,
            trace,
        }
    }

    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    #[must_use]
    pub fn executor(&self) -> &DeterministicExecutor {
        &self.executor
    }

    #[must_use]
    pub fn trace(&self) -> &TraceRecorder {
        &self.trace
    }

    /// Drives both the supplied future and all runtime tasks until it completes.
    pub fn run<F>(&self, future: F) -> Result<F::Output, DriveError>
    where
        F: std::future::Future,
    {
        self.executor.drive(future)
    }

    /// Obtains an actor-consistent snapshot and validates its structural invariants.
    pub fn snapshot(&self) -> Result<RuntimeSnapshot, SnapshotViolation> {
        let snapshot = self
            .run(self.runtime.snapshot())
            .expect("runtime snapshot must have runnable actor work");
        check_snapshot(&snapshot)?;
        Ok(snapshot)
    }
}

impl Default for Scenario {
    fn default() -> Self {
        Self::new()
    }
}
