use std::sync::Arc;

use crate::{
    ActivationFailure, BindingId, CleanupError, CleanupFailure, CleanupOwner, ComponentId,
    EffectError, EffectForwardOutcome, EffectId, EffectSnapshotState, EpisodeId, EpisodePhase,
    FiberId, FiberState, InverseSnapshotState, RealmId, RuntimeState, ServiceId, StepId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub sequence: u64,
    pub runtime_state: RuntimeState,
    pub quiescent: bool,
    pub fibers: Vec<FiberSnapshot>,
    pub bindings: Vec<BindingSnapshot>,
    pub effects: Vec<EffectSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiberSnapshot {
    pub id: FiberId,
    pub parent: Option<FiberId>,
    pub mount_effect: Option<EffectId>,
    pub component: ComponentId,
    pub state: FiberState,
    pub retired: bool,
    pub activation_revision: u64,
    pub latest_key: Option<ActivationKeySnapshot>,
    pub episode: Option<EpisodeSnapshot>,
    pub relied_by: Vec<FiberId>,
    pub children: Vec<FiberId>,
    pub failure: Option<ActivationFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpisodeSnapshot {
    pub id: EpisodeId,
    pub committed_key: ActivationKeySnapshot,
    pub committed_bindings: Vec<BindingId>,
    pub phase: EpisodePhase,
    pub running_step: Option<StepId>,
    pub accumulator: Vec<InverseSnapshot>,
    pub next_cleanup_seq: Option<u64>,
    pub cleanup_failures: Vec<CleanupFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationKeySnapshot {
    pub providers: Vec<FiberId>,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingSnapshot {
    pub id: BindingId,
    pub service: ServiceId,
    pub realm: RealmId,
    pub provider: FiberId,
    pub visible: bool,
    pub committed_consumers: Vec<FiberId>,
    pub owner_effect: EffectId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectSnapshot {
    pub id: EffectId,
    pub fiber: FiberId,
    pub episode: EpisodeId,
    pub label: Arc<str>,
    pub state: EffectSnapshotState,
    pub running_step: Option<StepId>,
    pub forward_error: Option<EffectError>,
    pub inverses: Vec<InverseSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InverseSnapshot {
    pub seq: u64,
    pub owner: CleanupOwner,
    pub label: Option<Arc<str>>,
    pub state: InverseSnapshotState,
    pub waiting_on: Vec<FiberId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEvent {
    pub sequence: u64,
    pub kind: RuntimeEventKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEventKind {
    RuntimeShuttingDown,
    RuntimeStopped,
    FiberInserted {
        fiber: FiberId,
        parent: Option<FiberId>,
        component: ComponentId,
    },
    FiberRetired {
        fiber: FiberId,
    },
    FiberRemoved {
        fiber: FiberId,
    },
    LBegin {
        fiber: FiberId,
        episode: EpisodeId,
        key: ActivationKeySnapshot,
    },
    LIter {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
    },
    LFinish {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
    },
    LDivert {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
        committed: ActivationKeySnapshot,
        latest: Option<ActivationKeySnapshot>,
    },
    LRaise {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
        error: ActivationFailure,
    },
    LLeave {
        fiber: FiberId,
        episode: EpisodeId,
        committed: ActivationKeySnapshot,
        latest: Option<ActivationKeySnapshot>,
        removed_bindings: Vec<BindingId>,
    },
    LUnloadStarted {
        fiber: FiberId,
        episode: EpisodeId,
    },
    LUnloadFinished {
        fiber: FiberId,
        episode: EpisodeId,
    },
    ActivationYielded {
        fiber: FiberId,
        episode: EpisodeId,
        step: StepId,
        seq: u64,
        has_next: bool,
    },
    BindingInstalled {
        binding: BindingEventRef,
    },
    BindingVisible {
        binding: BindingEventRef,
    },
    BindingRemoved {
        binding: BindingEventRef,
    },
    EffectStarted {
        fiber: FiberId,
        episode: EpisodeId,
        effect: EffectId,
        label: Arc<str>,
    },
    EffectYielded {
        effect: EffectId,
        step: StepId,
        seq: u64,
        has_next: bool,
    },
    EffectForwardCompleted {
        effect: EffectId,
        outcome: EffectForwardOutcome,
    },
    EffectForwardFailed {
        effect: EffectId,
        error: EffectError,
    },
    EffectDisposeRequested {
        effect: EffectId,
    },
    InverseStarted {
        fiber: FiberId,
        episode: EpisodeId,
        seq: u64,
        owner: CleanupOwner,
        label: Option<Arc<str>>,
    },
    InverseFinished {
        fiber: FiberId,
        episode: EpisodeId,
        seq: u64,
        owner: CleanupOwner,
        label: Option<Arc<str>>,
        error: Option<CleanupError>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingEventRef {
    pub id: BindingId,
    pub service: ServiceId,
    pub realm: RealmId,
    pub provider: FiberId,
    pub owner_effect: EffectId,
}
