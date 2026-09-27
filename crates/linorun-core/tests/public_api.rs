use std::{
    any::Any,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll, Waker},
};

use linorun_core::{
    ActivationKeySnapshot, BindingEventRef, BindingSnapshot, ComponentDescriptor, Context,
    DriverEvent, EffectDriver, EffectHandle, EffectSnapshot, EpisodeSnapshot, ErasedEndpoint,
    FiberHandle, FiberSnapshot, IntoEffectDriver, Requirement, RootContext, Runtime, RuntimeEvent,
    RuntimeSnapshot, Scope, ServiceContract, ServiceRef, effect,
};

struct Logger;

#[derive(Clone)]
struct LoggerClient {
    _reference: ServiceRef<Logger>,
}

struct LoggerEndpoint(u64);

impl ErasedEndpoint for LoggerEndpoint {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

impl ServiceContract for Logger {
    const ID: &'static str = "test/logger@1";

    type Provider = u64;
    type Intercept = Vec<&'static str>;
    type Client = LoggerClient;

    fn identity_intercept() -> Self::Intercept {
        Vec::new()
    }

    fn merge_intercept(base: &Self::Intercept, overlay: &Self::Intercept) -> Self::Intercept {
        base.iter().chain(overlay).copied().collect()
    }

    fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint> {
        Arc::new(LoggerEndpoint(provider))
    }

    fn client(reference: ServiceRef<Self>) -> Self::Client {
        LoggerClient {
            _reference: reference,
        }
    }
}

fn assert_clone_send_sync<T: Clone + Send + Sync>() {}

#[test]
fn downstream_contract_and_descriptor_shape_compiles() {
    assert_clone_send_sync::<Runtime>();
    assert_clone_send_sync::<RootContext>();
    assert_clone_send_sync::<Context>();
    assert_clone_send_sync::<Scope>();
    assert_clone_send_sync::<FiberHandle>();
    assert_clone_send_sync::<EffectHandle>();
    assert_clone_send_sync::<RuntimeSnapshot>();
    assert_clone_send_sync::<FiberSnapshot>();
    assert_clone_send_sync::<EpisodeSnapshot>();
    assert_clone_send_sync::<ActivationKeySnapshot>();
    assert_clone_send_sync::<BindingSnapshot>();
    assert_clone_send_sync::<EffectSnapshot>();
    assert_clone_send_sync::<RuntimeEvent>();
    assert_clone_send_sync::<BindingEventRef>();
    assert_clone_send_sync::<LoggerClient>();
    assert_clone_send_sync::<ServiceRef<Logger>>();

    let endpoint = Logger::erase(42);
    assert_eq!(
        endpoint
            .as_any()
            .downcast_ref::<LoggerEndpoint>()
            .unwrap()
            .0,
        42
    );

    let _descriptor = ComponentDescriptor::new::<()>(
        "test/consumer@1",
        vec![Requirement::new::<Logger>(Some(vec!["declared"]))],
        vec!["test/output@1"],
    );
}

#[test]
fn downstream_effect_adapter_obeys_poll_boundary() {
    let adapter = effect::sync(|| Ok(Some(effect::inverse(|| async { Ok(()) }))));
    let mut driver = Box::pin(adapter.into_driver());
    let mut context = TaskContext::from_waker(Waker::noop());

    let Poll::Ready(Ok(DriverEvent::Yield { inverse, has_next })) =
        EffectDriver::poll_step(Pin::as_mut(&mut driver), &mut context)
    else {
        panic!("sync adapter must yield its inverse")
    };

    assert!(!has_next);
    let mut cleanup = inverse();
    assert!(matches!(
        cleanup.as_mut().poll(&mut context),
        Poll::Ready(Ok(()))
    ));
}
