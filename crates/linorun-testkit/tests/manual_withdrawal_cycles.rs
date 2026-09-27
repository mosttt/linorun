use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use linorun_core::{
    ActivationFailure, CallContext, ComponentContext, FiberState, component, service_contract,
    service_impl,
};
use linorun_testkit::{Controlled, Scenario, controlled};

service_contract! {
    contract Left {
        id: "manual-withdrawal-cycle/left@1",
        methods: [async fn ping() -> ();],
    }
}

service_contract! {
    contract Right {
        id: "manual-withdrawal-cycle/right@1",
        methods: [async fn ping() -> ();],
    }
}

struct Service;

#[service_impl]
impl LeftProvider for Service {
    async fn ping(&self, _: CallContext<()>) {}
}

#[service_impl]
impl RightProvider for Service {
    async fn ping(&self, _: CallContext<()>) {}
}

#[component(id = "manual-withdrawal-cycle/source-left@1", provides = [Left])]
async fn source_left(context: ComponentContext) -> Result<(), ActivationFailure> {
    context.provide::<Left>(Arc::new(Service)).await?;
    Ok(())
}

#[component(id = "manual-withdrawal-cycle/source-right@1", provides = [Right])]
async fn source_right(context: ComponentContext) -> Result<(), ActivationFailure> {
    context.provide::<Right>(Arc::new(Service)).await?;
    Ok(())
}

struct ActivationConfig {
    context: Arc<Mutex<Option<ComponentContext>>>,
    gate: Mutex<Option<Controlled<()>>>,
    completed_steps: Arc<AtomicUsize>,
}

#[component(
    id = "manual-withdrawal-cycle/consumer-left@1",
    requires = [Left],
    provides = [Right],
)]
async fn consumer_left(
    context: ComponentContext,
    config: Arc<ActivationConfig>,
) -> Result<(), ActivationFailure> {
    *config.context.lock().unwrap() = Some(context);
    let gate = config.gate.lock().unwrap().take().unwrap();
    gate.await;
    config.completed_steps.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[component(
    id = "manual-withdrawal-cycle/consumer-right@1",
    requires = [Right],
    provides = [Left],
)]
async fn consumer_right(
    context: ComponentContext,
    config: Arc<ActivationConfig>,
) -> Result<(), ActivationFailure> {
    *config.context.lock().unwrap() = Some(context);
    let gate = config.gate.lock().unwrap().take().unwrap();
    gate.await;
    config.completed_steps.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[test]
fn manual_withdrawals_finish_without_a_cycle_between_uncommitted_watchers() {
    let scenario = Scenario::new();
    let source_left = scenario
        .run(scenario.runtime().root().mount(source_left(), ()))
        .unwrap()
        .unwrap();
    let source_right = scenario
        .run(scenario.runtime().root().mount(source_right(), ()))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(source_left.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert_eq!(
        scenario.run(source_right.wait_settled()).unwrap(),
        FiberState::Active
    );

    let (release_left, gate_left) = controlled();
    let (release_right, gate_right) = controlled();
    let context_left = Arc::new(Mutex::new(None));
    let context_right = Arc::new(Mutex::new(None));
    let completed_steps = Arc::new(AtomicUsize::new(0));
    let left = scenario
        .run(scenario.runtime().root().mount(
            consumer_left(),
            ActivationConfig {
                context: Arc::clone(&context_left),
                gate: Mutex::new(Some(gate_left)),
                completed_steps: Arc::clone(&completed_steps),
            },
        ))
        .unwrap()
        .unwrap();
    let right = scenario
        .run(scenario.runtime().root().mount(
            consumer_right(),
            ActivationConfig {
                context: Arc::clone(&context_right),
                gate: Mutex::new(Some(gate_right)),
                completed_steps: Arc::clone(&completed_steps),
            },
        ))
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(scenario.run(left.state()).unwrap(), FiberState::Loading);
    assert_eq!(scenario.run(right.state()).unwrap(), FiberState::Loading);

    // Each consumer committed an independent source, so their committed graph is acyclic.
    // Retirement frees both slots while the inertial activation steps are still running.
    let snapshot = scenario.snapshot().unwrap();
    for (consumer, source) in [
        (left.id(), source_left.id()),
        (right.id(), source_right.id()),
    ] {
        let episode = snapshot
            .fibers
            .iter()
            .find(|fiber| fiber.id == consumer)
            .unwrap()
            .episode
            .as_ref()
            .unwrap();
        assert_eq!(episode.committed_key.providers, vec![source]);
    }
    assert!(scenario.run(source_left.dispose()).is_err());
    assert!(scenario.run(source_right.dispose()).is_err());
    let context_left = context_left.lock().unwrap().clone().unwrap();
    let context_right = context_right.lock().unwrap().clone().unwrap();
    let left_binding = scenario
        .run(context_left.provide::<Right>(Arc::new(Service)))
        .unwrap()
        .unwrap();
    let right_binding = scenario
        .run(context_right.provide::<Left>(Arc::new(Service)))
        .unwrap()
        .unwrap();

    // Neither new binding was visible: both providers remain Loading. Waiting for every slot
    // watcher here used to create two Core-owned Withdraw inverses that waited on each other.
    let snapshot = scenario.snapshot().unwrap();
    for binding in &snapshot.bindings {
        if binding.owner_effect == left_binding.id() || binding.owner_effect == right_binding.id() {
            assert!(!binding.visible);
            assert!(binding.committed_consumers.is_empty());
        }
    }
    let _ = scenario.run(left_binding.dispose());
    let _ = scenario.run(right_binding.dispose());
    release_left.complete(()).unwrap();
    release_right.complete(()).unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(completed_steps.load(Ordering::SeqCst), 2);

    // All user Futures have returned, and this scenario has no user inverse. Core must finish.
    for binding in [&left_binding, &right_binding] {
        assert!(
            scenario
                .run(binding.dispose())
                .expect("Core withdrawal must progress after both user steps return")
                .cleanup_errors
                .is_empty()
        );
    }
    assert!(
        scenario
            .run(scenario.runtime().shutdown())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
    assert_eq!(scenario.run(left.state()).unwrap(), FiberState::Disposed);
    assert_eq!(scenario.run(right.state()).unwrap(), FiberState::Disposed);
}
