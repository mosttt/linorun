use std::sync::Arc;

use crate::topology::{ActivationKey, CommittedView};
use crate::{ActivationFailure, EpisodeId, FiberState, StepId};

pub(crate) struct Episode<P> {
    id: EpisodeId,
    key: ActivationKey,
    committed: Arc<CommittedView>,
    payload: P,
}

impl<P> Episode<P> {
    pub(crate) fn new(id: EpisodeId, committed: Arc<CommittedView>, payload: P) -> Self {
        Self {
            id,
            key: committed.key().clone(),
            committed,
            payload,
        }
    }

    pub(crate) const fn id(&self) -> EpisodeId {
        self.id
    }

    pub(crate) fn key(&self) -> &ActivationKey {
        &self.key
    }

    pub(crate) fn committed(&self) -> &Arc<CommittedView> {
        &self.committed
    }

    pub(crate) fn payload(&self) -> &P {
        &self.payload
    }

    pub(crate) fn payload_mut(&mut self) -> &mut P {
        &mut self.payload
    }
}

pub(crate) enum UnloadOutcome {
    Normal,
    ActivationFailed {
        failure: ActivationFailure,
        revision: u64,
    },
}

pub(crate) enum LifecycleState<P> {
    Inactive {
        failure: Option<ActivationFailure>,
    },
    Reloading {
        episode: Episode<P>,
        step: StepId,
    },
    Active {
        episode: Episode<P>,
    },
    Unloading {
        episode: Episode<P>,
        outcome: UnloadOutcome,
    },
}

pub(crate) struct Lifecycle<P> {
    retired: bool,
    revision: u64,
    state: LifecycleState<P>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReconcileAction {
    Begin,
    Leave,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SuccessBoundary {
    Continue { next_step: StepId },
    Final,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Landing {
    Iter,
    Finish,
    Divert,
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RaiseLanding {
    Raise,
    Stale,
}

impl<P> Lifecycle<P> {
    pub(crate) fn new() -> Self {
        Self {
            retired: false,
            revision: 0,
            state: LifecycleState::Inactive { failure: None },
        }
    }

    pub(crate) fn active(episode: Episode<P>) -> Self {
        Self {
            retired: false,
            revision: 0,
            state: LifecycleState::Active { episode },
        }
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn state(&self) -> &LifecycleState<P> {
        &self.state
    }

    pub(crate) const fn is_retired(&self) -> bool {
        self.retired
    }

    pub(crate) fn is_active(&self) -> bool {
        matches!(self.state, LifecycleState::Active { .. })
    }

    pub(crate) fn matches_reloading(&self, episode: EpisodeId, step: StepId) -> bool {
        matches!(
            &self.state,
            LifecycleState::Reloading {
                episode: current,
                step: current_step,
            } if current.id() == episode && *current_step == step
        )
    }

    pub(crate) fn reloading_episode_mut(&mut self) -> Option<&mut Episode<P>> {
        match &mut self.state {
            LifecycleState::Reloading { episode, .. } => Some(episode),
            _ => None,
        }
    }

    pub(crate) fn installed_episode(&self) -> Option<&Episode<P>> {
        match &self.state {
            LifecycleState::Reloading { episode, .. }
            | LifecycleState::Active { episode }
            | LifecycleState::Unloading { episode, .. } => Some(episode),
            LifecycleState::Inactive { .. } => None,
        }
    }

    pub(crate) fn installed_episode_mut(&mut self) -> Option<&mut Episode<P>> {
        match &mut self.state {
            LifecycleState::Reloading { episode, .. }
            | LifecycleState::Active { episode }
            | LifecycleState::Unloading { episode, .. } => Some(episode),
            LifecycleState::Inactive { .. } => None,
        }
    }

    pub(crate) fn accepts_scope(&self, episode: EpisodeId) -> bool {
        matches!(
            &self.state,
            LifecycleState::Reloading { episode: current, .. }
                | LifecycleState::Active { episode: current }
                if current.id() == episode && !self.retired
        )
    }

    pub(crate) fn is_unloading(&self) -> bool {
        matches!(self.state, LifecycleState::Unloading { .. })
    }

    pub(crate) fn failure(&self) -> Option<&ActivationFailure> {
        match &self.state {
            LifecycleState::Inactive {
                failure: Some(failure),
            } => Some(failure),
            LifecycleState::Unloading {
                outcome: UnloadOutcome::ActivationFailed { failure, revision },
                ..
            } if *revision == self.revision => Some(failure),
            _ => None,
        }
    }

    pub(crate) fn is_inactive(&self) -> bool {
        matches!(self.state, LifecycleState::Inactive { .. })
    }

    pub(crate) fn begin(&mut self, episode: Episode<P>, step: StepId) -> Result<(), Episode<P>> {
        match self.state {
            LifecycleState::Inactive { failure: None } if !self.retired => {
                self.state = LifecycleState::Reloading { episode, step };
                Ok(())
            }
            _ => Err(episode),
        }
    }

    pub(crate) fn reconcile(&mut self, latest: Option<&ActivationKey>) -> Option<ReconcileAction> {
        match &self.state {
            LifecycleState::Inactive { failure: None } if !self.retired && latest.is_some() => {
                Some(ReconcileAction::Begin)
            }
            LifecycleState::Active { episode } if latest != Some(episode.key()) => {
                let state =
                    std::mem::replace(&mut self.state, LifecycleState::Inactive { failure: None });
                let LifecycleState::Active { episode } = state else {
                    unreachable!("state matched above")
                };
                self.state = LifecycleState::Unloading {
                    episode,
                    outcome: UnloadOutcome::Normal,
                };
                Some(ReconcileAction::Leave)
            }
            _ => None,
        }
    }

    pub(crate) fn land_success(
        &mut self,
        completed_step: StepId,
        latest: Option<&ActivationKey>,
        boundary: SuccessBoundary,
    ) -> Landing {
        let state = std::mem::replace(&mut self.state, LifecycleState::Inactive { failure: None });
        let LifecycleState::Reloading { episode, step } = state else {
            self.state = state;
            return Landing::Stale;
        };
        if step != completed_step {
            self.state = LifecycleState::Reloading { episode, step };
            return Landing::Stale;
        }

        if latest != Some(episode.key()) {
            self.state = LifecycleState::Unloading {
                episode,
                outcome: UnloadOutcome::Normal,
            };
            return Landing::Divert;
        }

        match boundary {
            SuccessBoundary::Continue { next_step } => {
                self.state = LifecycleState::Reloading {
                    episode,
                    step: next_step,
                };
                Landing::Iter
            }
            SuccessBoundary::Final => {
                self.state = LifecycleState::Active { episode };
                Landing::Finish
            }
        }
    }

    pub(crate) fn land_failure(
        &mut self,
        completed_step: StepId,
        failure: ActivationFailure,
    ) -> RaiseLanding {
        let state = std::mem::replace(&mut self.state, LifecycleState::Inactive { failure: None });
        let LifecycleState::Reloading { episode, step } = state else {
            self.state = state;
            return RaiseLanding::Stale;
        };
        if step != completed_step {
            self.state = LifecycleState::Reloading { episode, step };
            return RaiseLanding::Stale;
        }

        let revision = episode.key().revision();
        self.state = LifecycleState::Unloading {
            episode,
            outcome: UnloadOutcome::ActivationFailed { failure, revision },
        };
        RaiseLanding::Raise
    }

    pub(crate) fn finish_unload(&mut self) -> Option<Episode<P>> {
        let state = std::mem::replace(&mut self.state, LifecycleState::Inactive { failure: None });
        let LifecycleState::Unloading { episode, outcome } = state else {
            self.state = state;
            return None;
        };

        let failure = match outcome {
            UnloadOutcome::Normal => None,
            UnloadOutcome::ActivationFailed { failure, revision } if revision == self.revision => {
                Some(failure)
            }
            UnloadOutcome::ActivationFailed { .. } => None,
        };
        self.state = LifecycleState::Inactive { failure };
        Some(episode)
    }

    pub(crate) fn restart(&mut self) {
        self.revision += 1;
        if let LifecycleState::Inactive { failure } = &mut self.state {
            *failure = None;
        }
    }

    pub(crate) fn retire(&mut self) {
        self.retired = true;
    }

    pub(crate) fn project(&self, removed: bool) -> FiberState {
        if removed {
            return FiberState::Disposed;
        }
        match &self.state {
            LifecycleState::Inactive { .. } if self.retired => FiberState::Unloading,
            LifecycleState::Inactive { failure: None } => FiberState::Pending,
            LifecycleState::Inactive { failure: Some(_) } => FiberState::Failed,
            LifecycleState::Reloading { .. } => FiberState::Loading,
            LifecycleState::Active { .. } => FiberState::Active,
            LifecycleState::Unloading { .. } => FiberState::Unloading,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::FiberId;
    use crate::topology::Target;

    use super::*;

    fn key(provider: u64, revision: u64) -> ActivationKey {
        ActivationKey::new(Target::new(vec![FiberId::new(provider)]), revision)
    }

    fn episode(id: u64, key: ActivationKey) -> Episode<()> {
        Episode::new(
            EpisodeId::new(id),
            Arc::new(CommittedView::new(key, Vec::new())),
            (),
        )
    }

    #[test]
    fn begin_iter_finish_is_one_episode() {
        let key = key(1, 0);
        let mut lifecycle = Lifecycle::new();
        lifecycle
            .begin(episode(1, key.clone()), StepId::new(1))
            .ok()
            .unwrap();

        assert_eq!(
            lifecycle.land_success(
                StepId::new(1),
                Some(&key),
                SuccessBoundary::Continue {
                    next_step: StepId::new(2),
                },
            ),
            Landing::Iter
        );
        assert_eq!(
            lifecycle.land_success(StepId::new(2), Some(&key), SuccessBoundary::Final),
            Landing::Finish
        );
        assert_eq!(lifecycle.project(false), FiberState::Active);

        let LifecycleState::Active { episode } = lifecycle.state() else {
            panic!("fiber must be active")
        };
        assert_eq!(episode.id(), EpisodeId::new(1));
    }

    #[test]
    fn changed_target_diverts_without_becoming_active() {
        let committed = key(1, 0);
        let latest = key(2, 0);
        let mut lifecycle = Lifecycle::new();
        lifecycle
            .begin(episode(1, committed), StepId::new(1))
            .ok()
            .unwrap();

        assert_eq!(
            lifecycle.land_success(StepId::new(1), Some(&latest), SuccessBoundary::Final),
            Landing::Divert
        );
        assert_eq!(lifecycle.project(false), FiberState::Unloading);
    }

    #[test]
    fn recovered_same_target_finishes_current_episode() {
        let committed = key(1, 0);
        let recovered = key(1, 0);
        let mut lifecycle = Lifecycle::new();
        lifecycle
            .begin(episode(1, committed), StepId::new(1))
            .ok()
            .unwrap();

        assert_eq!(
            lifecycle.land_success(StepId::new(1), Some(&recovered), SuccessBoundary::Final),
            Landing::Finish
        );
        assert_eq!(lifecycle.project(false), FiberState::Active);
    }

    #[test]
    fn failure_is_committed_only_after_unload() {
        let committed = key(1, 0);
        let mut lifecycle = Lifecycle::new();
        lifecycle
            .begin(episode(1, committed), StepId::new(1))
            .ok()
            .unwrap();

        assert_eq!(
            lifecycle.land_failure(StepId::new(1), ActivationFailure::user("activation failed"),),
            RaiseLanding::Raise
        );
        assert_eq!(lifecycle.project(false), FiberState::Unloading);

        lifecycle.finish_unload().unwrap();
        assert_eq!(lifecycle.project(false), FiberState::Failed);
    }

    #[test]
    fn restart_invalidates_failure_while_unloading() {
        let committed = key(1, 0);
        let mut lifecycle = Lifecycle::new();
        lifecycle
            .begin(episode(1, committed), StepId::new(1))
            .ok()
            .unwrap();
        lifecycle.land_failure(StepId::new(1), ActivationFailure::user("obsolete failure"));

        lifecycle.restart();
        lifecycle.finish_unload().unwrap();

        assert_eq!(lifecycle.revision(), 1);
        assert_eq!(lifecycle.project(false), FiberState::Pending);
    }

    #[test]
    fn active_key_change_enters_leave() {
        let committed = key(1, 0);
        let latest = key(2, 0);
        let mut lifecycle = Lifecycle::new();
        lifecycle
            .begin(episode(1, committed.clone()), StepId::new(1))
            .ok()
            .unwrap();
        lifecycle.land_success(StepId::new(1), Some(&committed), SuccessBoundary::Final);

        assert_eq!(
            lifecycle.reconcile(Some(&latest)),
            Some(ReconcileAction::Leave)
        );
        assert_eq!(lifecycle.project(false), FiberState::Unloading);
    }

    #[test]
    fn stale_completion_does_not_change_running_step() {
        let committed = key(1, 0);
        let mut lifecycle = Lifecycle::new();
        lifecycle
            .begin(episode(1, committed.clone()), StepId::new(2))
            .ok()
            .unwrap();

        assert_eq!(
            lifecycle.land_success(StepId::new(1), Some(&committed), SuccessBoundary::Final),
            Landing::Stale
        );
        assert_eq!(lifecycle.project(false), FiberState::Loading);
    }

    #[test]
    fn retired_inactive_projects_unloading_until_removed() {
        let mut lifecycle = Lifecycle::<()>::new();
        lifecycle.retire();

        assert_eq!(lifecycle.project(false), FiberState::Unloading);
        assert_eq!(lifecycle.project(true), FiberState::Disposed);
    }
}
