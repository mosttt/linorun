use std::{
    any::{Any, TypeId},
    collections::{BTreeSet, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
};

use futures_core::Stream;

use crate::context::ComponentContext;
use crate::service::{ContractVTable, InterceptValue};
use crate::{ActivationFailure, ComponentId, ServiceContract, ServiceId};

const ROOT_COMPONENT_ID: &str = "linorun/root@1";

pub trait Component: Send + Sync + 'static {
    type Config: Send + Sync + 'static;

    fn descriptor(&self) -> &'static ComponentDescriptor;

    fn activate(&self, context: ComponentContext, config: Arc<Self::Config>) -> Activation;
}

pub enum Activation {
    Once(ActivationFuture),
    Steps(ActivationStream),
}

impl Activation {
    /// Runs an async setup function. Register each resource's cleanup through
    /// [`ComponentContext::on_cleanup`] as soon as setup succeeds.
    pub fn run<F>(setup: F) -> Self
    where
        F: Future<Output = Result<(), ActivationFailure>> + Send + 'static,
    {
        Self::Once(Box::pin(async move { setup.await.map(|()| None) }))
    }

    /// Runs one setup step that may return its own cleanup function.
    pub fn once<F>(setup: F) -> Self
    where
        F: Future<Output = Result<Option<crate::InverseFn>, ActivationFailure>> + Send + 'static,
    {
        Self::Once(Box::pin(setup))
    }

    /// Runs a stream of reversible setup steps, checking dependency changes between steps.
    pub fn steps<S>(setup: S) -> Self
    where
        S: Stream<Item = Result<crate::InverseFn, ActivationFailure>> + Send + 'static,
    {
        Self::Steps(Box::pin(setup))
    }
}

pub type ActivationFuture = Pin<
    Box<dyn Future<Output = Result<Option<crate::InverseFn>, ActivationFailure>> + Send + 'static>,
>;

pub type ActivationStream =
    Pin<Box<dyn Stream<Item = Result<crate::InverseFn, ActivationFailure>> + Send + 'static>>;

pub(crate) type ErasedConfig = Arc<dyn Any + Send + Sync>;

pub(crate) trait DynComponent: Send + Sync + 'static {
    fn descriptor(&self) -> &'static ComponentDescriptor;

    fn activate(
        &self,
        context: ComponentContext,
        config: ErasedConfig,
    ) -> Result<Activation, ActivationFailure>;
}

pub(crate) struct ComponentAdapter<C: Component> {
    component: C,
    descriptor: &'static ComponentDescriptor,
}

impl<C: Component> ComponentAdapter<C> {
    pub(crate) fn new(component: C, descriptor: &'static ComponentDescriptor) -> Self {
        Self {
            component,
            descriptor,
        }
    }
}

impl<C: Component> DynComponent for ComponentAdapter<C> {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        self.descriptor
    }

    fn activate(
        &self,
        context: ComponentContext,
        config: ErasedConfig,
    ) -> Result<Activation, ActivationFailure> {
        let config = Arc::downcast::<C::Config>(config)
            .map_err(|_| ActivationFailure::user("component config type mismatch"))?;
        Ok(self.component.activate(context, config))
    }
}

#[doc(hidden)]
pub struct Requirement {
    service: ServiceId,
    contract: ContractVTable,
    declared_intercept: Option<InterceptValue>,
}

impl Requirement {
    #[doc(hidden)]
    pub fn new<C: ServiceContract>(declared: Option<C::Intercept>) -> Self {
        Self {
            service: ServiceId::new(C::ID),
            contract: ContractVTable::of::<C>(),
            declared_intercept: declared.map(InterceptValue::new::<C>),
        }
    }

    pub(crate) fn service(&self) -> &ServiceId {
        &self.service
    }

    pub(crate) const fn contract(&self) -> ContractVTable {
        self.contract
    }

    pub(crate) fn declared_intercept(&self) -> Option<&InterceptValue> {
        self.declared_intercept.as_ref()
    }
}

/// Static identity, configuration type, and service declarations for a [`Component`].
///
/// Application code normally creates descriptors with `component_descriptor!` rather than
/// calling the low-level constructor directly.
///
/// ```
/// use linorun_core::ComponentDescriptor;
/// use linorun_macros::component_descriptor;
///
/// component_descriptor! {
///     static WORKER: () {
///         id: "docs/worker@1",
///         requires: [],
///         provides: [],
///     }
/// }
///
/// let _: &'static ComponentDescriptor = &WORKER;
/// ```
pub struct ComponentDescriptor {
    id: ComponentId,
    config_type: TypeId,
    requirements: Vec<Requirement>,
    provides: BTreeSet<ServiceId>,
}

impl ComponentDescriptor {
    pub(crate) fn root() -> Self {
        Self {
            id: ComponentId::new(ROOT_COMPONENT_ID),
            config_type: TypeId::of::<()>(),
            requirements: Vec::new(),
            provides: BTreeSet::new(),
        }
    }

    #[doc(hidden)]
    pub fn new<Config>(
        id: &'static str,
        requirements: Vec<Requirement>,
        provides: Vec<&'static str>,
    ) -> Self
    where
        Config: Send + Sync + 'static,
    {
        assert_ne!(
            id, ROOT_COMPONENT_ID,
            "linorun/root@1 is reserved for the synthetic root"
        );

        let mut requirement_ids = HashSet::with_capacity(requirements.len());
        for requirement in &requirements {
            assert!(
                requirement_ids.insert(requirement.service.clone()),
                "duplicate requirement {}",
                requirement.service
            );
        }

        let mut provided_ids = BTreeSet::new();
        for service in provides {
            let service = ServiceId::new(service);
            assert!(
                provided_ids.insert(service.clone()),
                "duplicate provided service {service}"
            );
        }

        Self {
            id: ComponentId::new(id),
            config_type: TypeId::of::<Config>(),
            requirements,
            provides: provided_ids,
        }
    }

    pub(crate) fn id(&self) -> &ComponentId {
        &self.id
    }

    pub(crate) const fn config_type(&self) -> TypeId {
        self.config_type
    }

    pub(crate) fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    pub(crate) fn provides(&self) -> &BTreeSet<ServiceId> {
        &self.provides
    }
}

#[cfg(test)]
mod tests {
    use std::{any::Any, sync::Arc};

    use crate::{ErasedEndpoint, ServiceRef};

    use super::*;

    struct Contract;
    struct Endpoint;

    impl ErasedEndpoint for Endpoint {
        fn as_any(&self) -> &(dyn Any + Send + Sync) {
            self
        }
    }

    impl ServiceContract for Contract {
        const ID: &'static str = "test/dependency@1";

        type Provider = ();
        type Intercept = ();
        type Client = ServiceRef<Self>;

        fn identity_intercept() -> Self::Intercept {}

        fn merge_intercept(_: &Self::Intercept, _: &Self::Intercept) -> Self::Intercept {}

        fn erase(_: Self::Provider) -> Arc<dyn ErasedEndpoint> {
            Arc::new(Endpoint)
        }

        fn client(reference: ServiceRef<Self>) -> Self::Client {
            reference
        }
    }

    #[test]
    fn descriptor_keeps_requirement_order_and_config_type() {
        let descriptor = ComponentDescriptor::new::<u32>(
            "test/component@1",
            vec![Requirement::new::<Contract>(None)],
            vec!["test/output@1"],
        );

        assert_eq!(descriptor.id().as_str(), "test/component@1");
        assert_eq!(descriptor.config_type(), TypeId::of::<u32>());
        assert_eq!(
            descriptor.requirements()[0].service().as_str(),
            Contract::ID
        );
        assert!(
            descriptor
                .provides()
                .contains(&ServiceId::new("test/output@1"))
        );
    }

    #[test]
    #[should_panic(expected = "duplicate requirement")]
    fn duplicate_requirement_is_a_definition_error() {
        let _ = ComponentDescriptor::new::<()>(
            "test/component@1",
            vec![
                Requirement::new::<Contract>(None),
                Requirement::new::<Contract>(None),
            ],
            Vec::new(),
        );
    }

    #[test]
    #[should_panic(expected = "reserved")]
    fn root_id_is_reserved() {
        let _ = ComponentDescriptor::new::<()>(ROOT_COMPONENT_ID, Vec::new(), Vec::new());
    }
}
