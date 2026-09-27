use std::{
    any::Any,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicI32, AtomicUsize, Ordering},
    },
};

use futures_channel::oneshot;
use linorun_core::{
    Activation, ActivationFailure, BindingId, Component, ComponentContext, ComponentDescriptor,
    Context, EffectHandle, ErasedEndpoint, FiberId, FiberState, ProvideError, Requirement, Scope,
    ServiceAccessError, ServiceContract, ServiceRef, effect,
};
use linorun_testkit::{Controlled, Scenario, controlled};

#[derive(Default)]
struct ValueData {
    value: AtomicI32,
}

impl ValueData {
    fn new(value: i32) -> Arc<Self> {
        Arc::new(Self {
            value: AtomicI32::new(value),
        })
    }
}

struct ValueContract;

#[derive(Clone)]
struct ValueClient(ServiceRef<ValueContract>);

struct ValueEndpoint(Arc<ValueData>);

impl ErasedEndpoint for ValueEndpoint {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

impl ServiceContract for ValueContract {
    const ID: &'static str = "conformance/value@1";

    type Provider = Arc<ValueData>;
    type Intercept = Vec<&'static str>;
    type Client = ValueClient;

    fn identity_intercept() -> Self::Intercept {
        Vec::new()
    }

    fn merge_intercept(base: &Self::Intercept, overlay: &Self::Intercept) -> Self::Intercept {
        base.iter().chain(overlay).copied().collect()
    }

    fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint> {
        Arc::new(ValueEndpoint(provider))
    }

    fn client(reference: ServiceRef<Self>) -> Self::Client {
        ValueClient(reference)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Observation {
    provider: FiberId,
    binding: BindingId,
    value: i32,
    intercept: Vec<&'static str>,
}

impl ValueClient {
    fn observe(&self) -> Observation {
        let (endpoint, call) = self.0.dispatch_parts();
        let endpoint = endpoint
            .as_any()
            .downcast_ref::<ValueEndpoint>()
            .expect("contract endpoint type is fixed");
        Observation {
            provider: call.provider,
            binding: call.binding,
            value: endpoint.0.value.load(Ordering::SeqCst),
            intercept: call.intercept,
        }
    }
}

struct OtherContract;
struct OtherEndpoint;

impl ErasedEndpoint for OtherEndpoint {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

impl ServiceContract for OtherContract {
    const ID: &'static str = "conformance/other@1";

    type Provider = ();
    type Intercept = ();
    type Client = ServiceRef<Self>;

    fn identity_intercept() -> Self::Intercept {}

    fn merge_intercept(_: &Self::Intercept, _: &Self::Intercept) -> Self::Intercept {}

    fn erase(_: Self::Provider) -> Arc<dyn ErasedEndpoint> {
        Arc::new(OtherEndpoint)
    }

    fn client(reference: ServiceRef<Self>) -> Self::Client {
        reference
    }
}

static PROVIDER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/value-provider@1",
        vec![],
        vec![ValueContract::ID],
    )
});
static CONSUMER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/value-consumer@1",
        vec![Requirement::new::<ValueContract>(Some(vec!["declared"]))],
        vec![],
    )
});
static JOIN_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/join-consumer@1",
        vec![
            Requirement::new::<ValueContract>(None),
            Requirement::new::<OtherContract>(None),
        ],
        vec![],
    )
});
static UNDECLARED_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("conformance/undeclared@1", vec![], vec![]));
static DUAL_PROVIDER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/dual-provider@1",
        vec![],
        vec![ValueContract::ID, OtherContract::ID],
    )
});
static OTHER_CONSUMER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/other-consumer@1",
        vec![Requirement::new::<OtherContract>(None)],
        vec![],
    )
});

struct ProviderPublication {
    effect: EffectHandle,
    context: Context,
    scope: Scope,
}

struct ProviderComponent {
    data: Arc<ValueData>,
    finish: Mutex<Option<Controlled<()>>>,
    publication: Mutex<Option<oneshot::Sender<ProviderPublication>>>,
    cleanup_trace: Arc<Mutex<Vec<&'static str>>>,
    cleanup_name: &'static str,
}

impl Component for ProviderComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PROVIDER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        let data = Arc::clone(&self.data);
        let finish = self.finish.lock().unwrap().take();
        let publication = self.publication.lock().unwrap().take();
        let cleanup_trace = Arc::clone(&self.cleanup_trace);
        let cleanup_name = self.cleanup_name;
        Activation::Once(Box::pin(async move {
            let binding = scope
                .provide::<ValueContract>(&route, data)
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            if let Some(publication) = publication {
                let _ = publication.send(ProviderPublication {
                    effect: binding.clone(),
                    context: route,
                    scope,
                });
            }
            if let Some(finish) = finish {
                finish.await;
            }
            Ok(Some(effect::inverse(move || async move {
                cleanup_trace.lock().unwrap().push(cleanup_name);
                Ok(())
            })))
        }))
    }
}

fn provider(
    value: i32,
    cleanup_name: &'static str,
    cleanup_trace: &Arc<Mutex<Vec<&'static str>>>,
) -> ProviderComponent {
    ProviderComponent {
        data: ValueData::new(value),
        finish: Mutex::new(None),
        publication: Mutex::new(None),
        cleanup_trace: Arc::clone(cleanup_trace),
        cleanup_name,
    }
}

struct ConsumerComponent {
    first_gate: Mutex<Option<Controlled<()>>>,
    observations: Arc<Mutex<Vec<(&'static str, Observation)>>>,
    activations: Arc<AtomicUsize>,
}

impl Component for ConsumerComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CONSUMER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let client = context.context().service::<ValueContract>().unwrap();
        self.activations.fetch_add(1, Ordering::SeqCst);
        self.observations
            .lock()
            .unwrap()
            .push(("start", client.observe()));
        let first_gate = self.first_gate.lock().unwrap().take();
        let observations = Arc::clone(&self.observations);
        Activation::Once(Box::pin(async move {
            if let Some(first_gate) = first_gate {
                first_gate.await;
            }
            Ok(Some(effect::inverse(move || async move {
                observations
                    .lock()
                    .unwrap()
                    .push(("cleanup", client.observe()));
                Ok(())
            })))
        }))
    }
}

fn consumer(
    gate: Option<Controlled<()>>,
    observations: &Arc<Mutex<Vec<(&'static str, Observation)>>>,
    activations: &Arc<AtomicUsize>,
) -> ConsumerComponent {
    ConsumerComponent {
        first_gate: Mutex::new(gate),
        observations: Arc::clone(observations),
        activations: Arc::clone(activations),
    }
}

struct JoinConsumer;

impl Component for JoinConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &JOIN_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        panic!("join consumer must stay pending while one requirement is missing")
    }
}

#[test]
fn f01_f02_pending_waits_and_loading_provider_is_not_visible() {
    let scenario = Scenario::new();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(AtomicUsize::new(0));
    let consumer = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(consumer(None, &observations, &activations), ()),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(consumer.wait_settled()).unwrap(),
        FiberState::Pending
    );

    let (release, finish) = controlled();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = scenario
        .run(scenario.runtime().root().mount(
            ProviderComponent {
                data: ValueData::new(7),
                finish: Mutex::new(Some(finish)),
                publication: Mutex::new(None),
                cleanup_trace: Arc::clone(&cleanup),
                cleanup_name: "provider",
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    let loading = scenario.snapshot().unwrap();
    assert_eq!(
        loading
            .fibers
            .iter()
            .find(|fiber| fiber.id == provider.id())
            .unwrap()
            .state,
        FiberState::Loading
    );
    assert_eq!(scenario.run(consumer.state()).unwrap(), FiberState::Pending);
    assert_eq!(loading.bindings.len(), 1);
    assert!(!loading.bindings[0].visible);

    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(provider.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert_eq!(
        scenario.run(consumer.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert_eq!(observations.lock().unwrap()[0].1.value, 7);
    scenario.run(consumer.dispose()).unwrap();
    scenario.run(provider.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn f04_loading_target_loss_diverts_after_boundary_and_uses_committed_endpoint() {
    let scenario = Scenario::new();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(provider(11, "P", &cleanup), ()),
        )
        .unwrap()
        .unwrap();
    scenario.run(provider.wait_settled()).unwrap();

    let (release, gate) = controlled();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(AtomicUsize::new(0));
    let consumer = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(consumer(Some(gate), &observations, &activations), ()),
        )
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(scenario.run(consumer.state()).unwrap(), FiberState::Loading);
    assert!(scenario.run(provider.dispose()).is_err());
    assert_eq!(scenario.run(consumer.state()).unwrap(), FiberState::Loading);

    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(consumer.wait_settled()).unwrap(),
        FiberState::Pending
    );
    scenario.executor().run_until_stalled();
    assert_eq!(
        scenario.run(provider.state()).unwrap(),
        FiberState::Disposed
    );
    let observations = observations.lock().unwrap();
    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].0, "start");
    assert_eq!(observations[1].0, "cleanup");
    assert_eq!(observations[0].1.binding, observations[1].1.binding);
    assert_eq!(observations[1].1.value, 11);
    drop(observations);
    scenario.run(consumer.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn f06_d02_loading_switches_provider_without_parallel_episode() {
    let scenario = Scenario::new();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider_p = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(provider(1, "P", &cleanup), ()),
        )
        .unwrap()
        .unwrap();
    scenario.run(provider_p.wait_settled()).unwrap();

    let (release, gate) = controlled();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(AtomicUsize::new(0));
    let consumer = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(consumer(Some(gate), &observations, &activations), ()),
        )
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    assert!(scenario.run(provider_p.dispose()).is_err());

    let provider_q = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(provider(2, "Q", &cleanup), ()),
        )
        .unwrap()
        .unwrap();
    scenario.run(provider_q.wait_settled()).unwrap();
    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(consumer.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert_eq!(activations.load(Ordering::SeqCst), 2);
    let observations = observations.lock().unwrap();
    assert_eq!(
        observations
            .iter()
            .map(|(phase, observation)| (*phase, observation.value))
            .collect::<Vec<_>>(),
        [("start", 1), ("cleanup", 1), ("start", 2)]
    );
    assert_ne!(observations[0].1.provider, observations[2].1.provider);
    drop(observations);
    assert_eq!(
        scenario.run(provider_p.state()).unwrap(),
        FiberState::Disposed
    );
    scenario.run(consumer.dispose()).unwrap();
    scenario.run(provider_q.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn f05_d09_same_provider_rebind_preserves_loading_episode_and_old_client() {
    let scenario = Scenario::new();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let (send_publication, receive_publication) = oneshot::channel();
    let provider = scenario
        .run(scenario.runtime().root().mount(
            ProviderComponent {
                data: ValueData::new(1),
                finish: Mutex::new(None),
                publication: Mutex::new(Some(send_publication)),
                cleanup_trace: Arc::clone(&cleanup),
                cleanup_name: "P",
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(provider.wait_settled()).unwrap();
    let publication = scenario.run(receive_publication).unwrap().unwrap();

    let (release, gate) = controlled();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(AtomicUsize::new(0));
    let consumer = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(consumer(Some(gate), &observations, &activations), ()),
        )
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(scenario.run(consumer.state()).unwrap(), FiberState::Loading);

    assert!(scenario.run(publication.effect.dispose()).is_err());
    let replacement = scenario
        .run(
            publication
                .scope
                .provide::<ValueContract>(&publication.context, ValueData::new(2)),
        )
        .unwrap()
        .unwrap();
    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(consumer.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert_eq!(activations.load(Ordering::SeqCst), 1);
    assert_eq!(observations.lock().unwrap().as_slice()[0].1.value, 1);
    assert_eq!(
        scenario.run(publication.effect.dispose()).unwrap(),
        linorun_core::EffectDisposeReport::default()
    );

    scenario.run(consumer.dispose()).unwrap();
    let observations = observations.lock().unwrap();
    assert_eq!(
        observations
            .iter()
            .map(|(phase, observation)| (*phase, observation.value))
            .collect::<Vec<_>>(),
        [("start", 1), ("cleanup", 1)]
    );
    drop(observations);
    scenario.run(replacement.dispose()).unwrap();
    scenario.run(provider.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn r02_target_churn_uses_one_inertial_episode_and_only_the_latest_provider() {
    let scenario = Scenario::new();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let (send_publication, receive_publication) = oneshot::channel();
    let provider_p = scenario
        .run(scenario.runtime().root().mount(
            ProviderComponent {
                data: ValueData::new(1),
                finish: Mutex::new(None),
                publication: Mutex::new(Some(send_publication)),
                cleanup_trace: Arc::clone(&cleanup),
                cleanup_name: "P",
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(provider_p.wait_settled()).unwrap();
    let publication = scenario.run(receive_publication).unwrap().unwrap();

    let (release, gate) = controlled();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(AtomicUsize::new(0));
    let consumer = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(consumer(Some(gate), &observations, &activations), ()),
        )
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();

    assert!(scenario.run(publication.effect.dispose()).is_err());
    let replacement = scenario
        .run(
            publication
                .scope
                .provide::<ValueContract>(&publication.context, ValueData::new(2)),
        )
        .unwrap()
        .unwrap();
    assert!(scenario.run(provider_p.dispose()).is_err());
    let provider_q = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(provider(3, "Q", &cleanup), ()),
        )
        .unwrap()
        .unwrap();
    scenario.run(provider_q.wait_settled()).unwrap();

    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(consumer.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert_eq!(activations.load(Ordering::SeqCst), 2);
    assert_eq!(
        observations
            .lock()
            .unwrap()
            .iter()
            .map(|(phase, observation)| (*phase, observation.value))
            .collect::<Vec<_>>(),
        [("start", 1), ("cleanup", 1), ("start", 3)]
    );
    scenario.executor().run_until_stalled();
    scenario.run(publication.effect.dispose()).unwrap();
    scenario.run(replacement.dispose()).unwrap();
    assert_eq!(
        scenario.run(provider_p.state()).unwrap(),
        FiberState::Disposed
    );
    scenario.run(consumer.dispose()).unwrap();
    scenario.run(provider_q.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn r03_restart_and_provider_change_commit_one_latest_revision_episode() {
    let scenario = Scenario::new();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider_p = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(provider(1, "P", &cleanup), ()),
        )
        .unwrap()
        .unwrap();
    scenario.run(provider_p.wait_settled()).unwrap();
    let (release, gate) = controlled();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(AtomicUsize::new(0));
    let consumer = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(consumer(Some(gate), &observations, &activations), ()),
        )
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();

    scenario.run(consumer.restart()).unwrap().unwrap();
    assert!(scenario.run(provider_p.dispose()).is_err());
    let provider_q = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(provider(2, "Q", &cleanup), ()),
        )
        .unwrap()
        .unwrap();
    scenario.run(provider_q.wait_settled()).unwrap();
    release.complete(()).unwrap();
    scenario.run(consumer.wait_settled()).unwrap();
    let snapshot = scenario.snapshot().unwrap();
    let consumer_snapshot = snapshot
        .fibers
        .iter()
        .find(|fiber| fiber.id == consumer.id())
        .unwrap();
    assert_eq!(consumer_snapshot.activation_revision, 1);
    assert_eq!(consumer_snapshot.state, FiberState::Active);
    assert_eq!(
        consumer_snapshot
            .episode
            .as_ref()
            .unwrap()
            .committed_key
            .revision,
        1
    );
    assert_eq!(activations.load(Ordering::SeqCst), 2);
    assert_eq!(observations.lock().unwrap().last().unwrap().1.value, 2);

    scenario.run(consumer.dispose()).unwrap();
    scenario.run(provider_q.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn d01_provider_value_mutation_does_not_change_target_or_reload_consumer() {
    let scenario = Scenario::new();
    let data = ValueData::new(3);
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = scenario
        .run(scenario.runtime().root().mount(
            ProviderComponent {
                data: Arc::clone(&data),
                finish: Mutex::new(None),
                publication: Mutex::new(None),
                cleanup_trace: Arc::clone(&cleanup),
                cleanup_name: "P",
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(provider.wait_settled()).unwrap();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(AtomicUsize::new(0));
    let consumer = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(consumer(None, &observations, &activations), ()),
        )
        .unwrap()
        .unwrap();
    scenario.run(consumer.wait_settled()).unwrap();
    let before = scenario.snapshot().unwrap();
    let before_episode = before
        .fibers
        .iter()
        .find(|fiber| fiber.id == consumer.id())
        .unwrap()
        .episode
        .as_ref()
        .unwrap()
        .id;
    data.value.store(9, Ordering::SeqCst);
    let after = scenario.snapshot().unwrap();
    let after_episode = after
        .fibers
        .iter()
        .find(|fiber| fiber.id == consumer.id())
        .unwrap()
        .episode
        .as_ref()
        .unwrap()
        .id;
    assert_eq!(before_episode, after_episode);
    assert_eq!(activations.load(Ordering::SeqCst), 1);
    scenario.run(consumer.dispose()).unwrap();
    assert_eq!(observations.lock().unwrap().last().unwrap().1.value, 9);
    scenario.run(provider.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn d08_duplicate_binding_is_atomic_and_d10_pending_watcher_is_not_a_dependency() {
    let scenario = Scenario::new();
    let first = scenario
        .run(
            scenario
                .runtime()
                .root()
                .provide::<ValueContract>(ValueData::new(1)),
        )
        .unwrap()
        .unwrap();
    let duplicate = scenario
        .run(
            scenario
                .runtime()
                .root()
                .provide::<ValueContract>(ValueData::new(2)),
        )
        .unwrap();
    assert!(matches!(duplicate, Err(ProvideError::DuplicateBinding)));
    let snapshot = scenario.snapshot().unwrap();
    assert_eq!(snapshot.bindings.len(), 1);
    assert_eq!(snapshot.bindings[0].owner_effect, first.id());
    scenario.run(first.dispose()).unwrap();

    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let provider = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(provider(4, "P", &cleanup), ()),
        )
        .unwrap()
        .unwrap();
    scenario.run(provider.wait_settled()).unwrap();
    let pending = scenario
        .run(scenario.runtime().root().mount(JoinConsumer, ()))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(pending.wait_settled()).unwrap(),
        FiberState::Pending
    );
    assert!(scenario.run(provider.dispose()).is_ok());
    assert_eq!(scenario.run(pending.state()).unwrap(), FiberState::Pending);
    scenario.run(pending.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct DualProvider {
    output: Mutex<Option<oneshot::Sender<(EffectHandle, EffectHandle)>>>,
}

impl Component for DualProvider {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DUAL_PROVIDER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        let output = self.output.lock().unwrap().take().unwrap();
        Activation::Once(Box::pin(async move {
            let value = scope
                .provide::<ValueContract>(&route, ValueData::new(8))
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            let other = scope
                .provide::<OtherContract>(&route, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            let _ = output.send((value, other));
            Ok(None)
        }))
    }
}

struct OtherConsumer;

impl Component for OtherConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &OTHER_CONSUMER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let _service = context.context().service::<OtherContract>().unwrap();
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

#[test]
fn d07_manual_withdrawal_only_affects_consumers_of_that_binding() {
    let scenario = Scenario::new();
    let (send_handles, receive_handles) = oneshot::channel();
    let provider = scenario
        .run(scenario.runtime().root().mount(
            DualProvider {
                output: Mutex::new(Some(send_handles)),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(provider.wait_settled()).unwrap();
    let (value_binding, other_binding) = scenario.run(receive_handles).unwrap().unwrap();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let activations = Arc::new(AtomicUsize::new(0));
    let value_consumer = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(consumer(None, &observations, &activations), ()),
        )
        .unwrap()
        .unwrap();
    let other_consumer = scenario
        .run(scenario.runtime().root().mount(OtherConsumer, ()))
        .unwrap()
        .unwrap();
    scenario.run(value_consumer.wait_settled()).unwrap();
    scenario.run(other_consumer.wait_settled()).unwrap();

    scenario.run(value_binding.dispose()).unwrap();
    assert_eq!(
        scenario.run(value_consumer.wait_settled()).unwrap(),
        FiberState::Pending
    );
    assert_eq!(
        scenario.run(other_consumer.state()).unwrap(),
        FiberState::Active
    );
    assert_eq!(scenario.run(provider.state()).unwrap(), FiberState::Active);
    let snapshot = scenario.snapshot().unwrap();
    assert_eq!(snapshot.bindings.len(), 1);
    assert_eq!(snapshot.bindings[0].owner_effect, other_binding.id());

    scenario.run(value_consumer.dispose()).unwrap();
    scenario.run(other_consumer.dispose()).unwrap();
    scenario.run(other_binding.dispose()).unwrap();
    scenario.run(provider.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct PublishingConsumer {
    output: Mutex<Option<oneshot::Sender<(Context, Observation)>>>,
    activations: Arc<AtomicUsize>,
}

struct OwnerPublication {
    context: Context,
    scope: Scope,
    observation: Observation,
}

struct PublishingOwner {
    output: Mutex<Option<oneshot::Sender<OwnerPublication>>>,
}

impl Component for PublishingOwner {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CONSUMER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let observation = context
            .context()
            .service::<ValueContract>()
            .unwrap()
            .observe();
        let _ = self
            .output
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(OwnerPublication {
                context: context.context().clone(),
                scope: context.scope().clone(),
                observation,
            });
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

impl Component for PublishingConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CONSUMER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        self.activations.fetch_add(1, Ordering::SeqCst);
        let observation = context
            .context()
            .service::<ValueContract>()
            .unwrap()
            .observe();
        let sender = self.output.lock().unwrap().take().unwrap();
        let _ = sender.send((context.context().clone(), observation));
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

#[test]
fn c01_c02_realms_partition_or_share_bindings_and_c03_intercept_does_not_reload() {
    let scenario = Scenario::new();
    let root = scenario.runtime().root();
    let tenant = root.create_realm();
    let tenant_root = root.isolate::<ValueContract>(tenant.clone()).unwrap();
    let default_binding = scenario
        .run(root.provide::<ValueContract>(ValueData::new(10)))
        .unwrap()
        .unwrap();
    let tenant_binding = scenario
        .run(tenant_root.provide::<ValueContract>(ValueData::new(20)))
        .unwrap()
        .unwrap();

    let activations = Arc::new(AtomicUsize::new(0));
    let (send_default, receive_default) = oneshot::channel();
    let default_consumer = scenario
        .run(root.mount(
            PublishingConsumer {
                output: Mutex::new(Some(send_default)),
                activations: Arc::clone(&activations),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(default_consumer.wait_settled()).unwrap();
    let (default_context, default_observation) = scenario.run(receive_default).unwrap().unwrap();

    let (send_tenant_a, receive_tenant_a) = oneshot::channel();
    let tenant_a = scenario
        .run(tenant_root.mount(
            PublishingConsumer {
                output: Mutex::new(Some(send_tenant_a)),
                activations: Arc::clone(&activations),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(tenant_a.wait_settled()).unwrap();
    let (_, tenant_observation_a) = scenario.run(receive_tenant_a).unwrap().unwrap();

    let shared_route = root.isolate::<ValueContract>(tenant).unwrap();
    let (send_tenant_b, receive_tenant_b) = oneshot::channel();
    let tenant_b = scenario
        .run(shared_route.mount(
            PublishingConsumer {
                output: Mutex::new(Some(send_tenant_b)),
                activations: Arc::clone(&activations),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(tenant_b.wait_settled()).unwrap();
    let (_, tenant_observation_b) = scenario.run(receive_tenant_b).unwrap().unwrap();

    assert_eq!(default_observation.value, 10);
    assert_eq!(tenant_observation_a.value, 20);
    assert_ne!(default_observation.binding, tenant_observation_a.binding);
    assert_eq!(tenant_observation_a.binding, tenant_observation_b.binding);

    let derived = default_context.intercept::<ValueContract>(vec!["late"]);
    let derived_observation = derived.service::<ValueContract>().unwrap().observe();
    assert_eq!(derived_observation.binding, default_observation.binding);
    assert_eq!(derived_observation.provider, default_observation.provider);
    assert_eq!(derived_observation.intercept, ["declared", "late"]);
    assert_eq!(activations.load(Ordering::SeqCst), 3);

    scenario.run(default_consumer.dispose()).unwrap();
    scenario.run(tenant_a.dispose()).unwrap();
    scenario.run(tenant_b.dispose()).unwrap();
    scenario.run(default_binding.dispose()).unwrap();
    scenario.run(tenant_binding.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn c13_isolate_keeps_current_client_but_routes_children_and_c14_can_reset_default() {
    let scenario = Scenario::new();
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let root = scenario.runtime().root();
    let tenant = root.create_realm();
    let tenant_root = root.isolate::<ValueContract>(tenant.clone()).unwrap();
    let provider_p = scenario
        .run(root.mount(provider(1, "P", &cleanup), ()))
        .unwrap()
        .unwrap();
    let provider_q = scenario
        .run(tenant_root.mount(provider(2, "Q", &cleanup), ()))
        .unwrap()
        .unwrap();
    scenario.run(provider_p.wait_settled()).unwrap();
    scenario.run(provider_q.wait_settled()).unwrap();

    let (send_owner, receive_owner) = oneshot::channel();
    let owner = scenario
        .run(root.mount(
            PublishingOwner {
                output: Mutex::new(Some(send_owner)),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(owner.wait_settled()).unwrap();
    let publication = scenario.run(receive_owner).unwrap().unwrap();
    assert_eq!(publication.observation.value, 1);

    let tenant_context = publication
        .context
        .isolate::<ValueContract>(tenant)
        .unwrap();
    let current = tenant_context.service::<ValueContract>().unwrap().observe();
    assert_eq!(current.binding, publication.observation.binding);
    assert_eq!(current.provider, publication.observation.provider);
    assert_eq!(current.value, 1);

    let child_activations = Arc::new(AtomicUsize::new(0));
    let (send_tenant_child, receive_tenant_child) = oneshot::channel();
    let tenant_child = scenario
        .run(publication.scope.mount(
            &tenant_context,
            PublishingConsumer {
                output: Mutex::new(Some(send_tenant_child)),
                activations: Arc::clone(&child_activations),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(tenant_child.wait_settled()).unwrap();
    let (_, tenant_observation) = scenario.run(receive_tenant_child).unwrap().unwrap();
    assert_eq!(tenant_observation.value, 2);
    assert_eq!(tenant_observation.provider, provider_q.id());

    let default_context = tenant_context
        .isolate::<ValueContract>(publication.context.default_realm())
        .unwrap();
    let (send_default_child, receive_default_child) = oneshot::channel();
    let default_child = scenario
        .run(publication.scope.mount(
            &default_context,
            PublishingConsumer {
                output: Mutex::new(Some(send_default_child)),
                activations: Arc::clone(&child_activations),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(default_child.wait_settled()).unwrap();
    let (_, default_observation) = scenario.run(receive_default_child).unwrap().unwrap();
    assert_eq!(default_observation.value, 1);
    assert_eq!(default_observation.provider, provider_p.id());
    assert_eq!(child_activations.load(Ordering::SeqCst), 2);

    scenario.run(tenant_child.dispose()).unwrap();
    scenario.run(default_child.dispose()).unwrap();
    scenario.run(owner.dispose()).unwrap();
    scenario.run(provider_q.dispose()).unwrap();
    scenario.run(provider_p.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();

    let post_stop_default = publication.context.default_realm();
    let _post_stop_realm = scenario.runtime().root().create_realm();
    publication
        .context
        .isolate::<ValueContract>(post_stop_default)
        .unwrap();
}

struct OtherValueContract;

impl ServiceContract for OtherValueContract {
    const ID: &'static str = ValueContract::ID;

    type Provider = ();
    type Intercept = ();
    type Client = ServiceRef<Self>;

    fn identity_intercept() -> Self::Intercept {}

    fn merge_intercept(_: &Self::Intercept, _: &Self::Intercept) -> Self::Intercept {}

    fn erase(_: Self::Provider) -> Arc<dyn ErasedEndpoint> {
        Arc::new(OtherEndpoint)
    }

    fn client(reference: ServiceRef<Self>) -> Self::Client {
        reference
    }
}

struct UndeclaredConsumer {
    result: Mutex<Option<oneshot::Sender<Result<ValueClient, ServiceAccessError>>>>,
}

static PARENT_UNDECLARED_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/parent-undeclared@1",
        vec![Requirement::new::<ValueContract>(None)],
        vec![],
    )
});

struct ParentMountsUndeclared {
    result: Mutex<Option<oneshot::Sender<Result<ValueClient, ServiceAccessError>>>>,
}

impl Component for ParentMountsUndeclared {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PARENT_UNDECLARED_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let _parent_client = context.context().service::<ValueContract>().unwrap();
        let scope = context.scope().clone();
        let route = context.context().clone();
        let result = self.result.lock().unwrap().take().unwrap();
        Activation::Once(Box::pin(async move {
            scope
                .mount(
                    &route,
                    UndeclaredConsumer {
                        result: Mutex::new(Some(result)),
                    },
                    (),
                )
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(None)
        }))
    }
}

impl Component for UndeclaredConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &UNDECLARED_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let result = context.context().service::<ValueContract>();
        let _ = self.result.lock().unwrap().take().unwrap().send(result);
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

#[test]
fn c06_undeclared_lookup_c09_foreign_realm_and_c10_contract_conflict_are_atomic() {
    let scenario = Scenario::new();
    let binding = scenario
        .run(
            scenario
                .runtime()
                .root()
                .provide::<ValueContract>(ValueData::new(1)),
        )
        .unwrap()
        .unwrap();
    let before = scenario.snapshot().unwrap();
    let conflict = scenario
        .run(scenario.runtime().root().provide::<OtherValueContract>(()))
        .unwrap();
    assert!(matches!(
        conflict,
        Err(ProvideError::ServiceTypeMismatch(_))
    ));
    let after = scenario.snapshot().unwrap();
    assert_eq!(before.bindings, after.bindings);
    assert_eq!(before.fibers, after.fibers);

    let (send_lookup, receive_lookup) = oneshot::channel();
    let undeclared = scenario
        .run(scenario.runtime().root().mount(
            UndeclaredConsumer {
                result: Mutex::new(Some(send_lookup)),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(undeclared.wait_settled()).unwrap();
    assert!(matches!(
        scenario.run(receive_lookup).unwrap().unwrap(),
        Err(ServiceAccessError::UndeclaredDependency)
    ));

    let (send_child_lookup, receive_child_lookup) = oneshot::channel();
    let parent = scenario
        .run(scenario.runtime().root().mount(
            ParentMountsUndeclared {
                result: Mutex::new(Some(send_child_lookup)),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(parent.wait_settled()).unwrap();
    assert!(matches!(
        scenario.run(receive_child_lookup).unwrap().unwrap(),
        Err(ServiceAccessError::UndeclaredDependency)
    ));

    let other = Scenario::new();
    let foreign = scenario.runtime().root().create_realm();
    let mismatch = other
        .runtime()
        .root()
        .isolate::<ValueContract>(foreign.clone())
        .err()
        .unwrap();
    assert_eq!(mismatch.realm, foreign);

    scenario.run(undeclared.dispose()).unwrap();
    scenario.run(parent.dispose()).unwrap();
    scenario.run(binding.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
    other.run(other.runtime().shutdown()).unwrap();
}
