mod api;
mod completion;
mod task;

pub use api::{
    EffectHandle, FiberHandle, RootContext, Runtime, RuntimeBuilder, RuntimeTask, TaskSpawner,
};
pub(crate) use api::{ErasedEffectDriver, OperationOwner, RuntimeHandle};

use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, LazyLock, Weak},
    task::{Context as TaskContext, Poll},
};

use futures_channel::{mpsc, oneshot};
use futures_util::StreamExt;

use api::RuntimeInner;
use completion::{EffectCompletion, FiberCompletion};
use task::{activation_task, effect_task, inverse_task};

use crate::component::{DynComponent, ErasedConfig};
use crate::lifecycle::{
    Episode, Landing, Lifecycle, LifecycleState, RaiseLanding, ReconcileAction, SuccessBoundary,
};
use crate::route::ContextRoute;
use crate::service::{ContractVTable, ErasedEndpoint};
use crate::topology::{
    ActivationKey, BindingRecord, BindingSlot, BindingTable, CommittedView, ContractRegistry,
    Target, resolve_dependencies,
};
use crate::{
    Activation, ActivationFailure, ActivationFuture, ActivationKeySnapshot, ActivationStream,
    BindingEventRef, BindingSnapshot, CleanupError, CleanupFailure, CleanupOwner, ComponentContext,
    ComponentDescriptor, Context, DisposeReport, DriverEvent, EffectDisposeReport, EffectError,
    EffectForwardOutcome, EffectId, EffectSnapshot, EffectSnapshotState, EffectStartError,
    EpisodeId, EpisodePhase, FiberError, FiberId, FiberSnapshot, FiberState, InverseFn,
    InverseSnapshot, InverseSnapshotState, MountError, ProvideError, RealmId, RuntimeEvent,
    RuntimeEventKind, RuntimeSnapshot, RuntimeState, ServiceId, ShutdownReport, StepId,
    UpdateError,
};
use crate::{IntoEffectDriver, Scope};

const ROOT_FIBER: FiberId = FiberId::new(0);
const ROOT_EPISODE: EpisodeId = EpisodeId::new(0);

struct MountedFiber {
    id: FiberId,
    completion: Arc<FiberCompletion>,
}

struct StartedEffect {
    id: EffectId,
    completion: Arc<EffectCompletion>,
}

enum Command {
    Mount {
        owner: OperationOwner,
        descriptor: &'static ComponentDescriptor,
        implementation: Arc<dyn DynComponent>,
        config: ErasedConfig,
        config_type: TypeId,
        route: Arc<ContextRoute>,
        context_valid: bool,
        route_validation: Result<(), crate::ServiceTypeMismatch>,
        reply: oneshot::Sender<Result<MountedFiber, MountError>>,
    },
    StartEffect {
        owner: OperationOwner,
        label: Arc<str>,
        driver: ErasedEffectDriver,
        reply: oneshot::Sender<Result<StartedEffect, EffectStartError>>,
    },
    RegisterCleanup {
        owner: OperationOwner,
        label: Arc<str>,
        inverse: InverseFn,
        reply: oneshot::Sender<Result<StartedEffect, EffectStartError>>,
    },
    Provide {
        owner: OperationOwner,
        route: Arc<ContextRoute>,
        service: ServiceId,
        contract: ContractVTable,
        endpoint: Arc<dyn ErasedEndpoint>,
        host: bool,
        context_valid: bool,
        reply: oneshot::Sender<Result<StartedEffect, ProvideError>>,
    },
    ActivationFinished {
        fiber: FiberId,
        episode: EpisodeId,
        step: StepId,
        completion: ActivationCompletion,
    },
    EffectStepFinished {
        fiber: FiberId,
        episode: EpisodeId,
        effect: EffectId,
        step: StepId,
        completion: EffectStepCompletion,
    },
    InverseFinished {
        fiber: FiberId,
        episode: EpisodeId,
        seq: u64,
        result: Result<(), CleanupError>,
    },
    FiberState {
        fiber: FiberId,
        reply: oneshot::Sender<FiberState>,
    },
    WaitSettled {
        fiber: FiberId,
        reply: oneshot::Sender<FiberState>,
    },
    WaitEffectForward {
        effect: EffectId,
        completion: Arc<EffectCompletion>,
        reply: oneshot::Sender<Result<EffectForwardOutcome, EffectError>>,
    },
    DisposeEffect {
        effect: EffectId,
        completion: Arc<EffectCompletion>,
        reply: oneshot::Sender<EffectDisposeReport>,
    },
    Restart {
        fiber: FiberId,
        reply: oneshot::Sender<Result<(), FiberError>>,
    },
    UpdateConfig {
        fiber: FiberId,
        config: ErasedConfig,
        config_type: TypeId,
        reply: oneshot::Sender<Result<(), UpdateError>>,
    },
    Replace {
        fiber: FiberId,
        descriptor: &'static ComponentDescriptor,
        config_type: TypeId,
        implementation: Arc<dyn DynComponent>,
        reply: oneshot::Sender<Result<(), UpdateError>>,
    },
    Dispose {
        fiber: FiberId,
        reply: oneshot::Sender<DisposeReport>,
    },
    Snapshot {
        reply: oneshot::Sender<RuntimeSnapshot>,
    },
    WaitQuiescent {
        reply: oneshot::Sender<RuntimeSnapshot>,
    },
    Shutdown {
        reply: oneshot::Sender<ShutdownReport>,
    },
}

struct ActorCore {
    runtime: Weak<RuntimeInner>,
    spawner: Arc<dyn TaskSpawner>,
    runtime_state: RuntimeState,
    default_realm: RealmId,
    contracts: ContractRegistry,
    bindings: BindingTable,
    watchers: BTreeMap<BindingSlot, BTreeSet<FiberId>>,
    consumers_by_provider: BTreeMap<FiberId, BTreeSet<FiberId>>,
    fibers: BTreeMap<FiberId, FiberRecord>,
    effect_owners: BTreeMap<EffectId, FiberId>,
    next_fiber: u64,
    next_episode: u64,
    next_step: u64,
    next_effect: u64,
    next_binding: u64,
    shutdown_waiters: Vec<oneshot::Sender<ShutdownReport>>,
    shutdown_errors: Vec<CleanupFailure>,
    stopped_report: Option<ShutdownReport>,
    event_sender: Option<std::sync::mpsc::Sender<RuntimeEvent>>,
    next_event_sequence: u64,
    next_snapshot_sequence: u64,
    quiescent_waiters: Vec<oneshot::Sender<RuntimeSnapshot>>,
}

struct FiberRecord {
    parent: Option<FiberId>,
    mount_effect: Option<EffectId>,
    children: BTreeSet<FiberId>,
    descriptor: &'static ComponentDescriptor,
    implementation: Arc<dyn DynComponent>,
    config: ErasedConfig,
    route: Arc<ContextRoute>,
    lifecycle: Lifecycle<EpisodePayload>,
    effects: BTreeMap<EffectId, EffectRecord>,
    next_inverse_seq: u64,
    cleanup_running: Option<RunningInverse>,
    unload_started: bool,
    cleanup_errors: Vec<CleanupFailure>,
    episode_cleanup_errors: Vec<CleanupFailure>,
    completion: Arc<FiberCompletion>,
    settled_waiters: Vec<oneshot::Sender<FiberState>>,
    dispose_waiters: Vec<oneshot::Sender<DisposeReport>>,
}

struct EpisodePayload {
    _implementation: Arc<dyn DynComponent>,
    _config: ErasedConfig,
    inverses: Vec<InverseEntry>,
}

struct InverseEntry {
    seq: u64,
    owner: InverseOwner,
    label: Option<Arc<str>>,
    action: InverseAction,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum InverseOwner {
    Activation,
    Effect(EffectId),
}

enum InverseAction {
    User(InverseFn),
    RetireFiber(FiberId),
    WithdrawBinding {
        binding: crate::BindingId,
        slot: BindingSlot,
        endpoint: Arc<dyn ErasedEndpoint>,
    },
}

#[derive(Clone)]
struct RunningMeta {
    episode: EpisodeId,
    seq: u64,
    owner: InverseOwner,
    label: Option<Arc<str>>,
}

enum RunningInverse {
    User(RunningMeta),
    Withdraw {
        meta: RunningMeta,
        endpoint: Arc<dyn ErasedEndpoint>,
        consumers: Vec<(FiberId, EpisodeId)>,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum EffectState {
    Applying,
    Armed,
    Disposing,
}

struct EffectRecord {
    id: EffectId,
    episode: EpisodeId,
    label: Arc<str>,
    state: EffectState,
    running_step: Option<StepId>,
    forward_error: Option<EffectError>,
    cleanup_errors: Vec<CleanupFailure>,
    completion: Arc<EffectCompletion>,
    forward_waiters: Vec<oneshot::Sender<Result<EffectForwardOutcome, EffectError>>>,
    dispose_waiters: Vec<oneshot::Sender<EffectDisposeReport>>,
}

enum ActivationDriver {
    Init {
        implementation: Arc<dyn DynComponent>,
        context: ComponentContext,
        config: ErasedConfig,
    },
    Once(ActivationFuture),
    Steps(ActivationStream),
    Exhausted,
}

impl ActivationDriver {
    fn poll_step(
        &mut self,
        context: &mut TaskContext<'_>,
    ) -> Poll<Result<DriverEvent, ActivationFailure>> {
        loop {
            match self {
                Self::Init { .. } => {
                    let state = std::mem::replace(self, Self::Exhausted);
                    let Self::Init {
                        implementation,
                        context,
                        config,
                    } = state
                    else {
                        unreachable!("activation driver state was matched above")
                    };
                    *self = match implementation.activate(context, config)? {
                        Activation::Once(future) => Self::Once(future),
                        Activation::Steps(stream) => Self::Steps(stream),
                    };
                }
                Self::Once(future) => {
                    let Poll::Ready(result) = future.as_mut().poll(context) else {
                        return Poll::Pending;
                    };
                    *self = Self::Exhausted;
                    return Poll::Ready(result.map(|inverse| match inverse {
                        Some(inverse) => DriverEvent::Yield {
                            inverse,
                            has_next: false,
                        },
                        None => DriverEvent::Done,
                    }));
                }
                Self::Steps(stream) => match stream.as_mut().poll_next(context) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Some(Ok(inverse))) => {
                        return Poll::Ready(Ok(DriverEvent::Yield {
                            inverse,
                            has_next: true,
                        }));
                    }
                    Poll::Ready(Some(Err(error))) => {
                        *self = Self::Exhausted;
                        return Poll::Ready(Err(error));
                    }
                    Poll::Ready(None) => {
                        *self = Self::Exhausted;
                        return Poll::Ready(Ok(DriverEvent::Done));
                    }
                },
                Self::Exhausted => return Poll::Ready(Ok(DriverEvent::Done)),
            }
        }
    }
}

struct ActivationCompletion {
    result: Result<DriverEvent, ActivationFailure>,
    driver: ActivationDriver,
}

struct EffectStepCompletion {
    result: Result<DriverEvent, EffectError>,
    driver: ErasedEffectDriver,
}

static ROOT_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(ComponentDescriptor::root);

struct RootComponent;

impl DynComponent for RootComponent {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &ROOT_DESCRIPTOR
    }

    fn activate(
        &self,
        _: ComponentContext,
        _: ErasedConfig,
    ) -> Result<Activation, ActivationFailure> {
        Err(ActivationFailure::user(
            "the synthetic root cannot be activated",
        ))
    }
}

#[derive(Default)]
struct TurnOutput {
    tasks: Vec<RuntimeTask>,
    deferred_drops: Vec<Box<dyn Send + 'static>>,
    events: Vec<RuntimeEvent>,
}

impl ActorCore {
    fn new(
        runtime: Weak<RuntimeInner>,
        spawner: Arc<dyn TaskSpawner>,
        default_realm: RealmId,
        event_sender: Option<std::sync::mpsc::Sender<RuntimeEvent>>,
    ) -> Self {
        let root_implementation: Arc<dyn DynComponent> = Arc::new(RootComponent);
        let root_config: ErasedConfig = Arc::new(());
        let root_committed = Arc::new(CommittedView::new(
            ActivationKey::new(Target::new(Vec::new()), 0),
            Vec::new(),
        ));
        let root_episode = Episode::new(
            ROOT_EPISODE,
            root_committed,
            EpisodePayload {
                _implementation: Arc::clone(&root_implementation),
                _config: Arc::clone(&root_config),
                inverses: Vec::new(),
            },
        );
        let root_completion = Arc::new(FiberCompletion::new());
        let root = FiberRecord {
            parent: None,
            mount_effect: None,
            children: BTreeSet::new(),
            descriptor: &ROOT_DESCRIPTOR,
            implementation: root_implementation,
            config: root_config,
            route: ContextRoute::root(),
            lifecycle: Lifecycle::active(root_episode),
            effects: BTreeMap::new(),
            next_inverse_seq: 1,
            cleanup_running: None,
            unload_started: false,
            cleanup_errors: Vec::new(),
            episode_cleanup_errors: Vec::new(),
            completion: root_completion,
            settled_waiters: Vec::new(),
            dispose_waiters: Vec::new(),
        };
        Self {
            runtime,
            spawner,
            runtime_state: RuntimeState::Running,
            default_realm,
            contracts: ContractRegistry::default(),
            bindings: BindingTable::new(),
            watchers: BTreeMap::new(),
            consumers_by_provider: BTreeMap::new(),
            fibers: BTreeMap::from([(ROOT_FIBER, root)]),
            effect_owners: BTreeMap::new(),
            next_fiber: 1,
            next_episode: 1,
            next_step: 1,
            next_effect: 1,
            next_binding: 1,
            shutdown_waiters: Vec::new(),
            shutdown_errors: Vec::new(),
            stopped_report: None,
            event_sender,
            next_event_sequence: 2,
            next_snapshot_sequence: 1,
            quiescent_waiters: Vec::new(),
        }
    }

    fn handle(&mut self, command: Command) -> TurnOutput {
        let mut output = TurnOutput::default();
        match command {
            Command::Mount {
                owner,
                descriptor,
                implementation,
                config,
                config_type,
                route,
                context_valid,
                route_validation,
                reply,
            } => self.mount(
                owner,
                descriptor,
                implementation,
                config,
                config_type,
                route,
                context_valid,
                route_validation,
                reply,
                &mut output,
            ),
            Command::StartEffect {
                owner,
                label,
                driver,
                reply,
            } => self.start_effect(owner, label, driver, false, reply, &mut output),
            Command::RegisterCleanup {
                owner,
                label,
                inverse,
                reply,
            } => self.start_effect(
                owner,
                label,
                Box::pin(crate::effect::sync(move || Ok(Some(inverse))).into_driver()),
                true,
                reply,
                &mut output,
            ),
            Command::Provide {
                owner,
                route,
                service,
                contract,
                endpoint,
                host,
                context_valid,
                reply,
            } => self.provide(
                owner,
                route,
                service,
                contract,
                endpoint,
                host,
                context_valid,
                reply,
                &mut output,
            ),
            Command::ActivationFinished {
                fiber,
                episode,
                step,
                completion,
            } => self.activation_finished(fiber, episode, step, completion, &mut output),
            Command::EffectStepFinished {
                fiber,
                episode,
                effect,
                step,
                completion,
            } => self.effect_step_finished(fiber, episode, effect, step, completion, &mut output),
            Command::InverseFinished {
                fiber,
                episode,
                seq,
                result,
            } => self.inverse_finished(fiber, episode, seq, result, &mut output),
            Command::FiberState { fiber, reply } => {
                let state = self
                    .fibers
                    .get(&fiber)
                    .map_or(FiberState::Disposed, |record| {
                        record.lifecycle.project(false)
                    });
                let _ = reply.send(state);
            }
            Command::WaitSettled { fiber, reply } => self.wait_settled(fiber, reply),
            Command::WaitEffectForward {
                effect,
                completion,
                reply,
            } => self.wait_effect_forward(effect, completion, reply),
            Command::DisposeEffect {
                effect,
                completion,
                reply,
            } => self.dispose_effect(effect, completion, reply, &mut output),
            Command::Restart { fiber, reply } => self.restart(fiber, reply),
            Command::UpdateConfig {
                fiber,
                config,
                config_type,
                reply,
            } => self.update_config(fiber, config, config_type, reply, &mut output),
            Command::Replace {
                fiber,
                descriptor,
                config_type,
                implementation,
                reply,
            } => self.replace(
                fiber,
                descriptor,
                config_type,
                implementation,
                reply,
                &mut output,
            ),
            Command::Dispose { fiber, reply } => self.dispose(fiber, reply, &mut output),
            Command::Snapshot { reply } => {
                let snapshot = self.make_snapshot();
                let _ = reply.send(snapshot);
            }
            Command::WaitQuiescent { reply } => {
                if self.is_quiescent() {
                    let snapshot = self.make_snapshot();
                    let _ = reply.send(snapshot);
                } else {
                    self.quiescent_waiters.push(reply);
                }
            }
            Command::Shutdown { reply } => self.shutdown(reply, &mut output),
        }

        self.reconcile_all(&mut output);
        self.flush_settled_waiters();
        self.finish_shutdown_if_ready(&mut output);
        self.flush_quiescent_waiters();
        output
    }

    fn emit(&mut self, output: &mut TurnOutput, kind: RuntimeEventKind) {
        if self.event_sender.is_none() {
            return;
        }
        let sequence = self.next_event_sequence;
        self.next_event_sequence += 1;
        output.events.push(RuntimeEvent { sequence, kind });
    }

    #[allow(clippy::too_many_arguments)]
    fn mount(
        &mut self,
        owner: OperationOwner,
        descriptor: &'static ComponentDescriptor,
        implementation: Arc<dyn DynComponent>,
        config: ErasedConfig,
        config_type: TypeId,
        route: Arc<ContextRoute>,
        context_valid: bool,
        route_validation: Result<(), crate::ServiceTypeMismatch>,
        reply: oneshot::Sender<Result<MountedFiber, MountError>>,
        output: &mut TurnOutput,
    ) {
        if self.runtime_state != RuntimeState::Running {
            output.deferred_drops.push(Box::new(implementation));
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(MountError::RuntimeShuttingDown));
            return;
        }
        if self.scope_is_disposed(owner) {
            output.deferred_drops.push(Box::new(implementation));
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(MountError::FiberDisposed));
            return;
        }
        if !self.scope_accepts(owner) {
            output.deferred_drops.push(Box::new(implementation));
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(MountError::InactiveScope));
            return;
        }
        if !context_valid {
            output.deferred_drops.push(Box::new(implementation));
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(MountError::InactiveContext));
            return;
        }
        if descriptor.config_type() != config_type
            || !std::ptr::eq(descriptor, implementation.descriptor())
        {
            output.deferred_drops.push(Box::new(implementation));
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(MountError::DescriptorMismatch));
            return;
        }
        if let Err(error) = route_validation {
            output.deferred_drops.push(Box::new(implementation));
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(MountError::ServiceTypeMismatch(error)));
            return;
        }
        let staged = match self.contracts.validate_descriptor(descriptor) {
            Ok(staged) => staged,
            Err(error) => {
                output.deferred_drops.push(Box::new(implementation));
                output.deferred_drops.push(Box::new(config));
                let _ = reply.send(Err(MountError::ServiceTypeMismatch(error)));
                return;
            }
        };

        let id = self.allocate_fiber();
        let effect_id = self.allocate_effect();
        let completion = Arc::new(FiberCompletion::new());
        let effect_completion = Arc::new(EffectCompletion::new());
        effect_completion.complete_forward(Ok(EffectForwardOutcome::Completed));
        let label: Arc<str> = Arc::from(format!("linorun:mount:{}", descriptor.id()));
        self.contracts.commit(staged);
        {
            let parent = self
                .fibers
                .get_mut(&owner.fiber)
                .expect("validated mount owner is present");
            let seq = parent.next_inverse_seq;
            parent.next_inverse_seq += 1;
            parent
                .lifecycle
                .installed_episode_mut()
                .expect("validated mount owner has an episode")
                .payload_mut()
                .inverses
                .push(InverseEntry {
                    seq,
                    owner: InverseOwner::Effect(effect_id),
                    label: Some(Arc::clone(&label)),
                    action: InverseAction::RetireFiber(id),
                });
            parent.effects.insert(
                effect_id,
                EffectRecord {
                    id: effect_id,
                    episode: owner.episode,
                    label: Arc::clone(&label),
                    state: EffectState::Armed,
                    running_step: None,
                    forward_error: None,
                    cleanup_errors: Vec::new(),
                    completion: effect_completion,
                    forward_waiters: Vec::new(),
                    dispose_waiters: Vec::new(),
                },
            );
            parent.children.insert(id);
        }
        let previous_owner = self.effect_owners.insert(effect_id, owner.fiber);
        debug_assert_eq!(previous_owner, None);
        self.fibers.insert(
            id,
            FiberRecord {
                parent: Some(owner.fiber),
                mount_effect: Some(effect_id),
                children: BTreeSet::new(),
                descriptor,
                implementation,
                config,
                route,
                lifecycle: Lifecycle::new(),
                effects: BTreeMap::new(),
                next_inverse_seq: 1,
                cleanup_running: None,
                unload_started: false,
                cleanup_errors: Vec::new(),
                episode_cleanup_errors: Vec::new(),
                completion: Arc::clone(&completion),
                settled_waiters: Vec::new(),
                dispose_waiters: Vec::new(),
            },
        );
        self.register_watchers(id);
        self.emit(
            output,
            RuntimeEventKind::EffectStarted {
                fiber: owner.fiber,
                episode: owner.episode,
                effect: effect_id,
                label,
            },
        );
        self.emit(
            output,
            RuntimeEventKind::FiberInserted {
                fiber: id,
                parent: Some(owner.fiber),
                component: descriptor.id().clone(),
            },
        );
        self.emit(
            output,
            RuntimeEventKind::EffectForwardCompleted {
                effect: effect_id,
                outcome: EffectForwardOutcome::Completed,
            },
        );
        let _ = reply.send(Ok(MountedFiber { id, completion }));
    }

    fn start_effect(
        &mut self,
        owner: OperationOwner,
        label: Arc<str>,
        driver: ErasedEffectDriver,
        cleanup_registration: bool,
        reply: oneshot::Sender<Result<StartedEffect, EffectStartError>>,
        output: &mut TurnOutput,
    ) {
        // A started step must still be able to hand back cleanup for resources it acquires.
        let finishing_step = cleanup_registration
            && self.fibers.get(&owner.fiber).is_some_and(|record| {
                !record.unload_started
                    && record
                        .lifecycle
                        .installed_episode()
                        .is_some_and(|episode| episode.id() == owner.episode)
                    && (matches!(record.lifecycle.state(), LifecycleState::Reloading { .. })
                        || record
                            .effects
                            .values()
                            .any(|effect| effect.running_step.is_some()))
            });
        if self.runtime_state != RuntimeState::Running && !finishing_step {
            output.deferred_drops.push(Box::new(driver));
            let _ = reply.send(Err(EffectStartError::RuntimeShuttingDown));
            return;
        }
        if !self.scope_accepts(owner) && !finishing_step {
            output.deferred_drops.push(Box::new(driver));
            let _ = reply.send(Err(EffectStartError::InactiveScope));
            return;
        }

        let id = self.allocate_effect();
        let step = self.allocate_step();
        let completion = Arc::new(EffectCompletion::new());
        self.fibers
            .get_mut(&owner.fiber)
            .expect("validated effect owner is present")
            .effects
            .insert(
                id,
                EffectRecord {
                    id,
                    episode: owner.episode,
                    label: Arc::clone(&label),
                    state: EffectState::Applying,
                    running_step: Some(step),
                    forward_error: None,
                    cleanup_errors: Vec::new(),
                    completion: Arc::clone(&completion),
                    forward_waiters: Vec::new(),
                    dispose_waiters: Vec::new(),
                },
            );
        let previous_owner = self.effect_owners.insert(id, owner.fiber);
        debug_assert_eq!(previous_owner, None);
        self.emit(
            output,
            RuntimeEventKind::EffectStarted {
                fiber: owner.fiber,
                episode: owner.episode,
                effect: id,
                label,
            },
        );
        if self.fibers[&owner.fiber].lifecycle.is_unloading() {
            self.request_effect_dispose(owner.fiber, id, output);
        }
        self.schedule_effect(owner.fiber, owner.episode, id, step, driver, output);
        let _ = reply.send(Ok(StartedEffect { id, completion }));
    }

    #[allow(clippy::too_many_arguments)]
    fn provide(
        &mut self,
        owner: OperationOwner,
        route: Arc<ContextRoute>,
        service: ServiceId,
        contract: ContractVTable,
        endpoint: Arc<dyn ErasedEndpoint>,
        host: bool,
        context_valid: bool,
        reply: oneshot::Sender<Result<StartedEffect, ProvideError>>,
        output: &mut TurnOutput,
    ) {
        if self.runtime_state != RuntimeState::Running {
            output.deferred_drops.push(Box::new(endpoint));
            let _ = reply.send(Err(ProvideError::RuntimeShuttingDown));
            return;
        }
        if !self.scope_accepts(owner) {
            output.deferred_drops.push(Box::new(endpoint));
            let _ = reply.send(Err(ProvideError::InactiveScope));
            return;
        }
        if !context_valid {
            output.deferred_drops.push(Box::new(endpoint));
            let _ = reply.send(Err(ProvideError::InactiveContext));
            return;
        }
        let provider = self
            .fibers
            .get(&owner.fiber)
            .expect("validated provider is present");
        if !host && !provider.descriptor.provides().contains(&service) {
            output.deferred_drops.push(Box::new(endpoint));
            let _ = reply.send(Err(ProvideError::UndeclaredProvide));
            return;
        }
        let staged = match self
            .contracts
            .validate_erased_contract(service.clone(), contract)
        {
            Ok(staged) => staged,
            Err(error) => {
                output.deferred_drops.push(Box::new(endpoint));
                let _ = reply.send(Err(ProvideError::ServiceTypeMismatch(error)));
                return;
            }
        };
        let slot = BindingSlot::new(
            service.clone(),
            route.realm_for(&service, &self.default_realm),
        );
        if self.bindings.contains(&slot) {
            output.deferred_drops.push(Box::new(endpoint));
            let _ = reply.send(Err(ProvideError::DuplicateBinding));
            return;
        }

        let effect_id = self.allocate_effect();
        let binding_id = self.allocate_binding();
        let completion = Arc::new(EffectCompletion::new());
        completion.complete_forward(Ok(EffectForwardOutcome::Completed));
        let label: Arc<str> = Arc::from(format!("linorun:provide:{service}"));
        {
            let provider = self
                .fibers
                .get_mut(&owner.fiber)
                .expect("validated provider is present");
            let seq = provider.next_inverse_seq;
            provider.next_inverse_seq += 1;
            provider
                .lifecycle
                .installed_episode_mut()
                .expect("validated provider has an episode")
                .payload_mut()
                .inverses
                .push(InverseEntry {
                    seq,
                    owner: InverseOwner::Effect(effect_id),
                    label: Some(Arc::clone(&label)),
                    action: InverseAction::WithdrawBinding {
                        binding: binding_id,
                        slot: slot.clone(),
                        endpoint: Arc::clone(&endpoint),
                    },
                });
            provider.effects.insert(
                effect_id,
                EffectRecord {
                    id: effect_id,
                    episode: owner.episode,
                    label: Arc::clone(&label),
                    state: EffectState::Armed,
                    running_step: None,
                    forward_error: None,
                    cleanup_errors: Vec::new(),
                    completion: Arc::clone(&completion),
                    forward_waiters: Vec::new(),
                    dispose_waiters: Vec::new(),
                },
            );
        }
        let previous_owner = self.effect_owners.insert(effect_id, owner.fiber);
        debug_assert_eq!(previous_owner, None);
        let binding = BindingRecord::new(
            binding_id,
            slot,
            owner.fiber,
            contract.contract_type(),
            endpoint,
            effect_id,
        );
        let binding_event = binding_event_ref(&binding);
        assert!(
            self.bindings.insert(binding).is_ok(),
            "binding slot was validated as unoccupied"
        );
        self.contracts.commit(staged);
        self.emit(
            output,
            RuntimeEventKind::EffectStarted {
                fiber: owner.fiber,
                episode: owner.episode,
                effect: effect_id,
                label,
            },
        );
        self.emit(
            output,
            RuntimeEventKind::BindingInstalled {
                binding: binding_event.clone(),
            },
        );
        if self
            .fibers
            .get(&owner.fiber)
            .is_some_and(|provider| provider.lifecycle.is_active())
        {
            self.emit(
                output,
                RuntimeEventKind::BindingVisible {
                    binding: binding_event,
                },
            );
        }
        self.emit(
            output,
            RuntimeEventKind::EffectForwardCompleted {
                effect: effect_id,
                outcome: EffectForwardOutcome::Completed,
            },
        );
        let _ = reply.send(Ok(StartedEffect {
            id: effect_id,
            completion,
        }));
    }

    fn effect_step_finished(
        &mut self,
        fiber: FiberId,
        episode: EpisodeId,
        effect: EffectId,
        step: StepId,
        completion: EffectStepCompletion,
        output: &mut TurnOutput,
    ) {
        let valid = self
            .fibers
            .get(&fiber)
            .and_then(|record| record.effects.get(&effect))
            .is_some_and(|record| record.episode == episode && record.running_step == Some(step));
        if !valid {
            output.deferred_drops.push(Box::new(completion.driver));
            return;
        }

        match completion.result {
            Ok(event) => {
                let (has_next, yielded_seq) = match event {
                    DriverEvent::Yield { inverse, has_next } => {
                        let record = self
                            .fibers
                            .get_mut(&fiber)
                            .expect("validated effect fiber is present");
                        let label = Arc::clone(
                            &record
                                .effects
                                .get(&effect)
                                .expect("validated effect is present")
                                .label,
                        );
                        let seq = record.next_inverse_seq;
                        record.next_inverse_seq += 1;
                        record
                            .lifecycle
                            .installed_episode_mut()
                            .expect("effect owner episode remains installed")
                            .payload_mut()
                            .inverses
                            .push(InverseEntry {
                                seq,
                                owner: InverseOwner::Effect(effect),
                                label: Some(label),
                                action: InverseAction::User(inverse),
                            });
                        (has_next, Some(seq))
                    }
                    DriverEvent::Done => (false, None),
                };
                if let Some(seq) = yielded_seq {
                    self.emit(
                        output,
                        RuntimeEventKind::EffectYielded {
                            effect,
                            step,
                            seq,
                            has_next,
                        },
                    );
                }
                let disposing = self
                    .fibers
                    .get(&fiber)
                    .and_then(|record| record.effects.get(&effect))
                    .is_some_and(|record| record.state == EffectState::Disposing);
                if has_next && !disposing {
                    let next_step = self.allocate_step();
                    self.fibers
                        .get_mut(&fiber)
                        .expect("validated effect fiber is present")
                        .effects
                        .get_mut(&effect)
                        .expect("validated effect is present")
                        .running_step = Some(next_step);
                    self.schedule_effect(
                        fiber,
                        episode,
                        effect,
                        next_step,
                        completion.driver,
                        output,
                    );
                } else {
                    let outcome = if disposing {
                        EffectForwardOutcome::Disposed
                    } else {
                        EffectForwardOutcome::Completed
                    };
                    let effect_record = self
                        .fibers
                        .get_mut(&fiber)
                        .expect("validated effect fiber is present")
                        .effects
                        .get_mut(&effect)
                        .expect("validated effect is present");
                    effect_record.running_step = None;
                    if !disposing {
                        effect_record.state = EffectState::Armed;
                    }
                    self.complete_effect_forward(fiber, effect, Ok(outcome));
                    self.emit(
                        output,
                        RuntimeEventKind::EffectForwardCompleted { effect, outcome },
                    );
                    output.deferred_drops.push(Box::new(completion.driver));
                }
            }
            Err(error) => {
                let event_error = error.clone();
                let effect_record = self
                    .fibers
                    .get_mut(&fiber)
                    .expect("validated effect fiber is present")
                    .effects
                    .get_mut(&effect)
                    .expect("validated effect is present");
                effect_record.running_step = None;
                effect_record.forward_error = Some(error.clone());
                self.complete_effect_forward(fiber, effect, Err(error));
                self.emit(
                    output,
                    RuntimeEventKind::EffectForwardFailed {
                        effect,
                        error: event_error,
                    },
                );
                self.request_effect_dispose(fiber, effect, output);
                output.deferred_drops.push(Box::new(completion.driver));
            }
        }
    }

    fn activation_finished(
        &mut self,
        fiber: FiberId,
        episode: EpisodeId,
        step: StepId,
        completion: ActivationCompletion,
        output: &mut TurnOutput,
    ) {
        let Some(record) = self.fibers.get(&fiber) else {
            output.deferred_drops.push(Box::new(completion.driver));
            return;
        };
        if !record.lifecycle.matches_reloading(episode, step) {
            output.deferred_drops.push(Box::new(completion.driver));
            return;
        }

        let latest = self.latest_view(fiber);
        let committed = self
            .fibers
            .get(&fiber)
            .and_then(|record| record.lifecycle.installed_episode())
            .map(|episode| activation_key_snapshot(episode.key()))
            .expect("validated activation episode is reloading");
        let latest_snapshot = latest
            .as_ref()
            .map(|view| activation_key_snapshot(view.key()));
        match completion.result {
            Ok(event) => {
                let (has_next, yielded_seq) = match event {
                    DriverEvent::Yield { inverse, has_next } => {
                        let record = self
                            .fibers
                            .get_mut(&fiber)
                            .expect("validated activation fiber is present");
                        let seq = record.next_inverse_seq;
                        record.next_inverse_seq += 1;
                        record
                            .lifecycle
                            .reloading_episode_mut()
                            .expect("validated activation episode is reloading")
                            .payload_mut()
                            .inverses
                            .push(InverseEntry {
                                seq,
                                owner: InverseOwner::Activation,
                                label: None,
                                action: InverseAction::User(inverse),
                            });
                        (has_next, Some(seq))
                    }
                    DriverEvent::Done => (false, None),
                };
                if let Some(seq) = yielded_seq {
                    self.emit(
                        output,
                        RuntimeEventKind::ActivationYielded {
                            fiber,
                            episode,
                            step,
                            seq,
                            has_next,
                        },
                    );
                }

                let next_step = has_next.then(|| self.allocate_step());
                let boundary = next_step.map_or(SuccessBoundary::Final, |next_step| {
                    SuccessBoundary::Continue { next_step }
                });
                let landing = self
                    .fibers
                    .get_mut(&fiber)
                    .expect("validated activation fiber is present")
                    .lifecycle
                    .land_success(step, latest.as_ref().map(CommittedView::key), boundary);
                match landing {
                    Landing::Iter => {
                        self.emit(
                            output,
                            RuntimeEventKind::LIter {
                                fiber,
                                episode,
                                completed_step: step,
                            },
                        );
                        self.schedule_activation(
                            fiber,
                            episode,
                            next_step.expect("iteration allocated a next step"),
                            completion.driver,
                            output,
                        );
                    }
                    Landing::Divert => {
                        self.emit(
                            output,
                            RuntimeEventKind::LDivert {
                                fiber,
                                episode,
                                completed_step: step,
                                committed,
                                latest: latest_snapshot,
                            },
                        );
                        self.enter_unloading(fiber, output);
                        output.deferred_drops.push(Box::new(completion.driver));
                    }
                    Landing::Finish => {
                        self.emit(
                            output,
                            RuntimeEventKind::LFinish {
                                fiber,
                                episode,
                                completed_step: step,
                            },
                        );
                        for binding in self.binding_refs_for_provider(fiber) {
                            self.emit(output, RuntimeEventKind::BindingVisible { binding });
                        }
                        output.deferred_drops.push(Box::new(completion.driver));
                    }
                    Landing::Stale => {
                        output.deferred_drops.push(Box::new(completion.driver));
                    }
                }
            }
            Err(failure) => {
                let event_failure = failure.clone();
                let landing = self
                    .fibers
                    .get_mut(&fiber)
                    .expect("validated activation fiber is present")
                    .lifecycle
                    .land_failure(step, failure);
                debug_assert_eq!(landing, RaiseLanding::Raise);
                if landing == RaiseLanding::Raise {
                    self.emit(
                        output,
                        RuntimeEventKind::LRaise {
                            fiber,
                            episode,
                            completed_step: step,
                            error: event_failure,
                        },
                    );
                    self.enter_unloading(fiber, output);
                }
                output.deferred_drops.push(Box::new(completion.driver));
            }
        }
    }

    fn inverse_finished(
        &mut self,
        fiber: FiberId,
        episode: EpisodeId,
        seq: u64,
        result: Result<(), CleanupError>,
        output: &mut TurnOutput,
    ) {
        let event_error = result.as_ref().err().cloned();
        let failure = {
            let Some(record) = self.fibers.get_mut(&fiber) else {
                return;
            };
            let meta = match record.cleanup_running.as_ref() {
                Some(RunningInverse::User(meta)) if meta.episode == episode && meta.seq == seq => {
                    meta.clone()
                }
                _ => return,
            };
            let event_owner = meta.owner;
            let event_label = meta.label.clone();
            record.cleanup_running = None;
            let failure = result.err().map(|cause| {
                let failure = CleanupFailure {
                    owner: cleanup_owner(meta.owner),
                    fiber,
                    episode,
                    seq,
                    label: meta.label,
                    cause,
                };
                record.episode_cleanup_errors.push(failure.clone());
                if record.lifecycle.is_retired() {
                    record.cleanup_errors.push(failure.clone());
                }
                if let InverseOwner::Effect(effect) = meta.owner
                    && let Some(effect) = record.effects.get_mut(&effect)
                {
                    effect.cleanup_errors.push(failure.clone());
                }
                failure
            });
            self.emit(
                output,
                RuntimeEventKind::InverseFinished {
                    fiber,
                    episode,
                    seq,
                    owner: cleanup_owner(event_owner),
                    label: event_label,
                    error: event_error,
                },
            );
            failure
        };
        if let Some(failure) = failure {
            let mut ancestor = self.fibers.get(&fiber).and_then(|record| record.parent);
            while let Some(ancestor_id) = ancestor {
                let Some(record) = self.fibers.get_mut(&ancestor_id) else {
                    break;
                };
                ancestor = record.parent;
                if record.lifecycle.is_retired() {
                    record.cleanup_errors.push(failure.clone());
                }
            }
            if self.runtime_state == RuntimeState::ShuttingDown {
                self.shutdown_errors.push(failure);
            }
        }
    }

    fn wait_settled(&mut self, fiber: FiberId, reply: oneshot::Sender<FiberState>) {
        let Some(record) = self.fibers.get_mut(&fiber) else {
            let _ = reply.send(FiberState::Disposed);
            return;
        };
        let state = record.lifecycle.project(false);
        if is_settled(state) {
            let _ = reply.send(state);
        } else {
            record.settled_waiters.push(reply);
        }
    }

    fn wait_effect_forward(
        &mut self,
        effect: EffectId,
        completion: Arc<EffectCompletion>,
        reply: oneshot::Sender<Result<EffectForwardOutcome, EffectError>>,
    ) {
        let Some(fiber) = self.effect_fiber(effect) else {
            if let Some(result) = completion.forward() {
                let _ = reply.send(result);
            }
            return;
        };
        let record = self
            .fibers
            .get_mut(&fiber)
            .expect("effect index resolved an existing fiber")
            .effects
            .get_mut(&effect)
            .expect("effect index resolved an existing effect");
        if let Some(result) = record.completion.forward() {
            let _ = reply.send(result);
        } else {
            record.forward_waiters.push(reply);
        }
    }

    fn dispose_effect(
        &mut self,
        effect: EffectId,
        completion: Arc<EffectCompletion>,
        reply: oneshot::Sender<EffectDisposeReport>,
        output: &mut TurnOutput,
    ) {
        let Some(fiber) = self.effect_fiber(effect) else {
            let _ = reply.send(completion.report().unwrap_or_default());
            return;
        };
        let record = self
            .fibers
            .get_mut(&fiber)
            .expect("effect index resolved an existing fiber")
            .effects
            .get_mut(&effect)
            .expect("effect index resolved an existing effect");
        record.dispose_waiters.push(reply);
        self.request_effect_dispose(fiber, effect, output);
    }

    fn restart(&mut self, fiber: FiberId, reply: oneshot::Sender<Result<(), FiberError>>) {
        if self.runtime_state != RuntimeState::Running {
            let _ = reply.send(Err(FiberError::RuntimeShuttingDown));
            return;
        }
        let Some(record) = self.fibers.get_mut(&fiber) else {
            let _ = reply.send(Err(FiberError::FiberDisposed));
            return;
        };
        if record.lifecycle.is_retired() {
            let _ = reply.send(Err(FiberError::FiberDisposed));
            return;
        }
        record.lifecycle.restart();
        let _ = reply.send(Ok(()));
    }

    fn update_config(
        &mut self,
        fiber: FiberId,
        config: ErasedConfig,
        config_type: TypeId,
        reply: oneshot::Sender<Result<(), UpdateError>>,
        output: &mut TurnOutput,
    ) {
        if self.runtime_state != RuntimeState::Running {
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(UpdateError::RuntimeShuttingDown));
            return;
        }
        let Some(record) = self.fibers.get_mut(&fiber) else {
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(UpdateError::FiberDisposed));
            return;
        };
        if record.lifecycle.is_retired() {
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(UpdateError::FiberDisposed));
            return;
        }
        if record.descriptor.config_type() != config_type {
            output.deferred_drops.push(Box::new(config));
            let _ = reply.send(Err(UpdateError::DescriptorMismatch));
            return;
        }
        let old = std::mem::replace(&mut record.config, config);
        record.lifecycle.restart();
        output.deferred_drops.push(Box::new(old));
        let _ = reply.send(Ok(()));
    }

    fn replace(
        &mut self,
        fiber: FiberId,
        descriptor: &'static ComponentDescriptor,
        config_type: TypeId,
        implementation: Arc<dyn DynComponent>,
        reply: oneshot::Sender<Result<(), UpdateError>>,
        output: &mut TurnOutput,
    ) {
        if self.runtime_state != RuntimeState::Running {
            output.deferred_drops.push(Box::new(implementation));
            let _ = reply.send(Err(UpdateError::RuntimeShuttingDown));
            return;
        }
        let Some(record) = self.fibers.get_mut(&fiber) else {
            output.deferred_drops.push(Box::new(implementation));
            let _ = reply.send(Err(UpdateError::FiberDisposed));
            return;
        };
        if record.lifecycle.is_retired() {
            output.deferred_drops.push(Box::new(implementation));
            let _ = reply.send(Err(UpdateError::FiberDisposed));
            return;
        }
        if !std::ptr::eq(record.descriptor, descriptor)
            || record.descriptor.config_type() != config_type
            || !std::ptr::eq(descriptor, implementation.descriptor())
        {
            output.deferred_drops.push(Box::new(implementation));
            let _ = reply.send(Err(UpdateError::DescriptorMismatch));
            return;
        }
        let old = std::mem::replace(&mut record.implementation, implementation);
        record.lifecycle.restart();
        output.deferred_drops.push(Box::new(old));
        let _ = reply.send(Ok(()));
    }

    fn dispose(
        &mut self,
        fiber: FiberId,
        reply: oneshot::Sender<DisposeReport>,
        output: &mut TurnOutput,
    ) {
        self.satisfy_mount_inverse(fiber, output);
        let Some(record) = self.fibers.get_mut(&fiber) else {
            let _ = reply.send(DisposeReport::default());
            return;
        };
        record.dispose_waiters.push(reply);
        let first_retire = !record.lifecycle.is_retired();
        record.lifecycle.retire();
        if first_retire {
            self.emit(output, RuntimeEventKind::FiberRetired { fiber });
        }
    }

    fn shutdown(&mut self, reply: oneshot::Sender<ShutdownReport>, output: &mut TurnOutput) {
        match self.runtime_state {
            RuntimeState::Running => {
                self.runtime_state = RuntimeState::ShuttingDown;
                self.shutdown_waiters.push(reply);
                self.emit(output, RuntimeEventKind::RuntimeShuttingDown);
            }
            RuntimeState::ShuttingDown => self.shutdown_waiters.push(reply),
            RuntimeState::Stopped => {
                let _ = reply.send(self.stopped_report.clone().unwrap_or_default());
            }
        }
    }

    fn reconcile_all(&mut self, output: &mut TurnOutput) {
        loop {
            let fibers: Vec<_> = self.fibers.keys().copied().collect();
            let mut changed = false;
            for fiber in fibers {
                changed |= self.reconcile_one(fiber, output);
            }
            if !changed {
                break;
            }
        }
    }

    fn reconcile_one(&mut self, fiber: FiberId, output: &mut TurnOutput) -> bool {
        let latest = self.latest_view(fiber);
        let installed = self
            .fibers
            .get(&fiber)
            .and_then(|record| record.lifecycle.installed_episode())
            .map(|episode| (episode.id(), activation_key_snapshot(episode.key())));
        let latest_snapshot = latest
            .as_ref()
            .map(|view| activation_key_snapshot(view.key()));
        let action = self.fibers.get_mut(&fiber).and_then(|record| {
            record
                .lifecycle
                .reconcile(latest.as_ref().map(CommittedView::key))
        });

        match action {
            Some(ReconcileAction::Begin) => {
                return self.begin_episode(
                    fiber,
                    latest.expect("begin action requires a committed view"),
                    output,
                );
            }
            Some(ReconcileAction::Leave) => {
                let (episode, committed) =
                    installed.expect("leave action has an installed episode");
                let removed_bindings = self
                    .binding_refs_for_provider(fiber)
                    .iter()
                    .map(|binding| binding.id)
                    .collect();
                self.emit(
                    output,
                    RuntimeEventKind::LLeave {
                        fiber,
                        episode,
                        committed,
                        latest: latest_snapshot,
                        removed_bindings,
                    },
                );
                self.enter_unloading(fiber, output);
                self.drive_cleanup(fiber, output);
                return true;
            }
            None => {}
        }

        let Some(record) = self.fibers.get(&fiber) else {
            return false;
        };
        if matches!(record.lifecycle.state(), LifecycleState::Unloading { .. }) {
            return self.drive_cleanup(fiber, output);
        }
        if record
            .effects
            .values()
            .any(|effect| effect.state == EffectState::Disposing)
        {
            return self.drive_cleanup(fiber, output);
        }
        if fiber != ROOT_FIBER
            && record.lifecycle.is_retired()
            && record.lifecycle.is_inactive()
            && record.children.is_empty()
            && record.effects.is_empty()
            && !self.bindings.provider_has_bindings(fiber)
        {
            self.remove_fiber(fiber, output);
            return true;
        }
        false
    }

    fn begin_episode(
        &mut self,
        fiber: FiberId,
        committed: CommittedView,
        output: &mut TurnOutput,
    ) -> bool {
        let Some(runtime) = self.runtime.upgrade().map(RuntimeHandle::from_inner) else {
            return false;
        };
        let episode_id = self.allocate_episode();
        let step = self.allocate_step();
        let key = activation_key_snapshot(committed.key());
        let committed = Arc::new(committed);
        let providers = committed.key().target().providers().to_vec();
        let record = self
            .fibers
            .get_mut(&fiber)
            .expect("reconciled fiber is present");
        record.episode_cleanup_errors.clear();
        let implementation = Arc::clone(&record.implementation);
        let config = Arc::clone(&record.config);
        let context = Context::new(
            runtime.clone(),
            fiber,
            episode_id,
            Arc::clone(&record.route),
            Arc::clone(&committed),
        );
        let scope = Scope::new(runtime, fiber, episode_id);
        let component_context = ComponentContext::new(context, scope);
        let episode = Episode::new(
            episode_id,
            committed,
            EpisodePayload {
                _implementation: Arc::clone(&implementation),
                _config: Arc::clone(&config),
                inverses: Vec::new(),
            },
        );
        if record.lifecycle.begin(episode, step).is_err() {
            return false;
        }
        let _ = record;
        self.install_consumer_edges(fiber, &providers);
        self.emit(
            output,
            RuntimeEventKind::LBegin {
                fiber,
                episode: episode_id,
                key,
            },
        );
        self.schedule_activation(
            fiber,
            episode_id,
            step,
            ActivationDriver::Init {
                implementation,
                context: component_context,
                config,
            },
            output,
        );
        true
    }

    fn schedule_activation(
        &self,
        fiber: FiberId,
        episode: EpisodeId,
        step: StepId,
        driver: ActivationDriver,
        output: &mut TurnOutput,
    ) {
        let Some(runtime) = self.runtime.upgrade().map(RuntimeHandle::from_inner) else {
            output.deferred_drops.push(Box::new(driver));
            return;
        };
        output
            .tasks
            .push(activation_task(runtime, fiber, episode, step, driver));
    }

    fn schedule_effect(
        &self,
        fiber: FiberId,
        episode: EpisodeId,
        effect: EffectId,
        step: StepId,
        driver: ErasedEffectDriver,
        output: &mut TurnOutput,
    ) {
        let Some(runtime) = self.runtime.upgrade().map(RuntimeHandle::from_inner) else {
            output.deferred_drops.push(Box::new(driver));
            return;
        };
        output
            .tasks
            .push(effect_task(runtime, fiber, episode, effect, step, driver));
    }

    fn enter_unloading(&mut self, fiber: FiberId, output: &mut TurnOutput) {
        let removed = self.bindings.remove_provider(fiber);
        for binding in removed {
            self.emit(
                output,
                RuntimeEventKind::BindingRemoved {
                    binding: binding_event_ref(&binding),
                },
            );
            output.deferred_drops.push(Box::new(binding));
        }
        if let Some(record) = self.fibers.get_mut(&fiber) {
            let effects: Vec<_> = record.effects.keys().copied().collect();
            let _ = record;
            for effect in effects {
                self.request_effect_dispose(fiber, effect, output);
            }
        }
    }

    fn drive_cleanup(&mut self, fiber: FiberId, output: &mut TurnOutput) -> bool {
        let Some(record) = self.fibers.get(&fiber) else {
            return false;
        };
        if let Some(running) = record.cleanup_running.as_ref() {
            return match running {
                RunningInverse::User(_) => false,
                RunningInverse::Withdraw { consumers, .. } => {
                    if !consumers.iter().all(|(consumer, episode)| {
                        self.consumer_reached_boundary(*consumer, *episode)
                    }) {
                        return false;
                    }
                    let running = self
                        .fibers
                        .get_mut(&fiber)
                        .expect("cleanup fiber is present")
                        .cleanup_running
                        .take()
                        .expect("withdraw cleanup was observed above");
                    let RunningInverse::Withdraw {
                        meta,
                        endpoint,
                        consumers,
                    } = running
                    else {
                        unreachable!("withdraw cleanup was observed above")
                    };
                    self.emit(
                        output,
                        RuntimeEventKind::InverseFinished {
                            fiber,
                            episode: meta.episode,
                            seq: meta.seq,
                            owner: cleanup_owner(meta.owner),
                            label: meta.label.clone(),
                            error: None,
                        },
                    );
                    output.deferred_drops.push(Box::new(meta));
                    output.deferred_drops.push(Box::new(endpoint));
                    output.deferred_drops.push(Box::new(consumers));
                    true
                }
            };
        }

        let unloading = record.lifecycle.is_unloading();
        if unloading && self.has_installed_consumer(fiber) {
            return false;
        }
        if unloading
            && record
                .effects
                .values()
                .any(|effect| effect.running_step.is_some())
        {
            return false;
        }

        if unloading
            && !self
                .fibers
                .get(&fiber)
                .expect("cleanup fiber is present")
                .unload_started
        {
            let episode = self
                .fibers
                .get(&fiber)
                .and_then(|record| record.lifecycle.installed_episode())
                .map(Episode::id)
                .expect("unloading fiber has an episode");
            self.fibers
                .get_mut(&fiber)
                .expect("cleanup fiber is present")
                .unload_started = true;
            self.emit(output, RuntimeEventKind::LUnloadStarted { fiber, episode });
        }

        if self.finalize_ready_effects(fiber, output) {
            return true;
        }

        let requested_effects: BTreeSet<_> = self
            .fibers
            .get(&fiber)
            .expect("cleanup fiber is present")
            .effects
            .values()
            .filter(|effect| {
                effect.state == EffectState::Disposing && effect.running_step.is_none()
            })
            .map(|effect| effect.id)
            .collect();
        let entry = {
            let record = self
                .fibers
                .get_mut(&fiber)
                .expect("cleanup fiber is present");
            let Some(episode) = record.lifecycle.installed_episode_mut() else {
                return false;
            };
            let episode_id = episode.id();
            let inverses = &mut episode.payload_mut().inverses;
            let position = if unloading {
                inverses.len().checked_sub(1)
            } else {
                inverses.iter().rposition(|entry| {
                    matches!(entry.owner, InverseOwner::Effect(effect) if requested_effects.contains(&effect))
                })
            };
            position.map(|position| (episode_id, inverses.remove(position)))
        };

        let Some((episode, entry)) = entry else {
            if unloading {
                let record = self
                    .fibers
                    .get_mut(&fiber)
                    .expect("cleanup fiber is present");
                if !record.effects.is_empty() {
                    return false;
                }
                let episode = record
                    .lifecycle
                    .finish_unload()
                    .expect("cleanup operates on an unloading episode");
                let episode_id = episode.id();
                let providers = episode.key().target().providers().to_vec();
                record.unload_started = false;
                record.episode_cleanup_errors.clear();
                let _ = record;
                self.remove_consumer_edges(fiber, &providers);
                output.deferred_drops.push(Box::new(episode));
                self.emit(
                    output,
                    RuntimeEventKind::LUnloadFinished {
                        fiber,
                        episode: episode_id,
                    },
                );
                return true;
            }
            return false;
        };

        let meta = RunningMeta {
            episode,
            seq: entry.seq,
            owner: entry.owner,
            label: entry.label,
        };
        match entry.action {
            InverseAction::User(inverse) => {
                let Some(runtime) = self.runtime.upgrade().map(RuntimeHandle::from_inner) else {
                    output.deferred_drops.push(Box::new(inverse));
                    return false;
                };
                self.fibers
                    .get_mut(&fiber)
                    .expect("cleanup fiber is present")
                    .cleanup_running = Some(RunningInverse::User(meta.clone()));
                self.emit(
                    output,
                    RuntimeEventKind::InverseStarted {
                        fiber,
                        episode,
                        seq: entry.seq,
                        owner: cleanup_owner(meta.owner),
                        label: meta.label.clone(),
                    },
                );
                output
                    .tasks
                    .push(inverse_task(runtime, fiber, episode, entry.seq, inverse));
            }
            InverseAction::RetireFiber(child) => {
                self.emit(
                    output,
                    RuntimeEventKind::InverseStarted {
                        fiber,
                        episode,
                        seq: entry.seq,
                        owner: cleanup_owner(meta.owner),
                        label: meta.label.clone(),
                    },
                );
                self.retire_from_mount(child, output);
                self.emit(
                    output,
                    RuntimeEventKind::InverseFinished {
                        fiber,
                        episode,
                        seq: entry.seq,
                        owner: cleanup_owner(meta.owner),
                        label: meta.label,
                        error: None,
                    },
                );
            }
            InverseAction::WithdrawBinding {
                binding,
                slot,
                endpoint,
            } => {
                let removed = self.bindings.remove_if_matches(&slot, binding);
                let consumers = if removed.is_some() {
                    self.consumers_for_binding(&slot, binding)
                } else {
                    Vec::new()
                };
                if let Some(binding) = removed {
                    self.emit(
                        output,
                        RuntimeEventKind::BindingRemoved {
                            binding: binding_event_ref(&binding),
                        },
                    );
                    output.deferred_drops.push(Box::new(binding));
                }
                self.emit(
                    output,
                    RuntimeEventKind::InverseStarted {
                        fiber,
                        episode,
                        seq: entry.seq,
                        owner: cleanup_owner(meta.owner),
                        label: meta.label.clone(),
                    },
                );
                self.fibers
                    .get_mut(&fiber)
                    .expect("cleanup fiber is present")
                    .cleanup_running = Some(RunningInverse::Withdraw {
                    meta,
                    endpoint,
                    consumers,
                });
            }
        }
        true
    }

    fn finalize_ready_effects(&mut self, fiber: FiberId, output: &mut TurnOutput) -> bool {
        let ready: Vec<_> = {
            let Some(record) = self.fibers.get(&fiber) else {
                return false;
            };
            let owners: BTreeSet<_> = record
                .lifecycle
                .installed_episode()
                .map(|episode| {
                    episode
                        .payload()
                        .inverses
                        .iter()
                        .filter_map(|entry| match entry.owner {
                            InverseOwner::Effect(effect) => Some(effect),
                            InverseOwner::Activation => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            record
                .effects
                .values()
                .filter(|effect| {
                    effect.state == EffectState::Disposing
                        && effect.running_step.is_none()
                        && !owners.contains(&effect.id)
                })
                .map(|effect| effect.id)
                .collect()
        };
        if ready.is_empty() {
            return false;
        }
        for effect in ready {
            self.finish_effect(fiber, effect, output);
        }
        true
    }

    fn finish_effect(&mut self, fiber: FiberId, effect: EffectId, output: &mut TurnOutput) {
        let Some(mut effect) = self
            .fibers
            .get_mut(&fiber)
            .and_then(|record| record.effects.remove(&effect))
        else {
            debug_assert!(
                !self.effect_owners.contains_key(&effect),
                "effect owner index outlived its record"
            );
            return;
        };
        let previous_owner = self.effect_owners.remove(&effect.id);
        debug_assert_eq!(previous_owner, Some(fiber));
        if effect.completion.forward().is_none() {
            Self::publish_effect_forward(&mut effect, Ok(EffectForwardOutcome::Disposed));
        }
        let report = EffectDisposeReport {
            forward_error: effect.forward_error.clone(),
            cleanup_errors: effect.cleanup_errors.clone(),
        };
        effect.completion.finish(report.clone());
        for waiter in effect.dispose_waiters.drain(..) {
            let _ = waiter.send(report.clone());
        }
        output.deferred_drops.push(Box::new(effect));
    }

    fn complete_effect_forward(
        &mut self,
        fiber: FiberId,
        effect: EffectId,
        result: Result<EffectForwardOutcome, EffectError>,
    ) {
        let Some(effect) = self
            .fibers
            .get_mut(&fiber)
            .and_then(|record| record.effects.get_mut(&effect))
        else {
            return;
        };
        Self::publish_effect_forward(effect, result);
    }

    fn publish_effect_forward(
        effect: &mut EffectRecord,
        result: Result<EffectForwardOutcome, EffectError>,
    ) {
        effect.completion.complete_forward(result.clone());
        for waiter in effect.forward_waiters.drain(..) {
            let _ = waiter.send(result.clone());
        }
    }

    fn register_watchers(&mut self, fiber: FiberId) {
        let slots: Vec<_> = self
            .fibers
            .get(&fiber)
            .expect("mounted watcher fiber is present")
            .descriptor
            .requirements()
            .iter()
            .map(|requirement| {
                let record = self
                    .fibers
                    .get(&fiber)
                    .expect("mounted watcher fiber is present");
                BindingSlot::new(
                    requirement.service().clone(),
                    record
                        .route
                        .realm_for(requirement.service(), &self.default_realm),
                )
            })
            .collect();
        for slot in slots {
            self.watchers.entry(slot).or_default().insert(fiber);
        }
    }

    fn unregister_watchers(
        &mut self,
        fiber: FiberId,
        descriptor: &ComponentDescriptor,
        route: &ContextRoute,
    ) {
        let slots: Vec<_> = descriptor
            .requirements()
            .iter()
            .map(|requirement| {
                BindingSlot::new(
                    requirement.service().clone(),
                    route.realm_for(requirement.service(), &self.default_realm),
                )
            })
            .collect();
        for slot in slots {
            let remove_slot = self.watchers.get_mut(&slot).is_some_and(|watchers| {
                watchers.remove(&fiber);
                watchers.is_empty()
            });
            if remove_slot {
                self.watchers.remove(&slot);
            }
        }
    }

    fn install_consumer_edges(&mut self, consumer: FiberId, providers: &[FiberId]) {
        for provider in providers.iter().copied().collect::<BTreeSet<_>>() {
            if provider != consumer {
                self.consumers_by_provider
                    .entry(provider)
                    .or_default()
                    .insert(consumer);
            }
        }
    }

    fn remove_consumer_edges(&mut self, consumer: FiberId, providers: &[FiberId]) {
        for provider in providers.iter().copied().collect::<BTreeSet<_>>() {
            let remove_provider =
                self.consumers_by_provider
                    .get_mut(&provider)
                    .is_some_and(|consumers| {
                        consumers.remove(&consumer);
                        consumers.is_empty()
                    });
            if remove_provider {
                self.consumers_by_provider.remove(&provider);
            }
        }
    }

    fn consumers_for_binding(
        &self,
        slot: &BindingSlot,
        binding: crate::BindingId,
    ) -> Vec<(FiberId, EpisodeId)> {
        // A requirement watcher alone is not a dependency and may create a withdrawal wait cycle.
        self.watchers
            .get(slot)
            .into_iter()
            .flatten()
            .filter_map(|fiber| {
                let episode = self.fibers.get(fiber)?.lifecycle.installed_episode()?;
                episode
                    .committed()
                    .entries()
                    .iter()
                    .any(|entry| entry.binding() == binding)
                    .then_some((*fiber, episode.id()))
            })
            .collect()
    }

    fn binding_refs_for_provider(&self, provider: FiberId) -> Vec<BindingEventRef> {
        self.bindings
            .records()
            .filter(|binding| binding.provider() == provider)
            .map(binding_event_ref)
            .collect()
    }

    fn consumer_reached_boundary(&self, fiber: FiberId, captured_episode: EpisodeId) -> bool {
        let Some(record) = self.fibers.get(&fiber) else {
            return true;
        };
        // A later Episode no longer holds the withdrawn binding and must not extend this wait.
        if record
            .lifecycle
            .installed_episode()
            .is_none_or(|episode| episode.id() != captured_episode)
        {
            return true;
        }
        match record.lifecycle.state() {
            LifecycleState::Inactive { .. } => true,
            LifecycleState::Active { episode } => self
                .latest_view(fiber)
                .is_some_and(|latest| latest.key() == episode.key()),
            LifecycleState::Reloading { .. } | LifecycleState::Unloading { .. } => false,
        }
    }

    fn retire_from_mount(&mut self, child: FiberId, output: &mut TurnOutput) {
        let first_retire = if let Some(record) = self.fibers.get_mut(&child) {
            let first_retire = !record.lifecycle.is_retired();
            record.mount_effect = None;
            record.lifecycle.retire();
            first_retire
        } else {
            false
        };
        if first_retire {
            self.emit(output, RuntimeEventKind::FiberRetired { fiber: child });
        }
    }

    fn satisfy_mount_inverse(&mut self, child: FiberId, output: &mut TurnOutput) {
        let Some((parent, effect)) = self
            .fibers
            .get(&child)
            .and_then(|record| Some((record.parent?, record.mount_effect?)))
        else {
            return;
        };
        let mut satisfied = false;
        if let Some(parent_record) = self.fibers.get_mut(&parent)
            && let Some(episode) = parent_record.lifecycle.installed_episode_mut()
            && let Some(position) = episode.payload().inverses.iter().position(|entry| {
                entry.owner == InverseOwner::Effect(effect)
                    && matches!(&entry.action, InverseAction::RetireFiber(target) if *target == child)
            })
        {
            let entry = episode.payload_mut().inverses.remove(position);
            output.deferred_drops.push(Box::new(entry));
            satisfied = true;
        }
        if satisfied {
            self.request_effect_dispose(parent, effect, output);
        }
        if let Some(child) = self.fibers.get_mut(&child) {
            child.mount_effect = None;
        }
    }

    fn effect_fiber(&self, effect: EffectId) -> Option<FiberId> {
        self.effect_owners.get(&effect).copied()
    }

    fn scope_accepts(&self, owner: OperationOwner) -> bool {
        self.fibers
            .get(&owner.fiber)
            .is_some_and(|record| record.lifecycle.accepts_scope(owner.episode))
    }

    fn scope_is_disposed(&self, owner: OperationOwner) -> bool {
        self.fibers
            .get(&owner.fiber)
            .is_none_or(|record| record.lifecycle.is_retired())
    }

    fn request_effect_dispose(
        &mut self,
        fiber: FiberId,
        effect: EffectId,
        output: &mut TurnOutput,
    ) -> bool {
        let first_request = self
            .fibers
            .get(&fiber)
            .and_then(|record| record.effects.get(&effect))
            .is_some_and(|record| record.state != EffectState::Disposing);
        if !first_request {
            return false;
        }
        self.fibers
            .get_mut(&fiber)
            .expect("effect owner is present")
            .effects
            .get_mut(&effect)
            .expect("effect is present")
            .state = EffectState::Disposing;
        self.emit(output, RuntimeEventKind::EffectDisposeRequested { effect });
        true
    }

    fn has_installed_consumer(&self, provider: FiberId) -> bool {
        self.consumers_by_provider
            .get(&provider)
            .is_some_and(|consumers| !consumers.is_empty())
    }

    fn latest_view(&self, fiber: FiberId) -> Option<CommittedView> {
        if fiber == ROOT_FIBER {
            return (self.runtime_state == RuntimeState::Running).then(|| {
                CommittedView::new(ActivationKey::new(Target::new(Vec::new()), 0), Vec::new())
            });
        }
        let record = self.fibers.get(&fiber)?;
        if record.lifecycle.is_retired() {
            return None;
        }
        resolve_dependencies(
            record.descriptor,
            &record.route,
            &self.default_realm,
            &self.bindings,
            record.lifecycle.revision(),
            |provider| {
                self.fibers.get(&provider).is_some_and(|provider| {
                    provider.lifecycle.is_active() && !provider.lifecycle.is_retired()
                })
            },
        )
    }

    fn remove_fiber(&mut self, fiber: FiberId, output: &mut TurnOutput) {
        debug_assert_ne!(fiber, ROOT_FIBER);
        let Some(mut record) = self.fibers.remove(&fiber) else {
            return;
        };
        debug_assert!(record.effects.is_empty());
        debug_assert!(
            self.effect_owners.values().all(|owner| *owner != fiber),
            "fiber removal left indexed effects behind"
        );
        self.unregister_watchers(fiber, record.descriptor, &record.route);
        debug_assert!(
            self.consumers_by_provider
                .get(&fiber)
                .is_none_or(BTreeSet::is_empty),
            "a provider cannot be removed while it has installed consumers"
        );
        self.consumers_by_provider.remove(&fiber);
        debug_assert!(
            self.consumers_by_provider
                .values()
                .all(|consumers| !consumers.contains(&fiber))
        );
        if let Some(parent) = record.parent
            && let Some(parent) = self.fibers.get_mut(&parent)
        {
            parent.children.remove(&fiber);
        }
        let report = DisposeReport {
            cleanup_errors: record.cleanup_errors.clone(),
        };
        record.completion.finish(report.clone());
        for waiter in record.settled_waiters.drain(..) {
            let _ = waiter.send(FiberState::Disposed);
        }
        for waiter in record.dispose_waiters.drain(..) {
            let _ = waiter.send(report.clone());
        }
        self.emit(output, RuntimeEventKind::FiberRemoved { fiber });
        output.deferred_drops.push(Box::new(record));
    }

    fn is_quiescent(&self) -> bool {
        if self.runtime_state == RuntimeState::ShuttingDown {
            return false;
        }
        self.fibers.values().all(|record| {
            matches!(
                record.lifecycle.state(),
                LifecycleState::Inactive { .. } | LifecycleState::Active { .. }
            ) && record.cleanup_running.is_none()
                && record.effects.values().all(|effect| {
                    effect.state == EffectState::Armed && effect.running_step.is_none()
                })
        })
    }

    fn make_snapshot(&mut self) -> RuntimeSnapshot {
        let sequence = self.next_snapshot_sequence;
        self.next_snapshot_sequence += 1;

        let fiber_ids: Vec<_> = self.fibers.keys().copied().collect();
        let fibers = fiber_ids
            .iter()
            .map(|fiber| self.fiber_snapshot(*fiber))
            .collect();
        let bindings = self
            .bindings
            .records()
            .map(|binding| {
                let mut committed_consumers: Vec<_> = self
                    .fibers
                    .iter()
                    .filter_map(|(fiber, record)| {
                        record
                            .lifecycle
                            .installed_episode()
                            .is_some_and(|episode| {
                                episode
                                    .committed()
                                    .entries()
                                    .iter()
                                    .any(|entry| entry.binding() == binding.id())
                            })
                            .then_some(*fiber)
                    })
                    .collect();
                committed_consumers.sort_unstable();
                let visible = self
                    .fibers
                    .get(&binding.provider())
                    .is_some_and(|provider| {
                        provider.lifecycle.is_active() && !provider.lifecycle.is_retired()
                    });
                BindingSnapshot {
                    id: binding.id(),
                    service: binding.slot().service().clone(),
                    realm: binding.slot().realm().clone(),
                    provider: binding.provider(),
                    visible,
                    committed_consumers,
                    owner_effect: binding.owner_effect(),
                }
            })
            .collect();
        let mut effects: Vec<_> = fiber_ids
            .iter()
            .flat_map(|fiber| self.effect_snapshots(*fiber))
            .collect();
        effects.sort_by_key(|effect| {
            let min_seq = effect
                .inverses
                .iter()
                .map(|inverse| inverse.seq)
                .min()
                .unwrap_or(0);
            (effect.fiber, effect.inverses.is_empty(), min_seq, effect.id)
        });

        RuntimeSnapshot {
            sequence,
            runtime_state: self.runtime_state,
            quiescent: self.is_quiescent(),
            fibers,
            bindings,
            effects,
        }
    }

    fn fiber_snapshot(&self, fiber: FiberId) -> FiberSnapshot {
        let record = self.fibers.get(&fiber).expect("snapshot fiber is present");
        let latest_key = self
            .latest_view(fiber)
            .as_ref()
            .map(|view| activation_key_snapshot(view.key()));
        let relied_by = self
            .consumers_by_provider
            .get(&fiber)
            .map_or_else(Vec::new, |consumers| consumers.iter().copied().collect());
        let episode = record
            .lifecycle
            .installed_episode()
            .map(|episode| self.episode_snapshot(fiber, record, episode));
        FiberSnapshot {
            id: fiber,
            parent: record.parent,
            mount_effect: record.mount_effect,
            component: record.descriptor.id().clone(),
            state: record.lifecycle.project(false),
            retired: record.lifecycle.is_retired(),
            activation_revision: record.lifecycle.revision(),
            latest_key,
            episode,
            relied_by,
            children: record.children.iter().copied().collect(),
            failure: record.lifecycle.failure().cloned(),
        }
    }

    fn episode_snapshot(
        &self,
        fiber: FiberId,
        record: &FiberRecord,
        episode: &Episode<EpisodePayload>,
    ) -> crate::EpisodeSnapshot {
        let (phase, running_step) = match record.lifecycle.state() {
            LifecycleState::Reloading { step, .. } => (EpisodePhase::Loading, Some(*step)),
            LifecycleState::Active { .. } => (EpisodePhase::Active, None),
            LifecycleState::Unloading { .. } if record.cleanup_running.is_some() => {
                (EpisodePhase::Cleaning, None)
            }
            LifecycleState::Unloading { .. } => (EpisodePhase::WaitingWithdrawal, None),
            LifecycleState::Inactive { .. } => unreachable!("episode is installed"),
        };
        let mut accumulator = self.inverse_snapshots(fiber, record);
        accumulator.sort_by_key(|inverse| inverse.seq);
        crate::EpisodeSnapshot {
            id: episode.id(),
            committed_key: activation_key_snapshot(episode.key()),
            committed_bindings: episode
                .committed()
                .entries()
                .iter()
                .map(crate::topology::CommittedEntry::binding)
                .collect(),
            phase,
            running_step,
            accumulator,
            next_cleanup_seq: self.next_cleanup_seq(fiber, record),
            cleanup_failures: record.episode_cleanup_errors.clone(),
        }
    }

    fn effect_snapshots(&self, fiber: FiberId) -> Vec<EffectSnapshot> {
        let Some(record) = self.fibers.get(&fiber) else {
            return Vec::new();
        };
        let inverses = self.inverse_snapshots(fiber, record);
        record
            .effects
            .values()
            .map(|effect| EffectSnapshot {
                id: effect.id,
                fiber,
                episode: effect.episode,
                label: Arc::clone(&effect.label),
                state: match effect.state {
                    EffectState::Applying => EffectSnapshotState::Applying,
                    EffectState::Armed => EffectSnapshotState::Armed,
                    EffectState::Disposing => EffectSnapshotState::Disposing,
                },
                running_step: effect.running_step,
                forward_error: effect.forward_error.clone(),
                inverses: inverses
                    .iter()
                    .filter(|inverse| inverse.owner == CleanupOwner::Effect(effect.id))
                    .cloned()
                    .collect(),
            })
            .collect()
    }

    fn inverse_snapshots(&self, _fiber: FiberId, record: &FiberRecord) -> Vec<InverseSnapshot> {
        let mut inverses: Vec<_> = record
            .lifecycle
            .installed_episode()
            .into_iter()
            .flat_map(|episode| &episode.payload().inverses)
            .map(|entry| InverseSnapshot {
                seq: entry.seq,
                owner: cleanup_owner(entry.owner),
                label: entry.label.clone(),
                state: InverseSnapshotState::Armed,
                waiting_on: Vec::new(),
            })
            .collect();
        if let Some(running) = record.cleanup_running.as_ref() {
            let (meta, waiting_on) = match running {
                RunningInverse::User(meta) => (meta, Vec::new()),
                RunningInverse::Withdraw {
                    meta, consumers, ..
                } => {
                    let mut waiting: Vec<_> = consumers
                        .iter()
                        .filter(|(consumer, episode)| {
                            !self.consumer_reached_boundary(*consumer, *episode)
                        })
                        .map(|(consumer, _)| *consumer)
                        .collect();
                    waiting.sort_unstable();
                    (meta, waiting)
                }
            };
            inverses.push(InverseSnapshot {
                seq: meta.seq,
                owner: cleanup_owner(meta.owner),
                label: meta.label.clone(),
                state: InverseSnapshotState::Running,
                waiting_on,
            });
        }
        inverses
    }

    fn next_cleanup_seq(&self, fiber: FiberId, record: &FiberRecord) -> Option<u64> {
        if record.cleanup_running.is_some() {
            return None;
        }
        let episode = record.lifecycle.installed_episode()?;
        if record.lifecycle.is_unloading() {
            if self.has_installed_consumer(fiber)
                || record
                    .effects
                    .values()
                    .any(|effect| effect.running_step.is_some())
            {
                return None;
            }
            return episode
                .payload()
                .inverses
                .iter()
                .map(|entry| entry.seq)
                .max();
        }
        let requested: BTreeSet<_> = record
            .effects
            .values()
            .filter(|effect| {
                effect.state == EffectState::Disposing && effect.running_step.is_none()
            })
            .map(|effect| effect.id)
            .collect();
        episode
            .payload()
            .inverses
            .iter()
            .filter_map(|entry| match entry.owner {
                InverseOwner::Effect(effect) if requested.contains(&effect) => Some(entry.seq),
                _ => None,
            })
            .max()
    }

    fn flush_quiescent_waiters(&mut self) {
        if !self.is_quiescent() {
            return;
        }
        let waiters = std::mem::take(&mut self.quiescent_waiters);
        for waiter in waiters {
            let snapshot = self.make_snapshot();
            let _ = waiter.send(snapshot);
        }
    }

    fn flush_settled_waiters(&mut self) {
        for record in self.fibers.values_mut() {
            let state = record.lifecycle.project(false);
            if is_settled(state) {
                for waiter in record.settled_waiters.drain(..) {
                    let _ = waiter.send(state);
                }
            }
        }
    }

    fn finish_shutdown_if_ready(&mut self, output: &mut TurnOutput) {
        if self.runtime_state != RuntimeState::ShuttingDown || self.fibers.len() != 1 {
            return;
        }
        let root = self
            .fibers
            .get(&ROOT_FIBER)
            .expect("synthetic root remains registered");
        if !root.lifecycle.is_inactive()
            || !root.children.is_empty()
            || !root.effects.is_empty()
            || root.cleanup_running.is_some()
            || self.bindings.provider_has_bindings(ROOT_FIBER)
        {
            return;
        }
        self.runtime_state = RuntimeState::Stopped;
        self.emit(output, RuntimeEventKind::RuntimeStopped);
        let report = ShutdownReport {
            cleanup_errors: std::mem::take(&mut self.shutdown_errors),
        };
        self.stopped_report = Some(report.clone());
        for waiter in self.shutdown_waiters.drain(..) {
            let _ = waiter.send(report.clone());
        }
    }

    fn allocate_fiber(&mut self) -> FiberId {
        let id = FiberId::new(self.next_fiber);
        self.next_fiber += 1;
        id
    }

    fn allocate_episode(&mut self) -> EpisodeId {
        let id = EpisodeId::new(self.next_episode);
        self.next_episode += 1;
        id
    }

    fn allocate_step(&mut self) -> StepId {
        let id = StepId::new(self.next_step);
        self.next_step += 1;
        id
    }

    fn allocate_effect(&mut self) -> EffectId {
        let id = EffectId::new(self.next_effect);
        self.next_effect += 1;
        id
    }

    fn allocate_binding(&mut self) -> crate::BindingId {
        let id = crate::BindingId::new(self.next_binding);
        self.next_binding += 1;
        id
    }
}

async fn actor_loop(mut receiver: mpsc::UnboundedReceiver<Command>, mut actor: ActorCore) {
    if let Some(sender) = &actor.event_sender {
        let _ = sender.send(RuntimeEvent {
            sequence: 1,
            kind: RuntimeEventKind::FiberInserted {
                fiber: ROOT_FIBER,
                parent: None,
                component: ROOT_DESCRIPTOR.id().clone(),
            },
        });
    }
    while let Some(command) = receiver.next().await {
        let output = actor.handle(command);
        drop(output.deferred_drops);
        if let Some(sender) = &actor.event_sender {
            for event in output.events {
                let _ = sender.send(event);
            }
        }
        for task in output.tasks {
            actor.spawner.spawn(task);
        }
    }
}

const fn cleanup_owner(owner: InverseOwner) -> CleanupOwner {
    match owner {
        InverseOwner::Activation => CleanupOwner::Activation,
        InverseOwner::Effect(effect) => CleanupOwner::Effect(effect),
    }
}

fn activation_key_snapshot(key: &ActivationKey) -> ActivationKeySnapshot {
    ActivationKeySnapshot {
        providers: key.target().providers().to_vec(),
        revision: key.revision(),
    }
}

fn binding_event_ref(binding: &BindingRecord) -> BindingEventRef {
    BindingEventRef {
        id: binding.id(),
        service: binding.slot().service().clone(),
        realm: binding.slot().realm().clone(),
        provider: binding.provider(),
        owner_effect: binding.owner_effect(),
    }
}

const fn is_settled(state: FiberState) -> bool {
    matches!(
        state,
        FiberState::Pending | FiberState::Active | FiberState::Failed | FiberState::Disposed
    )
}

#[cfg(test)]
mod tests;
