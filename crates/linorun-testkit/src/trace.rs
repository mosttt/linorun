use std::sync::{Mutex, mpsc};

use linorun_core::{RuntimeEvent, RuntimeEventKind};

pub struct TraceRecorder {
    sender: mpsc::Sender<RuntimeEvent>,
    receiver: Mutex<mpsc::Receiver<RuntimeEvent>>,
}

impl TraceRecorder {
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver: Mutex::new(receiver),
        }
    }

    #[must_use]
    pub fn sender(&self) -> mpsc::Sender<RuntimeEvent> {
        self.sender.clone()
    }

    #[must_use]
    pub fn drain(&self) -> Vec<RuntimeEvent> {
        self.receiver
            .lock()
            .expect("trace receiver mutex poisoned")
            .try_iter()
            .collect()
    }
}

impl Default for TraceRecorder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventTag {
    RuntimeShuttingDown,
    RuntimeStopped,
    FiberInserted,
    FiberRetired,
    FiberRemoved,
    LBegin,
    LIter,
    LFinish,
    LDivert,
    LRaise,
    LLeave,
    LUnloadStarted,
    LUnloadFinished,
    ActivationYielded,
    BindingInstalled,
    BindingVisible,
    BindingRemoved,
    EffectStarted,
    EffectYielded,
    EffectForwardCompleted,
    EffectForwardFailed,
    EffectDisposeRequested,
    InverseStarted,
    InverseFinished,
}

#[must_use]
pub const fn event_tag(kind: &RuntimeEventKind) -> EventTag {
    match kind {
        RuntimeEventKind::RuntimeShuttingDown => EventTag::RuntimeShuttingDown,
        RuntimeEventKind::RuntimeStopped => EventTag::RuntimeStopped,
        RuntimeEventKind::FiberInserted { .. } => EventTag::FiberInserted,
        RuntimeEventKind::FiberRetired { .. } => EventTag::FiberRetired,
        RuntimeEventKind::FiberRemoved { .. } => EventTag::FiberRemoved,
        RuntimeEventKind::LBegin { .. } => EventTag::LBegin,
        RuntimeEventKind::LIter { .. } => EventTag::LIter,
        RuntimeEventKind::LFinish { .. } => EventTag::LFinish,
        RuntimeEventKind::LDivert { .. } => EventTag::LDivert,
        RuntimeEventKind::LRaise { .. } => EventTag::LRaise,
        RuntimeEventKind::LLeave { .. } => EventTag::LLeave,
        RuntimeEventKind::LUnloadStarted { .. } => EventTag::LUnloadStarted,
        RuntimeEventKind::LUnloadFinished { .. } => EventTag::LUnloadFinished,
        RuntimeEventKind::ActivationYielded { .. } => EventTag::ActivationYielded,
        RuntimeEventKind::BindingInstalled { .. } => EventTag::BindingInstalled,
        RuntimeEventKind::BindingVisible { .. } => EventTag::BindingVisible,
        RuntimeEventKind::BindingRemoved { .. } => EventTag::BindingRemoved,
        RuntimeEventKind::EffectStarted { .. } => EventTag::EffectStarted,
        RuntimeEventKind::EffectYielded { .. } => EventTag::EffectYielded,
        RuntimeEventKind::EffectForwardCompleted { .. } => EventTag::EffectForwardCompleted,
        RuntimeEventKind::EffectForwardFailed { .. } => EventTag::EffectForwardFailed,
        RuntimeEventKind::EffectDisposeRequested { .. } => EventTag::EffectDisposeRequested,
        RuntimeEventKind::InverseStarted { .. } => EventTag::InverseStarted,
        RuntimeEventKind::InverseFinished { .. } => EventTag::InverseFinished,
    }
}
