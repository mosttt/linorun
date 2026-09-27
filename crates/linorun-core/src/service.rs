use std::{
    any::{Any, TypeId},
    marker::PhantomData,
    sync::Arc,
};

use crate::{BindingId, EpisodeId, FiberId, ServiceId, ServiceTypeChannel, ServiceTypeMismatch};

pub trait ErasedEndpoint: Send + Sync + 'static {
    #[doc(hidden)]
    fn as_any(&self) -> &(dyn Any + Send + Sync);
}

pub trait ServiceContract: Sized + Send + Sync + 'static {
    const ID: &'static str;

    type Provider: Send + Sync + 'static;
    type Intercept: Clone + Send + Sync + 'static;
    type Client: Clone + Send + Sync + 'static;

    fn identity_intercept() -> Self::Intercept;

    fn merge_intercept(base: &Self::Intercept, overlay: &Self::Intercept) -> Self::Intercept;

    fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint>;

    #[doc(hidden)]
    fn client(reference: ServiceRef<Self>) -> Self::Client;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallContext<I> {
    pub caller: FiberId,
    pub caller_episode: EpisodeId,
    pub provider: FiberId,
    pub binding: BindingId,
    pub intercept: I,
}

pub struct ServiceRef<C: ServiceContract> {
    inner: Arc<ServiceRefInner<C>>,
}

struct ServiceRefInner<C: ServiceContract> {
    endpoint: Arc<dyn ErasedEndpoint>,
    call: CallContext<C::Intercept>,
    contract: PhantomData<fn() -> C>,
}

impl<C: ServiceContract> Clone for ServiceRef<C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<C: ServiceContract> ServiceRef<C> {
    pub(crate) fn new(endpoint: Arc<dyn ErasedEndpoint>, call: CallContext<C::Intercept>) -> Self {
        Self {
            inner: Arc::new(ServiceRefInner {
                endpoint,
                call,
                contract: PhantomData,
            }),
        }
    }

    #[doc(hidden)]
    pub fn dispatch_parts(&self) -> (Arc<dyn ErasedEndpoint>, CallContext<C::Intercept>) {
        (Arc::clone(&self.inner.endpoint), self.inner.call.clone())
    }
}

#[derive(Clone)]
pub(crate) struct InterceptValue {
    contract_type: TypeId,
    intercept_type: TypeId,
    value: Arc<dyn Any + Send + Sync>,
}

impl InterceptValue {
    pub(crate) fn new<C: ServiceContract>(value: C::Intercept) -> Self {
        Self {
            contract_type: TypeId::of::<C>(),
            intercept_type: TypeId::of::<C::Intercept>(),
            value: Arc::new(value),
        }
    }

    pub(crate) fn downcast_clone<T: Any + Clone>(&self) -> Option<T> {
        self.value.downcast_ref::<T>().cloned()
    }

    #[cfg(test)]
    pub(crate) fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.value.downcast_ref()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ContractVTable {
    contract_type: TypeId,
    intercept_type: TypeId,
    identity_intercept: fn() -> InterceptValue,
    merge_intercept: fn(
        &ServiceId,
        &InterceptValue,
        &InterceptValue,
    ) -> Result<InterceptValue, ServiceTypeMismatch>,
}

impl ContractVTable {
    pub(crate) fn of<C: ServiceContract>() -> Self {
        Self {
            contract_type: TypeId::of::<C>(),
            intercept_type: TypeId::of::<C::Intercept>(),
            identity_intercept: || InterceptValue::new::<C>(C::identity_intercept()),
            merge_intercept: merge_intercept::<C>,
        }
    }

    pub(crate) const fn contract_type(self) -> TypeId {
        self.contract_type
    }

    pub(crate) const fn intercept_type(self) -> TypeId {
        self.intercept_type
    }

    pub(crate) fn identity(self) -> InterceptValue {
        (self.identity_intercept)()
    }

    pub(crate) fn merge(
        self,
        service: &ServiceId,
        base: &InterceptValue,
        overlay: &InterceptValue,
    ) -> Result<InterceptValue, ServiceTypeMismatch> {
        (self.merge_intercept)(service, base, overlay)
    }
}

fn merge_intercept<C: ServiceContract>(
    service: &ServiceId,
    base: &InterceptValue,
    overlay: &InterceptValue,
) -> Result<InterceptValue, ServiceTypeMismatch> {
    validate_intercept::<C>(service, base)?;
    validate_intercept::<C>(service, overlay)?;

    let base = base
        .value
        .downcast_ref::<C::Intercept>()
        .expect("validated intercept type");
    let overlay = overlay
        .value
        .downcast_ref::<C::Intercept>()
        .expect("validated intercept type");

    Ok(InterceptValue::new::<C>(C::merge_intercept(base, overlay)))
}

fn validate_intercept<C: ServiceContract>(
    service: &ServiceId,
    value: &InterceptValue,
) -> Result<(), ServiceTypeMismatch> {
    let expected_contract = TypeId::of::<C>();
    if value.contract_type != expected_contract {
        return Err(ServiceTypeMismatch {
            service: service.clone(),
            channel: ServiceTypeChannel::Contract,
            expected: expected_contract,
            actual: value.contract_type,
        });
    }

    let expected_intercept = TypeId::of::<C::Intercept>();
    if value.intercept_type != expected_intercept {
        return Err(ServiceTypeMismatch {
            service: service.clone(),
            channel: ServiceTypeChannel::Intercept,
            expected: expected_intercept,
            actual: value.intercept_type,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct TestClient {
        _reference: ServiceRef<TestContract>,
    }

    struct TestContract;

    struct Endpoint;

    impl ErasedEndpoint for Endpoint {
        fn as_any(&self) -> &(dyn Any + Send + Sync) {
            self
        }
    }

    impl ServiceContract for TestContract {
        const ID: &'static str = "test/service@1";

        type Provider = ();
        type Intercept = Vec<&'static str>;
        type Client = TestClient;

        fn identity_intercept() -> Self::Intercept {
            Vec::new()
        }

        fn merge_intercept(base: &Self::Intercept, overlay: &Self::Intercept) -> Self::Intercept {
            base.iter().chain(overlay).copied().collect()
        }

        fn erase(_: Self::Provider) -> Arc<dyn ErasedEndpoint> {
            Arc::new(Endpoint)
        }

        fn client(reference: ServiceRef<Self>) -> Self::Client {
            TestClient {
                _reference: reference,
            }
        }
    }

    #[test]
    fn vtable_merges_typed_intercepts_in_order() {
        let service = ServiceId::new(TestContract::ID);
        let vtable = ContractVTable::of::<TestContract>();
        let declared = InterceptValue::new::<TestContract>(vec!["declared"]);
        let context = InterceptValue::new::<TestContract>(vec!["context"]);

        let merged = vtable.merge(&service, &declared, &context).unwrap();
        let value = merged.value.downcast_ref::<Vec<&'static str>>().unwrap();

        assert_eq!(value, &["declared", "context"]);
    }

    #[test]
    fn service_reference_keeps_endpoint_and_call_snapshot() {
        let call = CallContext {
            caller: FiberId::new(1),
            caller_episode: EpisodeId::new(2),
            provider: FiberId::new(3),
            binding: BindingId::new(4),
            intercept: vec!["trace"],
        };
        let reference = ServiceRef::<TestContract>::new(Arc::new(Endpoint), call);
        let (endpoint, call) = reference.dispatch_parts();

        assert!(endpoint.as_any().is::<Endpoint>());
        assert_eq!(call.provider, FiberId::new(3));
        assert_eq!(call.intercept, vec!["trace"]);
    }
}
