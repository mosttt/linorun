use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use futures_channel::oneshot;
use futures_util::{future, stream};
use linorun_core::{
    Activation, CleanupError, Component, ComponentContext, ComponentDescriptor,
    EffectForwardOutcome, EffectStartError, FiberState, Scope, effect,
};
use linorun_testkit::{Controlled, Scenario, controlled};

static SCOPE_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("conformance/scope@1", vec![], vec![]));
static LIFO_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("conformance/lifo@1", vec![], vec![]));

struct PublishScope {
    output: Mutex<Option<oneshot::Sender<Scope>>>,
}

impl Component for PublishScope {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &SCOPE_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let _ = self
            .output
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(context.scope().clone());
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

fn active_scope(scenario: &Scenario) -> (linorun_core::FiberHandle, Scope) {
    let (send_scope, receive_scope) = oneshot::channel();
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            PublishScope {
                output: Mutex::new(Some(send_scope)),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(fiber.wait_settled()).unwrap(),
        FiberState::Active
    );
    let scope = scenario.run(receive_scope).unwrap().unwrap();
    (fiber, scope)
}

#[test]
fn r07_active_effect_step_obeys_inertia_before_fiber_cleanup() {
    let scenario = Scenario::new();
    let (fiber, scope) = active_scope(&scenario);
    let (release, forward) = controlled();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let cleanup_trace = Arc::clone(&trace);
    let effect = scenario
        .run(scope.effect(
            "in-flight",
            effect::once(move || async move {
                forward.await;
                Ok(Some(effect::inverse(move || async move {
                    cleanup_trace.lock().unwrap().push("cleanup");
                    Ok(())
                })))
            }),
        ))
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(
        scenario.run(fiber.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert!(!scenario.snapshot().unwrap().quiescent);

    assert!(scenario.run(fiber.dispose()).is_err());
    assert_eq!(scenario.run(fiber.state()).unwrap(), FiberState::Unloading);
    assert!(matches!(
        scenario
            .run(scope.effect(
                "late",
                effect::sync(|| Ok::<_, linorun_core::EffectError>(None)),
            ))
            .unwrap(),
        Err(EffectStartError::InactiveScope)
    ));
    assert!(trace.lock().unwrap().is_empty());

    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(effect.wait_forward()).unwrap().unwrap(),
        EffectForwardOutcome::Disposed
    );
    assert_eq!(scenario.run(fiber.dispose()).unwrap().cleanup_errors, []);
    assert_eq!(trace.lock().unwrap().as_slice(), ["cleanup"]);
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn r11_dispose_before_first_executor_poll_still_lands_one_driver_step() {
    let scenario = Scenario::new();
    let polls = Arc::new(AtomicUsize::new(0));
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let observed_polls = Arc::clone(&polls);
    let first_cleanup = Arc::clone(&cleanup_calls);
    let second_cleanup = Arc::clone(&cleanup_calls);
    let effect = scenario
        .run(scenario.runtime().root().effect(
            "queued-driver",
            effect::iter(move || {
                observed_polls.fetch_add(1, Ordering::SeqCst);
                [
                    Ok(effect::inverse(move || async move {
                        first_cleanup.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })),
                    Ok(effect::inverse(move || async move {
                        second_cleanup.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })),
                ]
            }),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 0);

    let report = scenario.run(effect.dispose()).unwrap();
    assert_eq!(report, linorun_core::EffectDisposeReport::default());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        scenario.run(effect.wait_forward()).unwrap().unwrap(),
        EffectForwardOutcome::Disposed
    );
    let events = scenario.trace().drain();
    let yields = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                linorun_core::RuntimeEventKind::EffectYielded {
                    effect: id,
                    has_next: true,
                    ..
                } if id == effect.id()
            )
        })
        .count();
    assert_eq!(yields, 1);
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

fn gated_inverse(
    name: &'static str,
    gate: Option<Controlled<()>>,
    trace: Arc<Mutex<Vec<&'static str>>>,
) -> linorun_core::InverseFn {
    effect::inverse(move || async move {
        trace.lock().unwrap().push(match name {
            "A" => "A-start",
            "B" => "B-start",
            _ => "unknown-start",
        });
        if let Some(gate) = gate {
            gate.await;
        }
        trace.lock().unwrap().push(match name {
            "A" => "A-finish",
            "B" => "B-finish",
            _ => "unknown-finish",
        });
        Ok(())
    })
}

#[test]
fn r08_manual_cleanup_and_fiber_cleanup_merge_without_overlap() {
    let scenario = Scenario::new();
    let (fiber, scope) = active_scope(&scenario);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (release_a, gate_a) = controlled();
    let a_trace = Arc::clone(&trace);
    let effect_a = scenario
        .run(scope.effect(
            "A",
            effect::sync(move || Ok(Some(gated_inverse("A", Some(gate_a), a_trace)))),
        ))
        .unwrap()
        .unwrap();
    scenario.run(effect_a.wait_forward()).unwrap().unwrap();

    let b_trace = Arc::clone(&trace);
    let effect_b = scenario
        .run(scope.effect(
            "B",
            effect::sync(move || Ok(Some(gated_inverse("B", None, b_trace)))),
        ))
        .unwrap()
        .unwrap();
    scenario.run(effect_b.wait_forward()).unwrap().unwrap();

    assert!(scenario.run(effect_a.dispose()).is_err());
    assert_eq!(trace.lock().unwrap().as_slice(), ["A-start"]);
    assert!(scenario.run(fiber.dispose()).is_err());
    assert_eq!(trace.lock().unwrap().as_slice(), ["A-start"]);

    release_a.complete(()).unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["A-start", "A-finish", "B-start", "B-finish"]
    );
    assert_eq!(
        scenario.run(effect_a.dispose()).unwrap(),
        linorun_core::EffectDisposeReport::default()
    );
    assert_eq!(
        scenario.run(effect_b.dispose()).unwrap(),
        linorun_core::EffectDisposeReport::default()
    );
    assert_eq!(
        scenario.run(fiber.dispose()).unwrap(),
        linorun_core::DisposeReport::default()
    );
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn r05_lifo_cleanup_is_serial_even_when_lower_entries_are_ready() {
    let scenario = Scenario::new();
    let (fiber, scope) = active_scope(&scenario);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let a_trace = Arc::clone(&trace);
    let effect_a = scenario
        .run(scope.effect(
            "A",
            effect::sync(move || Ok(Some(gated_inverse("A", None, a_trace)))),
        ))
        .unwrap()
        .unwrap();
    scenario.run(effect_a.wait_forward()).unwrap().unwrap();
    let (release_b, gate_b) = controlled();
    let b_trace = Arc::clone(&trace);
    let effect_b = scenario
        .run(scope.effect(
            "B",
            effect::sync(move || Ok(Some(gated_inverse("B", Some(gate_b), b_trace)))),
        ))
        .unwrap()
        .unwrap();
    scenario.run(effect_b.wait_forward()).unwrap().unwrap();

    assert!(scenario.run(fiber.dispose()).is_err());
    assert_eq!(trace.lock().unwrap().as_slice(), ["B-start"]);
    release_b.complete(()).unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["B-start", "B-finish", "A-start", "A-finish"]
    );
    scenario.run(fiber.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct FailingLifo {
    trace: Arc<Mutex<Vec<u8>>>,
}

impl Component for FailingLifo {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &LIFO_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let one = Arc::clone(&self.trace);
        let two = Arc::clone(&self.trace);
        let three = Arc::clone(&self.trace);
        Activation::Steps(Box::pin(stream::iter([
            Ok(effect::inverse(move || async move {
                one.lock().unwrap().push(1);
                Ok(())
            })),
            Ok(effect::inverse(move || async move {
                two.lock().unwrap().push(2);
                Ok(())
            })),
            Ok(effect::inverse(move || async move {
                three.lock().unwrap().push(3);
                Err(CleanupError::user("third failed"))
            })),
        ])))
    }
}

#[test]
fn e02_e08_fiber_dispose_is_shared_and_inverse_error_does_not_stop_lifo() {
    let scenario = Scenario::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            FailingLifo {
                trace: Arc::clone(&trace),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(fiber.wait_settled()).unwrap();
    let first = fiber.clone();
    let second = fiber.clone();
    let (first_report, second_report) = scenario
        .run(async move { future::join(first.dispose(), second.dispose()).await })
        .unwrap();
    assert_eq!(first_report, second_report);
    assert_eq!(first_report.cleanup_errors.len(), 1);
    assert_eq!(
        first_report.cleanup_errors[0].cause.to_string(),
        "third failed"
    );
    assert_eq!(trace.lock().unwrap().as_slice(), [3, 2, 1]);
    assert_eq!(scenario.run(fiber.state()).unwrap(), FiberState::Disposed);
    assert_eq!(scenario.run(fiber.dispose()).unwrap(), first_report);
    scenario.run(scenario.runtime().shutdown()).unwrap();
}
