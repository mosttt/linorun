use std::{any::TypeId, future::Future, sync::Arc};

use crate::route::ContextRoute;
use crate::runtime::{ErasedEffectDriver, OperationOwner, RuntimeHandle};
use crate::service::ContractVTable;
use crate::topology::CommittedView;
use crate::{
    CallContext, RealmId, RealmMismatch, ServiceAccessError, ServiceContract, ServiceId,
    ServiceRef, ServiceTypeChannel, ServiceTypeMismatch,
};
use crate::{CleanupError, EpisodeId, FiberId};
use crate::{
    Component, EffectHandle, EffectStartError, IntoEffectDriver, MountError, ProvideError,
};

#[derive(Clone)]
pub struct Context {
    runtime: RuntimeHandle,
    fiber: FiberId,
    episode: EpisodeId,
    route: Arc<ContextRoute>,
    committed: Arc<CommittedView>,
}

impl Context {
    pub(crate) fn new(
        runtime: RuntimeHandle,
        fiber: FiberId,
        episode: EpisodeId,
        route: Arc<ContextRoute>,
        committed: Arc<CommittedView>,
    ) -> Self {
        Self {
            runtime,
            fiber,
            episode,
            route,
            committed,
        }
    }

    pub fn default_realm(&self) -> RealmId {
        self.runtime.default_realm()
    }

    pub fn service<C: ServiceContract>(&self) -> Result<C::Client, ServiceAccessError> {
        let service = ServiceId::new(C::ID);
        let entry = self
            .committed
            .entry_for(&service)
            .ok_or(ServiceAccessError::UndeclaredDependency)?;
        let expected = TypeId::of::<C>();
        if entry.contract_type() != expected {
            return Err(ServiceAccessError::ServiceTypeMismatch(
                ServiceTypeMismatch {
                    service,
                    channel: ServiceTypeChannel::Contract,
                    expected,
                    actual: entry.contract_type(),
                },
            ));
        }

        let intercept = self
            .route
            .effective_intercept(
                &service,
                ContractVTable::of::<C>(),
                entry.declared_intercept(),
            )
            .map_err(ServiceAccessError::ServiceTypeMismatch)?;
        let intercept = intercept
            .downcast_clone::<C::Intercept>()
            .expect("validated ServiceContract intercept invariant");
        let reference = ServiceRef::new(
            entry.endpoint(),
            CallContext {
                caller: self.fiber,
                caller_episode: self.episode,
                provider: entry.provider(),
                binding: entry.binding(),
                intercept,
            },
        );
        Ok(C::client(reference))
    }

    pub fn isolate<C: ServiceContract>(&self, realm: RealmId) -> Result<Self, RealmMismatch> {
        let route = self.route.isolate::<C>(self.runtime.runtime_tag(), realm)?;
        Ok(Self {
            route,
            ..self.clone()
        })
    }

    pub fn intercept<C: ServiceContract>(&self, value: C::Intercept) -> Self {
        Self {
            route: self.route.intercept::<C>(value),
            ..self.clone()
        }
    }

    pub(crate) fn route(&self) -> Arc<ContextRoute> {
        Arc::clone(&self.route)
    }

    fn belongs_to(&self, scope: &Scope) -> bool {
        self.runtime.same_runtime(&scope.runtime)
            && self.fiber == scope.fiber
            && self.episode == scope.episode
    }
}

#[derive(Clone)]
pub struct Scope {
    runtime: RuntimeHandle,
    fiber: FiberId,
    episode: EpisodeId,
}

impl Scope {
    pub(crate) fn new(runtime: RuntimeHandle, fiber: FiberId, episode: EpisodeId) -> Self {
        Self {
            runtime,
            fiber,
            episode,
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
        let driver: ErasedEffectDriver = Box::pin(effect.into_driver());
        self.runtime
            .start_effect(
                OperationOwner::new(self.fiber, self.episode),
                label.into(),
                driver,
            )
            .await
    }

    /// Registers cleanup and waits until the runtime has recorded it.
    /// An already running activation or effect step may finish registering cleanup during
    /// disposal or shutdown. An expired episode cannot register more cleanup.
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
            .register_cleanup(
                OperationOwner::new(self.fiber, self.episode),
                label.into(),
                cleanup,
            )
            .await
    }

    pub async fn provide<C>(
        &self,
        context: &Context,
        provider: C::Provider,
    ) -> Result<EffectHandle, ProvideError>
    where
        C: ServiceContract,
    {
        self.runtime
            .provide::<C>(
                OperationOwner::new(self.fiber, self.episode),
                context.route(),
                provider,
                false,
                context.belongs_to(self),
            )
            .await
    }

    pub async fn mount<C>(
        &self,
        context: &Context,
        component: C,
        config: C::Config,
    ) -> Result<crate::FiberHandle, MountError>
    where
        C: Component,
    {
        self.runtime
            .mount_component(
                OperationOwner::new(self.fiber, self.episode),
                context.route(),
                component,
                config,
                context.belongs_to(self),
            )
            .await
    }
}

/// The services and resource operations available to one running component.
///
/// Use [`service`](Self::service) for declared dependencies and
/// [`on_cleanup`](Self::on_cleanup) to release resources when the component stops.
/// Derived contexts keep the same resource owner while changing service routing or metadata.
#[derive(Clone)]
pub struct ComponentContext {
    context: Context,
    scope: Scope,
}

impl ComponentContext {
    pub(crate) fn new(context: Context, scope: Scope) -> Self {
        Self { context, scope }
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Returns the typed client for a service declared in this component's requirements.
    pub fn service<C: ServiceContract>(&self) -> Result<C::Client, ServiceAccessError> {
        self.context.service::<C>()
    }

    /// Publishes a declared service. It is withdrawn automatically when the component stops.
    pub async fn provide<C: ServiceContract>(
        &self,
        provider: C::Provider,
    ) -> Result<EffectHandle, ProvideError> {
        self.scope.provide::<C>(&self.context, provider).await
    }

    /// Mounts a child using this context's routing and metadata.
    pub async fn mount<C: Component>(
        &self,
        component: C,
        config: C::Config,
    ) -> Result<crate::FiberHandle, MountError> {
        self.scope.mount(&self.context, component, config).await
    }

    /// Starts an independently disposable operation, including iterative effects.
    pub async fn effect<E: IntoEffectDriver>(
        &self,
        label: impl Into<Arc<str>>,
        effect: E,
    ) -> Result<EffectHandle, EffectStartError> {
        self.scope.effect(label, effect).await
    }

    /// Records how to release a resource and waits for registration before returning.
    ///
    /// Cleanup runs automatically on stop, restart, or failed setup, in reverse registration
    /// order. The returned handle also allows early cleanup. The callback runs at most once;
    /// it must finish any asynchronous resource shutdown before returning.
    /// An already running activation or effect step may finish registering cleanup during
    /// disposal or shutdown. An expired episode cannot register more cleanup.
    pub async fn on_cleanup<F, Fut>(
        &self,
        label: impl Into<Arc<str>>,
        cleanup: F,
    ) -> Result<EffectHandle, EffectStartError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CleanupError>> + Send + 'static,
    {
        self.scope.on_cleanup(label, cleanup).await
    }

    /// Routes future child mounts and service publications through the given service realm.
    /// Existing clients and this component's committed dependencies do not change.
    pub fn isolate<C: ServiceContract>(&self, realm: RealmId) -> Result<Self, RealmMismatch> {
        Ok(Self {
            context: self.context.isolate::<C>(realm)?,
            scope: self.scope.clone(),
        })
    }

    /// Adds call metadata for new clients and child components, without restarting anything.
    pub fn intercept<C: ServiceContract>(&self, value: C::Intercept) -> Self {
        Self {
            context: self.context.intercept::<C>(value),
            scope: self.scope.clone(),
        }
    }

    /// Returns the default service realm, for explicitly resetting a derived route.
    pub fn default_realm(&self) -> RealmId {
        self.context.default_realm()
    }
}
