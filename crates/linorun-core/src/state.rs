#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    Running,
    ShuttingDown,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FiberState {
    Pending,
    Loading,
    Active,
    Unloading,
    Failed,
    Disposed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EpisodePhase {
    Loading,
    Active,
    WaitingWithdrawal,
    Cleaning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectSnapshotState {
    Applying,
    Armed,
    Disposing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InverseSnapshotState {
    Armed,
    Running,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectForwardOutcome {
    Completed,
    Disposed,
}
