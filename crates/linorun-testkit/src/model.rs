use std::collections::{BTreeMap, BTreeSet};

use linorun_core::{BindingId, EffectId, EpisodeId, FiberId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelLifecycle {
    Inactive,
    Reloading {
        episode: EpisodeId,
    },
    Active {
        episode: EpisodeId,
    },
    Unloading {
        episode: EpisodeId,
        outcome: ModelUnloadOutcome,
    },
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelUnloadOutcome {
    Normal,
    ActivationFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelFiber {
    pub id: FiberId,
    pub parent: Option<FiberId>,
    pub children: BTreeSet<FiberId>,
    pub lifecycle: ModelLifecycle,
    pub retired: bool,
    pub running_inverse: Option<(EffectId, u64)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelBinding {
    pub id: BindingId,
    pub provider: FiberId,
    pub consumers: BTreeSet<FiberId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelEffect {
    pub id: EffectId,
    pub fiber: FiberId,
    pub inverses: BTreeSet<u64>,
    pub disposing: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelSnapshot {
    pub fibers: Vec<ModelFiber>,
    pub bindings: Vec<ModelBinding>,
    pub effects: Vec<ModelEffect>,
}

pub enum ModelCommand {
    InsertFiber {
        fiber: FiberId,
        parent: Option<FiberId>,
    },
    Begin {
        fiber: FiberId,
        episode: EpisodeId,
    },
    Finish {
        fiber: FiberId,
    },
    Iter {
        fiber: FiberId,
        episode: EpisodeId,
    },
    Divert {
        fiber: FiberId,
    },
    Raise {
        fiber: FiberId,
    },
    Leave {
        fiber: FiberId,
    },
    FinishUnload {
        fiber: FiberId,
    },
    Retire {
        fiber: FiberId,
    },
    Remove {
        fiber: FiberId,
    },
    InstallBinding(ModelBinding),
    AddConsumer {
        binding: BindingId,
        consumer: FiberId,
    },
    RemoveConsumer {
        binding: BindingId,
        consumer: FiberId,
    },
    RemoveBinding {
        binding: BindingId,
    },
    StartEffect {
        effect: EffectId,
        fiber: FiberId,
    },
    YieldInverse {
        effect: EffectId,
        seq: u64,
    },
    RequestEffectDispose {
        effect: EffectId,
    },
    StartInverse {
        effect: EffectId,
        seq: u64,
    },
    FinishInverse {
        effect: EffectId,
        seq: u64,
    },
    FinishEffect {
        effect: EffectId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelError(pub String);

impl std::fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ModelError {}

#[derive(Default)]
pub struct ReferenceModel {
    fibers: BTreeMap<FiberId, ModelFiber>,
    bindings: BTreeMap<BindingId, ModelBinding>,
    effects: BTreeMap<EffectId, ModelEffect>,
}

impl ReferenceModel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, command: ModelCommand) -> Result<(), ModelError> {
        match command {
            ModelCommand::InsertFiber { fiber, parent } => {
                if self.fibers.contains_key(&fiber) {
                    return Err(ModelError(format!("fiber {fiber:?} already exists")));
                }
                if let Some(parent_id) = parent {
                    self.fiber_mut(parent_id)?.children.insert(fiber);
                }
                self.fibers.insert(
                    fiber,
                    ModelFiber {
                        id: fiber,
                        parent,
                        children: BTreeSet::new(),
                        lifecycle: ModelLifecycle::Inactive,
                        retired: false,
                        running_inverse: None,
                    },
                );
            }
            ModelCommand::Begin { fiber, episode } => {
                let record = self.fiber_mut(fiber)?;
                if record.retired
                    || !matches!(
                        record.lifecycle,
                        ModelLifecycle::Inactive | ModelLifecycle::Failed
                    )
                {
                    return Err(ModelError(format!("fiber {fiber:?} cannot begin")));
                }
                record.lifecycle = ModelLifecycle::Reloading { episode };
            }
            ModelCommand::Finish { fiber } => {
                let record = self.fiber_mut(fiber)?;
                let ModelLifecycle::Reloading { episode } = record.lifecycle else {
                    return Err(ModelError(format!("fiber {fiber:?} is not reloading")));
                };
                record.lifecycle = ModelLifecycle::Active { episode };
            }
            ModelCommand::Iter { fiber, episode } => {
                let record = self.fiber_mut(fiber)?;
                if record.lifecycle != (ModelLifecycle::Reloading { episode }) {
                    return Err(ModelError(format!(
                        "fiber {fiber:?} is not iterating episode {episode:?}"
                    )));
                }
            }
            ModelCommand::Divert { fiber } => {
                let record = self.fiber_mut(fiber)?;
                let ModelLifecycle::Reloading { episode } = record.lifecycle else {
                    return Err(ModelError(format!("fiber {fiber:?} is not reloading")));
                };
                record.lifecycle = ModelLifecycle::Unloading {
                    episode,
                    outcome: ModelUnloadOutcome::Normal,
                };
            }
            ModelCommand::Raise { fiber } => {
                let record = self.fiber_mut(fiber)?;
                let ModelLifecycle::Reloading { episode } = record.lifecycle else {
                    return Err(ModelError(format!("fiber {fiber:?} is not reloading")));
                };
                record.lifecycle = ModelLifecycle::Unloading {
                    episode,
                    outcome: ModelUnloadOutcome::ActivationFailed,
                };
            }
            ModelCommand::Leave { fiber } => {
                let record = self.fiber_mut(fiber)?;
                let ModelLifecycle::Active { episode } = record.lifecycle else {
                    return Err(ModelError(format!("fiber {fiber:?} is not active")));
                };
                record.lifecycle = ModelLifecycle::Unloading {
                    episode,
                    outcome: ModelUnloadOutcome::Normal,
                };
            }
            ModelCommand::FinishUnload { fiber } => {
                let has_binding = self
                    .bindings
                    .values()
                    .any(|binding| binding.provider == fiber || binding.consumers.contains(&fiber));
                let has_effect = self.effects.values().any(|effect| effect.fiber == fiber);
                let record = self.fiber_mut(fiber)?;
                let ModelLifecycle::Unloading { outcome, .. } = record.lifecycle else {
                    return Err(ModelError(format!("fiber {fiber:?} is not unloading")));
                };
                if has_binding || has_effect || record.running_inverse.is_some() {
                    return Err(ModelError(format!(
                        "fiber {fiber:?} cleanup is not finished"
                    )));
                }
                record.lifecycle = match outcome {
                    ModelUnloadOutcome::Normal => ModelLifecycle::Inactive,
                    ModelUnloadOutcome::ActivationFailed => ModelLifecycle::Failed,
                };
            }
            ModelCommand::Retire { fiber } => self.fiber_mut(fiber)?.retired = true,
            ModelCommand::Remove { fiber } => {
                let record = self
                    .fibers
                    .remove(&fiber)
                    .ok_or_else(|| ModelError(format!("fiber {fiber:?} is missing")))?;
                let has_binding = self
                    .bindings
                    .values()
                    .any(|binding| binding.provider == fiber || binding.consumers.contains(&fiber));
                let has_effect = self.effects.values().any(|effect| effect.fiber == fiber);
                if !record.retired
                    || !matches!(
                        record.lifecycle,
                        ModelLifecycle::Inactive | ModelLifecycle::Failed
                    )
                    || !record.children.is_empty()
                    || has_binding
                    || has_effect
                    || record.running_inverse.is_some()
                {
                    self.fibers.insert(fiber, record);
                    return Err(ModelError(format!("fiber {fiber:?} is not removable")));
                }
                if let Some(parent) = record.parent {
                    self.fiber_mut(parent)?.children.remove(&fiber);
                }
            }
            ModelCommand::InstallBinding(binding) => {
                if !self.fibers.contains_key(&binding.provider) {
                    return Err(ModelError("binding provider is missing".to_owned()));
                }
                if binding
                    .consumers
                    .iter()
                    .any(|consumer| !self.fibers.contains_key(consumer))
                {
                    return Err(ModelError("binding consumer is missing".to_owned()));
                }
                if self.bindings.contains_key(&binding.id) {
                    return Err(ModelError("binding id is already installed".to_owned()));
                }
                self.bindings.insert(binding.id, binding);
            }
            ModelCommand::AddConsumer { binding, consumer } => {
                if !self.fibers.contains_key(&consumer) {
                    return Err(ModelError("binding consumer is missing".to_owned()));
                }
                self.bindings
                    .get_mut(&binding)
                    .ok_or_else(|| ModelError(format!("binding {binding:?} is missing")))?
                    .consumers
                    .insert(consumer);
            }
            ModelCommand::RemoveConsumer { binding, consumer } => {
                if !self
                    .bindings
                    .get_mut(&binding)
                    .ok_or_else(|| ModelError(format!("binding {binding:?} is missing")))?
                    .consumers
                    .remove(&consumer)
                {
                    return Err(ModelError(format!(
                        "fiber {consumer:?} does not consume binding {binding:?}"
                    )));
                }
            }
            ModelCommand::RemoveBinding { binding } => {
                self.bindings
                    .remove(&binding)
                    .ok_or_else(|| ModelError(format!("binding {binding:?} is missing")))?;
            }
            ModelCommand::StartEffect { effect, fiber } => {
                if !self.fibers.contains_key(&fiber) {
                    return Err(ModelError("effect owner is missing".to_owned()));
                }
                if self.effects.contains_key(&effect) {
                    return Err(ModelError("effect id already exists".to_owned()));
                }
                self.effects.insert(
                    effect,
                    ModelEffect {
                        id: effect,
                        fiber,
                        inverses: BTreeSet::new(),
                        disposing: false,
                    },
                );
            }
            ModelCommand::YieldInverse { effect, seq } => {
                let fiber = self.effect_mut(effect)?.fiber;
                if self
                    .effects
                    .values()
                    .filter(|candidate| candidate.fiber == fiber)
                    .any(|candidate| candidate.inverses.contains(&seq))
                {
                    return Err(ModelError(format!(
                        "fiber {fiber:?} repeats inverse sequence {seq}"
                    )));
                }
                let record = self.effect_mut(effect)?;
                if !record.inverses.insert(seq) {
                    return Err(ModelError(format!("inverse sequence {seq} is duplicated")));
                }
            }
            ModelCommand::RequestEffectDispose { effect } => {
                self.effect_mut(effect)?.disposing = true;
            }
            ModelCommand::StartInverse { effect, seq } => {
                let record = self.effect_mut(effect)?;
                if !record.disposing || !record.inverses.contains(&seq) {
                    return Err(ModelError(format!(
                        "inverse {seq} of effect {effect:?} is not requested"
                    )));
                }
                let fiber = record.fiber;
                let expected = self
                    .effects
                    .values()
                    .filter(|candidate| candidate.fiber == fiber && candidate.disposing)
                    .flat_map(|candidate| candidate.inverses.iter())
                    .copied()
                    .max();
                if expected != Some(seq) {
                    return Err(ModelError(format!(
                        "inverse {seq} is not the next LIFO sequence {expected:?}"
                    )));
                }
                let owner = self.fiber_mut(fiber)?;
                if owner.running_inverse.is_some() {
                    return Err(ModelError(format!(
                        "fiber {fiber:?} already has a running inverse"
                    )));
                }
                owner.running_inverse = Some((effect, seq));
            }
            ModelCommand::FinishInverse { effect, seq } => {
                let fiber = self.effect_mut(effect)?.fiber;
                if self.fiber_mut(fiber)?.running_inverse != Some((effect, seq)) {
                    return Err(ModelError(format!(
                        "inverse {seq} of effect {effect:?} is not running"
                    )));
                }
                self.fiber_mut(fiber)?.running_inverse = None;
                if !self.effect_mut(effect)?.inverses.remove(&seq) {
                    return Err(ModelError(format!("inverse sequence {seq} is missing")));
                }
            }
            ModelCommand::FinishEffect { effect } => {
                let record = self
                    .effects
                    .remove(&effect)
                    .ok_or_else(|| ModelError(format!("effect {effect:?} is missing")))?;
                if !record.disposing || !record.inverses.is_empty() {
                    self.effects.insert(effect, record);
                    return Err(ModelError(format!("effect {effect:?} is not finished")));
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> ModelSnapshot {
        ModelSnapshot {
            fibers: self.fibers.values().cloned().collect(),
            bindings: self.bindings.values().cloned().collect(),
            effects: self.effects.values().cloned().collect(),
        }
    }

    fn fiber_mut(&mut self, fiber: FiberId) -> Result<&mut ModelFiber, ModelError> {
        self.fibers
            .get_mut(&fiber)
            .ok_or_else(|| ModelError(format!("fiber {fiber:?} is missing")))
    }

    fn effect_mut(&mut self, effect: EffectId) -> Result<&mut ModelEffect, ModelError> {
        self.effects
            .get_mut(&effect)
            .ok_or_else(|| ModelError(format!("effect {effect:?} is missing")))
    }
}
