use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use linorun_core::{
    ActivationFailure, CallContext, ComponentContext, EpisodeSnapshot, FiberHandle, FiberState,
    component, service_contract, service_impl,
};
use linorun_testkit::{Controlled, Scenario, controlled};

service_contract! {
    contract Value {
        id: "withdrawal-episodes/value@1",
        methods: [async fn read() -> u32;],
    }
}

struct ValueService(u32);

#[service_impl]
impl ValueProvider for ValueService {
    async fn read(&self, _: CallContext<()>) -> u32 {
        self.0
    }
}

#[component(id = "withdrawal-episodes/provider@1", provides = [Value])]
async fn provider(context: ComponentContext, value: Arc<u32>) -> Result<(), ActivationFailure> {
    context
        .provide::<Value>(Arc::new(ValueService(*value)))
        .await?;
    Ok(())
}

struct ConsumerConfig {
    gates: Mutex<VecDeque<Controlled<()>>>,
    observations: Arc<Mutex<Vec<u32>>>,
}

#[component(id = "withdrawal-episodes/consumer@1", requires = [Value])]
async fn consumer(
    context: ComponentContext,
    config: Arc<ConsumerConfig>,
) -> Result<(), ActivationFailure> {
    let value = context.service::<Value>()?.read().await;
    config.observations.lock().unwrap().push(value);
    let gate = config.gates.lock().unwrap().pop_front();
    if let Some(gate) = gate {
        gate.await;
    }
    Ok(())
}

fn episode(scenario: &Scenario, fiber: &FiberHandle) -> EpisodeSnapshot {
    scenario
        .snapshot()
        .unwrap()
        .fibers
        .into_iter()
        .find(|snapshot| snapshot.id == fiber.id())
        .unwrap()
        .episode
        .unwrap()
}

#[test]
fn withdrawal_does_not_follow_a_consumer_into_a_later_episode() {
    let scenario = Scenario::new();
    let binding = scenario
        .run(
            scenario
                .runtime()
                .provide::<Value>(Arc::new(ValueService(1))),
        )
        .unwrap()
        .unwrap();
    let (release_first, first_gate) = controlled();
    let (release_later, later_gate) = controlled();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let first = scenario
        .run(scenario.runtime().mount(
            consumer(),
            ConsumerConfig {
                gates: Mutex::new(VecDeque::from([first_gate, later_gate])),
                observations: Arc::clone(&observations),
            },
        ))
        .unwrap()
        .unwrap();
    let (release_second, second_gate) = controlled();
    let second = scenario
        .run(scenario.runtime().mount(
            consumer(),
            ConsumerConfig {
                gates: Mutex::new(VecDeque::from([second_gate])),
                observations: Arc::new(Mutex::new(Vec::new())),
            },
        ))
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    let first_episode = episode(&scenario, &first);
    assert!(scenario.run(binding.dispose()).is_err());
    let replacement = scenario
        .run(scenario.runtime().mount(provider(), 2))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(replacement.wait_settled()).unwrap(),
        FiberState::Active
    );

    release_first.complete(()).unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(scenario.run(first.state()).unwrap(), FiberState::Loading);
    assert_ne!(episode(&scenario, &first).id, first_episode.id);
    assert_eq!(*observations.lock().unwrap(), [1, 2]);
    assert!(scenario.run(binding.dispose()).is_err());
    let snapshot = scenario.snapshot().unwrap();
    let inverse = &snapshot
        .effects
        .iter()
        .find(|effect| effect.id == binding.id())
        .unwrap()
        .inverses[0];
    assert_eq!(inverse.waiting_on, [second.id()]);

    // The second old Episode kept withdrawal pending while the first Fiber began its next one.
    release_second.complete(()).unwrap();
    assert!(
        scenario
            .run(binding.dispose())
            .expect("withdrawal must not wait for the replacement Episode")
            .cleanup_errors
            .is_empty()
    );
    assert_eq!(scenario.run(first.state()).unwrap(), FiberState::Loading);
    release_later.complete(()).unwrap();
    scenario.run(first.wait_settled()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn same_provider_republication_preserves_the_captured_episode_and_old_endpoint() {
    let scenario = Scenario::new();
    let binding = scenario
        .run(
            scenario
                .runtime()
                .provide::<Value>(Arc::new(ValueService(1))),
        )
        .unwrap()
        .unwrap();
    let (release, gate) = controlled();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let consumer = scenario
        .run(scenario.runtime().mount(
            consumer(),
            ConsumerConfig {
                gates: Mutex::new(VecDeque::from([gate])),
                observations: Arc::clone(&observations),
            },
        ))
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    let original = episode(&scenario, &consumer);
    assert!(scenario.run(binding.dispose()).is_err());
    let replacement = scenario
        .run(
            scenario
                .runtime()
                .provide::<Value>(Arc::new(ValueService(2))),
        )
        .unwrap()
        .unwrap();
    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(consumer.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert!(
        scenario
            .run(binding.dispose())
            .expect("the original Episode can reach an Active boundary with the old endpoint")
            .cleanup_errors
            .is_empty()
    );
    let continued = episode(&scenario, &consumer);
    assert_eq!(continued.id, original.id);
    assert_eq!(continued.committed_bindings, original.committed_bindings);
    assert_eq!(*observations.lock().unwrap(), [1]);

    scenario.run(replacement.dispose()).unwrap();
    assert_eq!(
        scenario.run(consumer.wait_settled()).unwrap(),
        FiberState::Pending
    );
    scenario.run(scenario.runtime().shutdown()).unwrap();
}
