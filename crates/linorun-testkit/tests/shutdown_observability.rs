use std::{
    any::Any,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use linorun_core::{
    Activation, ActivationFailure, CleanupError, Component, ComponentContext, ComponentDescriptor,
    ErasedEndpoint, FiberHandle, FiberState, InverseSnapshotState, Requirement, RuntimeEventKind,
    RuntimeState, ServiceContract, ServiceRef, effect,
};
use linorun_testkit::{EventTag, Scenario, controlled, event_tag};

use futures_util::stream;

type ControlledCleanup = (linorun_testkit::Controlled<()>, &'static str);

struct UnitContract;
struct UnitEndpoint;

impl ErasedEndpoint for UnitEndpoint {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

impl ServiceContract for UnitContract {
    const ID: &'static str = "conformance/shutdown-unit@1";
    type Provider = ();
    type Intercept = ();
    type Client = ServiceRef<Self>;

    fn identity_intercept() -> Self::Intercept {}
    fn merge_intercept(_: &Self::Intercept, _: &Self::Intercept) -> Self::Intercept {}
    fn erase(_: Self::Provider) -> Arc<dyn ErasedEndpoint> {
        Arc::new(UnitEndpoint)
    }
    fn client(reference: ServiceRef<Self>) -> Self::Client {
        reference
    }
}

static CONSUMER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/shutdown-consumer@1",
        vec![Requirement::new::<UnitContract>(None)],
        vec![],
    )
});
static HISTORY_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>("conformance/history-errors@1", vec![], vec![])
});
static ORDERED_CHILD_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>("conformance/ordered-failure-child@1", vec![], vec![])
});
static ORDERED_PARENT_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>("conformance/ordered-failure-parent@1", vec![], vec![])
});

#[test]
fn s01_empty_runtime_has_synthetic_root_and_exact_shutdown_trace() {
    let scenario = Scenario::new();
    assert_eq!(
        scenario.run(scenario.runtime().shutdown()).unwrap(),
        linorun_core::ShutdownReport::default()
    );
    let events = scenario.trace().drain();
    assert_eq!(
        events
            .iter()
            .map(|event| event_tag(&event.kind))
            .collect::<Vec<_>>(),
        [
            EventTag::FiberInserted,
            EventTag::RuntimeShuttingDown,
            EventTag::LLeave,
            EventTag::LUnloadStarted,
            EventTag::LUnloadFinished,
            EventTag::RuntimeStopped,
        ]
    );
    assert!(events.iter().all(|event| !matches!(
        event.kind,
        RuntimeEventKind::LBegin { .. }
            | RuntimeEventKind::LFinish { .. }
            | RuntimeEventKind::FiberRemoved { .. }
    )));
    let snapshot = scenario.snapshot().unwrap();
    assert_eq!(snapshot.runtime_state, RuntimeState::Stopped);
    assert!(snapshot.quiescent);
    assert_eq!(snapshot.fibers.len(), 1);
    assert_eq!(snapshot.fibers[0].state, FiberState::Pending);
}

struct GatedConsumer {
    cleanup: Mutex<Option<linorun_testkit::Controlled<()>>>,
}

impl Component for GatedConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CONSUMER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let _service = context.context().service::<UnitContract>().unwrap();
        let cleanup = self.cleanup.lock().unwrap().take();
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                if let Some(cleanup) = cleanup {
                    cleanup.await;
                }
                Ok(())
            })))
        }))
    }
}

#[test]
fn r06_running_withdraw_snapshot_lists_only_unsettled_committed_watcher() {
    let scenario = Scenario::new();
    let binding = scenario
        .run(scenario.runtime().root().provide::<UnitContract>(()))
        .unwrap()
        .unwrap();
    let (release, cleanup) = controlled();
    let consumer = scenario
        .run(scenario.runtime().root().mount(
            GatedConsumer {
                cleanup: Mutex::new(Some(cleanup)),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(consumer.wait_settled()).unwrap();
    assert!(scenario.run(binding.dispose()).is_err());

    let snapshot = scenario.snapshot().unwrap();
    let effect = snapshot
        .effects
        .iter()
        .find(|effect| effect.id == binding.id())
        .unwrap();
    assert_eq!(effect.running_step, None);
    assert_eq!(effect.inverses.len(), 1);
    assert_eq!(effect.inverses[0].state, InverseSnapshotState::Running);
    assert_eq!(effect.inverses[0].waiting_on, [consumer.id()]);
    assert_eq!(
        snapshot
            .fibers
            .iter()
            .find(|fiber| fiber.id == consumer.id())
            .unwrap()
            .state,
        FiberState::Unloading
    );

    release.complete(()).unwrap();
    scenario.run(binding.dispose()).unwrap();
    assert_eq!(
        scenario.run(consumer.wait_settled()).unwrap(),
        FiberState::Pending
    );
    scenario.run(consumer.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn s05_root_binding_waits_for_consumer_unload_during_shutdown() {
    let scenario = Scenario::new();
    let binding = scenario
        .run(scenario.runtime().root().provide::<UnitContract>(()))
        .unwrap()
        .unwrap();
    let consumer = scenario
        .run(scenario.runtime().root().mount(
            GatedConsumer {
                cleanup: Mutex::new(None),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(consumer.wait_settled()).unwrap();
    let _ = scenario.trace().drain();
    scenario.run(scenario.runtime().shutdown()).unwrap();
    let events = scenario.trace().drain();
    let consumer_unloaded = events
        .iter()
        .position(|event| {
            matches!(event.kind, RuntimeEventKind::LUnloadFinished { fiber, .. } if fiber == consumer.id())
        })
        .unwrap();
    let binding_inverse = events
        .iter()
        .position(|event| {
            matches!(
                event.kind,
                RuntimeEventKind::InverseStarted {
                    owner: linorun_core::CleanupOwner::Effect(effect),
                    ..
                } if effect == binding.id()
            )
        })
        .unwrap();
    assert!(consumer_unloaded < binding_inverse);
    assert!(matches!(
        events.last().unwrap().kind,
        RuntimeEventKind::RuntimeStopped
    ));
}

struct HistoricalErrors {
    activations: Arc<AtomicUsize>,
}

impl Component for HistoricalErrors {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &HISTORY_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let activation = self.activations.fetch_add(1, Ordering::SeqCst);
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                if activation == 0 {
                    Err(CleanupError::user("old reload error"))
                } else {
                    Err(CleanupError::user("shutdown error"))
                }
            })))
        }))
    }
}

#[test]
fn s03_shutdown_report_excludes_cleanup_failures_from_old_reload() {
    let scenario = Scenario::new();
    let activations = Arc::new(AtomicUsize::new(0));
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            HistoricalErrors {
                activations: Arc::clone(&activations),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(fiber.wait_settled()).unwrap();
    scenario.run(fiber.restart()).unwrap().unwrap();
    scenario.run(fiber.wait_settled()).unwrap();
    assert_eq!(activations.load(Ordering::SeqCst), 2);

    let report = scenario.run(scenario.runtime().shutdown()).unwrap();
    assert_eq!(report.cleanup_errors.len(), 1);
    assert_eq!(report.cleanup_errors[0].cause.to_string(), "shutdown error");
    let errors: Vec<_> = scenario
        .trace()
        .drain()
        .into_iter()
        .filter_map(|event| match event.kind {
            RuntimeEventKind::InverseFinished {
                error: Some(error), ..
            } => Some(error.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(errors, ["old reload error", "shutdown error"]);
}

struct OrderedFailureChild {
    cleanups: Mutex<Option<Vec<ControlledCleanup>>>,
}

impl Component for OrderedFailureChild {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &ORDERED_CHILD_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let cleanups = self.cleanups.lock().unwrap().take().unwrap();
        Activation::Steps(Box::pin(stream::iter(cleanups.into_iter().map(
            |(release, message)| {
                Ok::<_, ActivationFailure>(effect::inverse(move || async move {
                    release.await;
                    Err(CleanupError::user(message))
                }))
            },
        ))))
    }
}

struct ParentWithOrderedFailureChildren {
    first: Mutex<Option<OrderedFailureChild>>,
    second: Mutex<Option<OrderedFailureChild>>,
}

impl Component for ParentWithOrderedFailureChildren {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &ORDERED_PARENT_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        let first = self.first.lock().unwrap().take().unwrap();
        let second = self.second.lock().unwrap().take().unwrap();
        Activation::Once(Box::pin(async move {
            scope
                .mount(&route, first, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            scope
                .mount(&route, second, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(None)
        }))
    }
}

#[test]
fn s07_subtree_dispose_report_preserves_cross_child_completion_order() {
    let scenario = Scenario::new();
    let (release_a1, cleanup_a1) = controlled();
    let (release_a2, cleanup_a2) = controlled();
    let (release_b1, cleanup_b1) = controlled();
    let parent = scenario
        .run(scenario.runtime().root().mount(
            ParentWithOrderedFailureChildren {
                first: Mutex::new(Some(OrderedFailureChild {
                    cleanups: Mutex::new(Some(vec![(cleanup_a1, "a1"), (cleanup_a2, "a2")])),
                })),
                second: Mutex::new(Some(OrderedFailureChild {
                    cleanups: Mutex::new(Some(vec![(cleanup_b1, "b1")])),
                })),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(parent.wait_settled()).unwrap(),
        FiberState::Active
    );

    assert!(scenario.run(parent.dispose()).is_err());
    let running = scenario
        .snapshot()
        .unwrap()
        .fibers
        .iter()
        .filter_map(|fiber| fiber.episode.as_ref())
        .flat_map(|episode| &episode.accumulator)
        .filter(|inverse| inverse.state == InverseSnapshotState::Running)
        .count();
    assert_eq!(running, 2);

    release_a2.complete(()).unwrap();
    scenario.executor().run_until_stalled();
    release_b1.complete(()).unwrap();
    scenario.executor().run_until_stalled();
    release_a1.complete(()).unwrap();

    let report = scenario.run(parent.dispose()).unwrap();
    assert_eq!(
        report
            .cleanup_errors
            .iter()
            .map(|failure| failure.cause.to_string())
            .collect::<Vec<_>>(),
        ["a2", "b1", "a1"]
    );
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct ParentExposingChild {
    child: Mutex<Option<OrderedFailureChild>>,
    handle: Arc<Mutex<Option<FiberHandle>>>,
}

impl Component for ParentExposingChild {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &ORDERED_PARENT_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        let child = self.child.lock().unwrap().take().unwrap();
        let handle = Arc::clone(&self.handle);
        Activation::Once(Box::pin(async move {
            let child = scope
                .mount(&route, child, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            *handle.lock().unwrap() = Some(child);
            Ok(None)
        }))
    }
}

#[test]
fn s06_subtree_dispose_report_starts_its_error_window_at_parent_retire() {
    let scenario = Scenario::new();
    let (release_after, cleanup_after) = controlled();
    let (release_before, cleanup_before) = controlled();
    let child_handle = Arc::new(Mutex::new(None));
    let parent = scenario
        .run(scenario.runtime().root().mount(
            ParentExposingChild {
                child: Mutex::new(Some(OrderedFailureChild {
                    cleanups: Mutex::new(Some(vec![
                        (cleanup_after, "after parent retire"),
                        (cleanup_before, "before parent retire"),
                    ])),
                })),
                handle: Arc::clone(&child_handle),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(parent.wait_settled()).unwrap(),
        FiberState::Active
    );
    let child = child_handle.lock().unwrap().clone().unwrap();

    assert!(scenario.run(child.dispose()).is_err());
    release_before.complete(()).unwrap();
    scenario.executor().run_until_stalled();
    assert!(scenario.run(parent.dispose()).is_err());
    release_after.complete(()).unwrap();

    let parent_report = scenario.run(parent.dispose()).unwrap();
    assert_eq!(
        parent_report
            .cleanup_errors
            .iter()
            .map(|failure| failure.cause.to_string())
            .collect::<Vec<_>>(),
        ["after parent retire"]
    );
    let child_report = scenario.run(child.dispose()).unwrap();
    assert_eq!(
        child_report
            .cleanup_errors
            .iter()
            .map(|failure| failure.cause.to_string())
            .collect::<Vec<_>>(),
        ["before parent retire", "after parent retire"]
    );
    scenario.run(scenario.runtime().shutdown()).unwrap();
}
