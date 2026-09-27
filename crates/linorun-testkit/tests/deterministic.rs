use std::sync::{
    Arc, LazyLock, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use linorun_core::{
    Activation, Component, ComponentContext, ComponentDescriptor, FiberState, effect,
};
use linorun_testkit::{
    EventTag, ModelCommand, ModelLifecycle, ReferenceModel, Scenario, check_snapshot, controlled,
    event_tag,
};

static DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>("testkit/controlled@1", Vec::new(), Vec::new())
});

struct ControlledComponent {
    activation: Mutex<Option<linorun_testkit::Controlled<()>>>,
    cleanup_calls: Arc<AtomicUsize>,
}

impl Component for ControlledComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let activation = self.activation.lock().unwrap().take().unwrap();
        let cleanup_calls = Arc::clone(&self.cleanup_calls);
        Activation::Once(Box::pin(async move {
            activation.await;
            Ok(Some(effect::inverse(move || async move {
                cleanup_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })))
        }))
    }
}

#[test]
fn scenario_advances_only_when_the_test_releases_a_boundary() {
    let scenario = Scenario::new();
    let (release, activation) = controlled();
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            ControlledComponent {
                activation: Mutex::new(Some(activation)),
                cleanup_calls: Arc::clone(&cleanup_calls),
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
            .find(|candidate| candidate.id == fiber.id())
            .unwrap()
            .state,
        FiberState::Loading
    );
    let mut corrupted = loading.clone();
    corrupted
        .fibers
        .iter_mut()
        .find(|candidate| candidate.id == fiber.id())
        .unwrap()
        .mount_effect = None;
    assert!(check_snapshot(&corrupted).is_err());

    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(fiber.wait_settled()).unwrap(),
        FiberState::Active
    );
    scenario.run(fiber.dispose()).unwrap();
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
    scenario.run(scenario.runtime().shutdown()).unwrap();

    let events = scenario.trace().drain();
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    assert_eq!(
        event_tag(&events.last().unwrap().kind),
        EventTag::RuntimeStopped
    );
}

#[test]
fn reference_model_encodes_every_lifecycle_transition() {
    let scenario = Scenario::new();
    let (release, activation) = controlled();
    let runtime_fiber = scenario
        .run(scenario.runtime().root().mount(
            ControlledComponent {
                activation: Mutex::new(Some(activation)),
                cleanup_calls: Arc::new(AtomicUsize::new(0)),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    release.complete(()).unwrap();
    scenario.run(runtime_fiber.wait_settled()).unwrap();
    let runtime_snapshot = scenario.snapshot().unwrap();
    let root = runtime_snapshot
        .fibers
        .iter()
        .find(|fiber| fiber.parent.is_none())
        .unwrap()
        .id;
    let child = runtime_fiber.id();
    let episode = runtime_snapshot
        .fibers
        .iter()
        .find(|fiber| fiber.id == child)
        .unwrap()
        .episode
        .as_ref()
        .unwrap()
        .id;

    let mut model = ReferenceModel::new();
    model
        .apply(ModelCommand::InsertFiber {
            fiber: root,
            parent: None,
        })
        .unwrap();
    model
        .apply(ModelCommand::InsertFiber {
            fiber: child,
            parent: Some(root),
        })
        .unwrap();
    model
        .apply(ModelCommand::Begin {
            fiber: child,
            episode,
        })
        .unwrap();
    model.apply(ModelCommand::Finish { fiber: child }).unwrap();
    assert_eq!(
        model
            .snapshot()
            .fibers
            .iter()
            .find(|fiber| fiber.id == child)
            .unwrap()
            .lifecycle,
        ModelLifecycle::Active { episode }
    );
    model.apply(ModelCommand::Leave { fiber: child }).unwrap();
    model
        .apply(ModelCommand::FinishUnload { fiber: child })
        .unwrap();

    model
        .apply(ModelCommand::Begin {
            fiber: child,
            episode,
        })
        .unwrap();
    model
        .apply(ModelCommand::Iter {
            fiber: child,
            episode,
        })
        .unwrap();
    model.apply(ModelCommand::Divert { fiber: child }).unwrap();
    model
        .apply(ModelCommand::FinishUnload { fiber: child })
        .unwrap();

    model
        .apply(ModelCommand::Begin {
            fiber: child,
            episode,
        })
        .unwrap();
    model.apply(ModelCommand::Raise { fiber: child }).unwrap();
    model
        .apply(ModelCommand::FinishUnload { fiber: child })
        .unwrap();
    assert_eq!(
        model
            .snapshot()
            .fibers
            .iter()
            .find(|fiber| fiber.id == child)
            .unwrap()
            .lifecycle,
        ModelLifecycle::Failed
    );

    model
        .apply(ModelCommand::Begin {
            fiber: child,
            episode,
        })
        .unwrap();
    model.apply(ModelCommand::Finish { fiber: child }).unwrap();
    model.apply(ModelCommand::Leave { fiber: child }).unwrap();
    model
        .apply(ModelCommand::FinishUnload { fiber: child })
        .unwrap();
    model.apply(ModelCommand::Retire { fiber: child }).unwrap();
    model.apply(ModelCommand::Remove { fiber: child }).unwrap();
    assert_eq!(model.snapshot().fibers.len(), 1);

    scenario.run(runtime_fiber.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn reference_model_enforces_global_fiber_lifo_and_serial_inverse_execution() {
    let scenario = Scenario::new();
    let root_context = scenario.runtime().root();
    let first = scenario
        .run(root_context.effect(
            "first",
            effect::sync(|| Ok::<_, linorun_core::EffectError>(None)),
        ))
        .unwrap()
        .unwrap();
    let second = scenario
        .run(root_context.effect(
            "second",
            effect::sync(|| Ok::<_, linorun_core::EffectError>(None)),
        ))
        .unwrap()
        .unwrap();
    scenario.run(first.wait_forward()).unwrap().unwrap();
    scenario.run(second.wait_forward()).unwrap().unwrap();

    let root = scenario
        .snapshot()
        .unwrap()
        .fibers
        .into_iter()
        .find(|fiber| fiber.parent.is_none())
        .unwrap()
        .id;
    let mut model = ReferenceModel::new();
    model
        .apply(ModelCommand::InsertFiber {
            fiber: root,
            parent: None,
        })
        .unwrap();
    model
        .apply(ModelCommand::StartEffect {
            effect: first.id(),
            fiber: root,
        })
        .unwrap();
    let before_duplicate = model.snapshot();
    assert!(
        model
            .apply(ModelCommand::StartEffect {
                effect: first.id(),
                fiber: root,
            })
            .is_err()
    );
    assert_eq!(model.snapshot(), before_duplicate);
    model
        .apply(ModelCommand::StartEffect {
            effect: second.id(),
            fiber: root,
        })
        .unwrap();
    model
        .apply(ModelCommand::YieldInverse {
            effect: first.id(),
            seq: 1,
        })
        .unwrap();
    for seq in [2, 3] {
        model
            .apply(ModelCommand::YieldInverse {
                effect: second.id(),
                seq,
            })
            .unwrap();
    }
    model
        .apply(ModelCommand::RequestEffectDispose { effect: first.id() })
        .unwrap();
    model
        .apply(ModelCommand::RequestEffectDispose {
            effect: second.id(),
        })
        .unwrap();

    assert!(
        model
            .apply(ModelCommand::StartInverse {
                effect: first.id(),
                seq: 1,
            })
            .is_err(),
        "the scheduler must select the highest requested sequence"
    );
    model
        .apply(ModelCommand::StartInverse {
            effect: second.id(),
            seq: 3,
        })
        .unwrap();
    assert!(
        model
            .apply(ModelCommand::StartInverse {
                effect: second.id(),
                seq: 3,
            })
            .is_err(),
        "a fiber cannot run two inverse tasks at once"
    );
    model
        .apply(ModelCommand::FinishInverse {
            effect: second.id(),
            seq: 3,
        })
        .unwrap();
    model
        .apply(ModelCommand::StartInverse {
            effect: second.id(),
            seq: 2,
        })
        .unwrap();
    model
        .apply(ModelCommand::FinishInverse {
            effect: second.id(),
            seq: 2,
        })
        .unwrap();
    model
        .apply(ModelCommand::FinishEffect {
            effect: second.id(),
        })
        .unwrap();
    model
        .apply(ModelCommand::StartInverse {
            effect: first.id(),
            seq: 1,
        })
        .unwrap();
    model
        .apply(ModelCommand::FinishInverse {
            effect: first.id(),
            seq: 1,
        })
        .unwrap();
    model
        .apply(ModelCommand::FinishEffect { effect: first.id() })
        .unwrap();
    assert!(model.snapshot().effects.is_empty());

    scenario.run(first.dispose()).unwrap();
    scenario.run(second.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}
