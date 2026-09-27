use std::{
    any::TypeId,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use futures_channel::{mpsc, oneshot};

use crate::component::{ComponentAdapter, DynComponent, ErasedConfig};
use crate::id::RuntimeTag;
use crate::route::ContextRoute;
use crate::service::ContractVTable;
use crate::topology::validate_descriptor_route;
use crate::{
    CleanupError, Component, DisposeReport, EffectDisposeReport, EffectDriver, EffectError,
    EffectForwardOutcome, EffectId, EffectStartError, EpisodeId, FiberError, FiberId, FiberState,
    IntoEffectDriver, MountError, ProvideError, RealmId, RealmMismatch, RuntimeEvent,
    RuntimeSnapshot, ServiceContract, ServiceId, ShutdownReport, UpdateError, effect,
};

use super::completion::{EffectCompletion, FiberCompletion};
use super::{ActorCore, Command, ROOT_EPISODE, ROOT_FIBER, actor_loop};

pub type RuntimeTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Submits runtime tasks without polling them inline or blocking the caller.
///
/// Closures implement this trait, so an existing executor can be connected directly.
pub trait TaskSpawner: Send + Sync + 'static {
    fn spawn(&self, task: RuntimeTask);
}

impl<F> TaskSpawner for F
where
    F: Fn(RuntimeTask) + Send + Sync + 'static,
{
    fn spawn(&self, task: RuntimeTask) {
        self(task);
    }
}

/// Owns the serialized actor that coordinates component and effect lifecycles.
///
/// Handles are cheap to clone and may be used from different threads. User futures are spawned
/// through the supplied [`TaskSpawner`]; the actor itself never awaits user code.
///
/// ```no_run
/// use std::{sync::Arc, thread};
///
/// use futures_executor::block_on;
/// use linorun_core::{
///     Activation, Component, ComponentContext, ComponentDescriptor, EffectForwardOutcome,
///     EffectHandle, FiberHandle, Runtime, RuntimeTask, TaskSpawner, effect,
/// };
/// use linorun_macros::component_descriptor;
///
/// component_descriptor! {
///     static WORKER: () {
///         id: "docs/runtime-worker@1",
///         requires: [],
///         provides: [],
///     }
/// }
///
/// struct Worker;
///
/// impl Component for Worker {
///     type Config = ();
///
///     fn descriptor(&self) -> &'static ComponentDescriptor {
///         &WORKER
///     }
///
///     fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
///         Activation::Once(Box::pin(async { Ok(None) }))
///     }
/// }
///
/// #[derive(Clone, Copy)]
/// struct ThreadSpawner;
///
/// impl TaskSpawner for ThreadSpawner {
///     fn spawn(&self, task: RuntimeTask) {
///         thread::spawn(move || block_on(task));
///     }
/// }
///
/// block_on(async {
///     let runtime = Runtime::builder(ThreadSpawner).build();
///     let host_effect: EffectHandle = runtime
///         .root()
///         .effect("host setup", effect::sync(|| Ok(None)))
///         .await
///         .unwrap();
///     assert_eq!(
///         host_effect.wait_forward().await,
///         Ok(EffectForwardOutcome::Completed)
///     );
///
///     let fiber: FiberHandle = runtime.root().mount(Worker, ()).await.unwrap();
///     fiber.wait_settled().await;
///     runtime.shutdown().await;
/// });
/// ```
#[derive(Clone)]
pub struct Runtime {
    handle: RuntimeHandle,
}

pub struct RuntimeBuilder {
    spawner: Arc<dyn TaskSpawner>,
    event_sender: Option<std::sync::mpsc::Sender<RuntimeEvent>>,
}

#[derive(Clone)]
pub struct RootContext {
    runtime: RuntimeHandle,
    route: Arc<ContextRoute>,
}

/// A stable reference to one mounted component Fiber.
#[derive(Clone)]
pub struct FiberHandle {
    runtime: RuntimeHandle,
    id: FiberId,
    completion: Arc<FiberCompletion>,
}

/// A stable reference to one independently disposable effect.
#[derive(Clone)]
pub struct EffectHandle {
    runtime: RuntimeHandle,
    id: EffectId,
    completion: Arc<EffectCompletion>,
}

impl Runtime {
    /// Creates a runtime using the host's task submission function or [`TaskSpawner`].
    /// Use [`Self::builder`] when an event receiver is also needed.
    pub fn new<S: TaskSpawner>(spawner: S) -> Self {
        Self::builder(spawner).build()
    }

    pub fn builder<S>(spawner: S) -> RuntimeBuilder
    where
        S: TaskSpawner,
    {
        RuntimeBuilder {
            spawner: Arc::new(spawner),
            event_sender: None,
        }
    }

    pub fn root(&self) -> RootContext {
        RootContext {
            runtime: self.handle.clone(),
            route: ContextRoute::root(),
        }
    }

    /// Mounts a component in the default service realm, returning its control handle.
    /// Mounting does not wait for startup; use [`FiberHandle::wait_settled`] to observe the result.
    pub async fn mount<C: Component>(
        &self,
        component: C,
        config: C::Config,
    ) -> Result<FiberHandle, MountError> {
        self.root().mount(component, config).await
    }

    /// Publishes a host-owned service until its handle is disposed or the runtime shuts down.
    pub async fn provide<C: ServiceContract>(
        &self,
        provider: C::Provider,
    ) -> Result<EffectHandle, ProvideError> {
        self.root().provide::<C>(provider).await
    }

    /// Starts a host-owned reversible operation.
    pub async fn effect<E: IntoEffectDriver>(
        &self,
        label: impl Into<Arc<str>>,
        effect: E,
    ) -> Result<EffectHandle, EffectStartError> {
        self.root().effect(label, effect).await
    }

    /// Registers host resource cleanup, to run on manual disposal or shutdown.
    pub async fn on_cleanup<F, Fut>(
        &self,
        label: impl Into<Arc<str>>,
        cleanup: F,
    ) -> Result<EffectHandle, EffectStartError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CleanupError>> + Send + 'static,
    {
        self.root().on_cleanup(label, cleanup).await
    }

    pub async fn shutdown(&self) -> ShutdownReport {
        let (reply, receiver) = oneshot::channel();
        if !self.handle.post(Command::Shutdown { reply }) {
            return ShutdownReport::default();
        }
        receiver
            .await
            .expect("runtime actor stopped before acknowledging shutdown")
    }

    pub async fn snapshot(&self) -> RuntimeSnapshot {
        let (reply, receiver) = oneshot::channel();
        self.handle
            .post(Command::Snapshot { reply })
            .then_some(())
            .expect("runtime actor stopped before snapshot request");
        receiver
            .await
            .expect("runtime actor stopped before producing snapshot")
    }

    pub async fn wait_quiescent(&self) -> RuntimeSnapshot {
        let (reply, receiver) = oneshot::channel();
        self.handle
            .post(Command::WaitQuiescent { reply })
            .then_some(())
            .expect("runtime actor stopped before quiescence request");
        receiver
            .await
            .expect("runtime actor stopped before reaching quiescence")
    }
}

impl RuntimeBuilder {
    pub fn event_sender(mut self, sender: std::sync::mpsc::Sender<RuntimeEvent>) -> Self {
        self.event_sender = Some(sender);
        self
    }

    pub fn build(self) -> Runtime {
        let runtime_tag = RuntimeTag::allocate();
        let default_realm = RealmId::new(Arc::clone(&runtime_tag), 0);
        let (sender, receiver) = mpsc::unbounded();
        let inner = Arc::new(RuntimeInner {
            sender,
            runtime_tag,
            default_realm: default_realm.clone(),
            next_realm: AtomicU64::new(1),
        });
        let handle = RuntimeHandle(Arc::clone(&inner));
        let actor = ActorCore::new(
            Arc::downgrade(&inner),
            Arc::clone(&self.spawner),
            default_realm,
            self.event_sender,
        );
        self.spawner.spawn(Box::pin(actor_loop(receiver, actor)));
        Runtime { handle }
    }
}

impl RootContext {
    pub fn default_realm(&self) -> RealmId {
        self.runtime.default_realm()
    }

    pub fn create_realm(&self) -> RealmId {
        self.runtime.create_realm()
    }

    pub fn isolate<C: crate::ServiceContract>(
        &self,
        realm: RealmId,
    ) -> Result<Self, RealmMismatch> {
        let route = self.route.isolate::<C>(self.runtime.runtime_tag(), realm)?;
        Ok(Self {
            runtime: self.runtime.clone(),
            route,
        })
    }

    pub fn intercept<C: crate::ServiceContract>(&self, value: C::Intercept) -> Self {
        Self {
            runtime: self.runtime.clone(),
            route: self.route.intercept::<C>(value),
        }
    }

    pub async fn effect<E>(
        &self,
        label: impl Into<Arc<str>>,
        effect: E,
    ) -> Result<EffectHandle, EffectStartError>
    where
        E: IntoEffectDriver,
    {
        self.runtime
            .start_effect(
                OperationOwner::root(),
                label.into(),
                Box::pin(effect.into_driver()),
            )
            .await
    }

    /// Registers cleanup in this host context and waits until it has been recorded.
    pub async fn on_cleanup<F, Fut>(
        &self,
        label: impl Into<Arc<str>>,
        cleanup: F,
    ) -> Result<EffectHandle, EffectStartError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CleanupError>> + Send + 'static,
    {
        self.runtime
            .register_cleanup(OperationOwner::root(), label.into(), cleanup)
            .await
    }

    pub async fn provide<C>(&self, service: C::Provider) -> Result<EffectHandle, ProvideError>
    where
        C: ServiceContract,
    {
        self.runtime
            .provide::<C>(
                OperationOwner::root(),
                Arc::clone(&self.route),
                service,
                true,
                true,
            )
            .await
    }

    pub async fn mount<C>(&self, component: C, config: C::Config) -> Result<FiberHandle, MountError>
    where
        C: Component,
    {
        self.runtime
            .mount_component(
                OperationOwner::root(),
                Arc::clone(&self.route),
                component,
                config,
                true,
            )
            .await
    }
}

impl FiberHandle {
    pub const fn id(&self) -> FiberId {
        self.id
    }

    pub async fn state(&self) -> FiberState {
        if self.completion.is_disposed() {
            return FiberState::Disposed;
        }
        let (reply, receiver) = oneshot::channel();
        if !self.runtime.post(Command::FiberState {
            fiber: self.id,
            reply,
        }) {
            return self.completed_state();
        }
        receiver.await.unwrap_or_else(|_| self.completed_state())
    }

    pub async fn wait_settled(&self) -> FiberState {
        if self.completion.is_disposed() {
            return FiberState::Disposed;
        }
        let (reply, receiver) = oneshot::channel();
        if !self.runtime.post(Command::WaitSettled {
            fiber: self.id,
            reply,
        }) {
            return self.completed_state();
        }
        receiver.await.unwrap_or_else(|_| self.completed_state())
    }

    pub async fn restart(&self) -> Result<(), FiberError> {
        if self.completion.is_disposed() {
            return Err(FiberError::FiberDisposed);
        }
        let (reply, receiver) = oneshot::channel();
        if !self.runtime.post(Command::Restart {
            fiber: self.id,
            reply,
        }) {
            return Err(FiberError::RuntimeShuttingDown);
        }
        receiver
            .await
            .unwrap_or(Err(FiberError::RuntimeShuttingDown))
    }

    pub async fn update_config<T>(&self, config: T) -> Result<(), UpdateError>
    where
        T: Send + Sync + 'static,
    {
        if self.completion.is_disposed() {
            return Err(UpdateError::FiberDisposed);
        }
        let (reply, receiver) = oneshot::channel();
        if !self.runtime.post(Command::UpdateConfig {
            fiber: self.id,
            config: Arc::new(config),
            config_type: TypeId::of::<T>(),
            reply,
        }) {
            return Err(UpdateError::RuntimeShuttingDown);
        }
        receiver
            .await
            .unwrap_or(Err(UpdateError::RuntimeShuttingDown))
    }

    pub async fn replace<C>(&self, component: C) -> Result<(), UpdateError>
    where
        C: Component,
    {
        if self.completion.is_disposed() {
            return Err(UpdateError::FiberDisposed);
        }
        let descriptor = component.descriptor();
        let implementation: Arc<dyn DynComponent> =
            Arc::new(ComponentAdapter::new(component, descriptor));
        let (reply, receiver) = oneshot::channel();
        if !self.runtime.post(Command::Replace {
            fiber: self.id,
            descriptor,
            config_type: TypeId::of::<C::Config>(),
            implementation,
            reply,
        }) {
            return Err(UpdateError::RuntimeShuttingDown);
        }
        receiver
            .await
            .unwrap_or(Err(UpdateError::RuntimeShuttingDown))
    }

    pub async fn dispose(&self) -> DisposeReport {
        if let Some(report) = self.completion.report() {
            return report;
        }
        let (reply, receiver) = oneshot::channel();
        if !self.runtime.post(Command::Dispose {
            fiber: self.id,
            reply,
        }) {
            return self.completion.report().unwrap_or_default();
        }
        let actor_report = receiver
            .await
            .unwrap_or_else(|_| self.completion.report().unwrap_or_default());
        self.completion.report().unwrap_or(actor_report)
    }

    fn completed_state(&self) -> FiberState {
        if self.completion.is_disposed() {
            FiberState::Disposed
        } else {
            panic!("runtime actor stopped while the fiber was still registered")
        }
    }
}

impl EffectHandle {
    pub const fn id(&self) -> EffectId {
        self.id
    }

    pub async fn wait_forward(&self) -> Result<EffectForwardOutcome, EffectError> {
        if let Some(result) = self.completion.forward() {
            return result;
        }
        let (reply, receiver) = oneshot::channel();
        if !self.runtime.post(Command::WaitEffectForward {
            effect: self.id,
            completion: Arc::clone(&self.completion),
            reply,
        }) {
            return self
                .completion
                .forward()
                .expect("runtime stopped before effect forward reached a boundary");
        }
        receiver.await.unwrap_or_else(|_| {
            self.completion
                .forward()
                .expect("runtime stopped before effect forward reached a boundary")
        })
    }

    /// Requests cleanup and waits for this effect's inverses to finish.
    /// For service publications, withdrawal waits only for captured consumer Episodes,
    /// not unrelated requirement watchers or later Episodes of the same consumers.
    pub async fn dispose(&self) -> EffectDisposeReport {
        if let Some(report) = self.completion.report() {
            return report;
        }
        let (reply, receiver) = oneshot::channel();
        if !self.runtime.post(Command::DisposeEffect {
            effect: self.id,
            completion: Arc::clone(&self.completion),
            reply,
        }) {
            return self.completion.report().unwrap_or_default();
        }
        receiver
            .await
            .unwrap_or_else(|_| self.completion.report().unwrap_or_default())
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeHandle(Arc<RuntimeInner>);

pub(super) struct RuntimeInner {
    sender: mpsc::UnboundedSender<Command>,
    runtime_tag: Arc<RuntimeTag>,
    default_realm: RealmId,
    next_realm: AtomicU64,
}

impl RuntimeHandle {
    pub(super) fn from_inner(inner: Arc<RuntimeInner>) -> Self {
        Self(inner)
    }

    pub(super) fn post(&self, command: Command) -> bool {
        self.0.sender.unbounded_send(command).is_ok()
    }

    pub(crate) fn default_realm(&self) -> RealmId {
        self.0.default_realm.clone()
    }

    pub(crate) fn runtime_tag(&self) -> &Arc<RuntimeTag> {
        &self.0.runtime_tag
    }

    fn create_realm(&self) -> RealmId {
        let local = self.0.next_realm.fetch_add(1, Ordering::Relaxed);
        RealmId::new(Arc::clone(&self.0.runtime_tag), local)
    }

    pub(crate) fn same_runtime(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) async fn start_effect(
        &self,
        owner: OperationOwner,
        label: Arc<str>,
        driver: ErasedEffectDriver,
    ) -> Result<EffectHandle, EffectStartError> {
        let (reply, receiver) = oneshot::channel();
        if !self.post(Command::StartEffect {
            owner,
            label,
            driver,
            reply,
        }) {
            return Err(EffectStartError::RuntimeShuttingDown);
        }
        let started = receiver
            .await
            .expect("runtime actor stopped before acknowledging effect")?;
        Ok(EffectHandle {
            runtime: self.clone(),
            id: started.id,
            completion: started.completion,
        })
    }

    pub(crate) async fn register_cleanup<F, Fut>(
        &self,
        owner: OperationOwner,
        label: Arc<str>,
        cleanup: F,
    ) -> Result<EffectHandle, EffectStartError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CleanupError>> + Send + 'static,
    {
        let (reply, receiver) = oneshot::channel();
        if !self.post(Command::RegisterCleanup {
            owner,
            label,
            inverse: effect::inverse(cleanup),
            reply,
        }) {
            return Err(EffectStartError::RuntimeShuttingDown);
        }
        let started = receiver
            .await
            .expect("runtime actor stopped before acknowledging cleanup registration")?;
        let handle = EffectHandle {
            runtime: self.clone(),
            id: started.id,
            completion: started.completion,
        };
        // Registration never calls user code. Waiting preserves sequence order across registrations.
        handle
            .wait_forward()
            .await
            .expect("recording a cleanup callback cannot fail");
        Ok(handle)
    }

    pub(crate) async fn provide<C>(
        &self,
        owner: OperationOwner,
        route: Arc<ContextRoute>,
        provider: C::Provider,
        host: bool,
        context_valid: bool,
    ) -> Result<EffectHandle, ProvideError>
    where
        C: ServiceContract,
    {
        let endpoint = C::erase(provider);
        let (reply, receiver) = oneshot::channel();
        let command = Command::Provide {
            owner,
            route,
            service: ServiceId::new(C::ID),
            contract: ContractVTable::of::<C>(),
            endpoint,
            host,
            context_valid,
            reply,
        };
        if !self.post(command) {
            return Err(ProvideError::RuntimeShuttingDown);
        }
        let started = receiver
            .await
            .expect("runtime actor stopped before acknowledging provide")?;
        Ok(EffectHandle {
            runtime: self.clone(),
            id: started.id,
            completion: started.completion,
        })
    }

    pub(crate) async fn mount_component<C>(
        &self,
        owner: OperationOwner,
        route: Arc<ContextRoute>,
        component: C,
        config: C::Config,
        context_valid: bool,
    ) -> Result<FiberHandle, MountError>
    where
        C: Component,
    {
        let descriptor = component.descriptor();
        let route_validation = validate_descriptor_route(descriptor, &route);
        let implementation: Arc<dyn DynComponent> =
            Arc::new(ComponentAdapter::new(component, descriptor));
        let config: ErasedConfig = Arc::new(config);
        let (reply, receiver) = oneshot::channel();
        let command = Command::Mount {
            owner,
            descriptor,
            implementation,
            config,
            config_type: TypeId::of::<C::Config>(),
            route,
            context_valid,
            route_validation,
            reply,
        };
        if !self.post(command) {
            return Err(MountError::RuntimeShuttingDown);
        }

        let mounted = receiver
            .await
            .expect("runtime actor stopped before acknowledging mount")?;
        Ok(FiberHandle {
            runtime: self.clone(),
            id: mounted.id,
            completion: mounted.completion,
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct OperationOwner {
    pub(super) fiber: FiberId,
    pub(super) episode: EpisodeId,
}

impl OperationOwner {
    const fn root() -> Self {
        Self {
            fiber: ROOT_FIBER,
            episode: ROOT_EPISODE,
        }
    }

    pub(crate) const fn new(fiber: FiberId, episode: EpisodeId) -> Self {
        Self { fiber, episode }
    }
}

pub(crate) type ErasedEffectDriver = Pin<Box<dyn EffectDriver>>;
