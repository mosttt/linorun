use std::{
    any::Any,
    sync::{Arc, LazyLock, Mutex},
};

use linorun_core::{
    Activation, CleanupError, Component, ComponentContext, ComponentDescriptor,
    EffectForwardOutcome, EffectStartError, ErasedEndpoint, FailureKind, FiberState, MountError,
    ProvideError, RuntimeEventKind, ServiceContract, ServiceRef, effect,
};
use linorun_testkit::{EventTag, Scenario, controlled, event_tag};

static NOOP_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("conformance/noop@1", vec![], vec![]));
static PANIC_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("conformance/panic@1", vec![], vec![]));
static CONTEXT_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("conformance/context@1", vec![], vec![]));

struct Noop;

impl Component for Noop {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &NOOP_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

struct PanicComponent;

impl Component for PanicComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PANIC_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        panic!("synchronous activation panic")
    }
}

struct UnitContract;
struct UnitEndpoint;

impl ErasedEndpoint for UnitEndpoint {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

impl ServiceContract for UnitContract {
    const ID: &'static str = "conformance/unit@1";

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

#[test]
fn e15_core_owned_effect_trace_and_e11_host_mount_path_are_exact() {
    let scenario = Scenario::new();
    let provide = scenario.run(scenario.runtime().root().provide::<UnitContract>(()));
    let provide = provide.unwrap().unwrap();
    assert_eq!(
        scenario.run(provide.wait_forward()).unwrap().unwrap(),
        EffectForwardOutcome::Completed
    );
    let child = scenario
        .run(scenario.runtime().root().mount(Noop, ()))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(child.wait_settled()).unwrap(),
        FiberState::Active
    );

    let snapshot = scenario.snapshot().unwrap();
    let mount_effect = snapshot
        .fibers
        .iter()
        .find(|fiber| fiber.id == child.id())
        .unwrap()
        .mount_effect
        .unwrap();
    let before_dispose = scenario.trace().drain();
    let provide_tags: Vec<_> = before_dispose
        .iter()
        .filter_map(|event| match &event.kind {
            RuntimeEventKind::EffectStarted { effect, .. }
            | RuntimeEventKind::EffectForwardCompleted { effect, .. }
            | RuntimeEventKind::EffectYielded { effect, .. }
                if *effect == provide.id() =>
            {
                Some(event_tag(&event.kind))
            }
            RuntimeEventKind::BindingInstalled { binding }
            | RuntimeEventKind::BindingVisible { binding }
            | RuntimeEventKind::BindingRemoved { binding }
                if binding.owner_effect == provide.id() =>
            {
                Some(event_tag(&event.kind))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        provide_tags,
        [
            EventTag::EffectStarted,
            EventTag::BindingInstalled,
            EventTag::BindingVisible,
            EventTag::EffectForwardCompleted,
        ]
    );

    let mount_positions = before_dispose
        .iter()
        .enumerate()
        .filter_map(|(position, event)| match &event.kind {
            RuntimeEventKind::EffectStarted { effect, .. }
            | RuntimeEventKind::EffectForwardCompleted { effect, .. }
                if *effect == mount_effect =>
            {
                Some((event_tag(&event.kind), position))
            }
            RuntimeEventKind::FiberInserted { fiber, .. } if *fiber == child.id() => {
                Some((EventTag::FiberInserted, position))
            }
            RuntimeEventKind::LBegin { fiber, .. } if *fiber == child.id() => {
                Some((EventTag::LBegin, position))
            }
            RuntimeEventKind::EffectYielded { effect, .. } if *effect == mount_effect => {
                panic!("Core-owned mount must not yield a user step")
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        mount_positions
            .iter()
            .map(|entry| entry.0)
            .collect::<Vec<_>>(),
        [
            EventTag::EffectStarted,
            EventTag::FiberInserted,
            EventTag::EffectForwardCompleted,
            EventTag::LBegin,
        ]
    );

    scenario.run(child.dispose()).unwrap();
    let host_events = scenario.trace().drain();
    let requested = host_events
        .iter()
        .position(|event| {
            matches!(
                event.kind,
                RuntimeEventKind::EffectDisposeRequested { effect } if effect == mount_effect
            )
        })
        .unwrap();
    let retired = host_events
        .iter()
        .position(|event| {
            matches!(event.kind, RuntimeEventKind::FiberRetired { fiber } if fiber == child.id())
        })
        .unwrap();
    assert!(requested < retired);
    assert!(!host_events.iter().any(|event| {
        matches!(
            event.kind,
            RuntimeEventKind::InverseStarted {
                owner: linorun_core::CleanupOwner::Effect(effect),
                ..
            } if effect == mount_effect
        )
    }));

    scenario.run(provide.dispose()).unwrap();
    let provide_cleanup = scenario.trace().drain();
    let removed = provide_cleanup
        .iter()
        .position(|event| {
            matches!(
                &event.kind,
                RuntimeEventKind::BindingRemoved { binding }
                    if binding.owner_effect == provide.id()
            )
        })
        .unwrap();
    let started = provide_cleanup
        .iter()
        .position(|event| {
            matches!(
                event.kind,
                RuntimeEventKind::InverseStarted {
                    owner: linorun_core::CleanupOwner::Effect(effect),
                    ..
                } if effect == provide.id()
            )
        })
        .unwrap();
    let finished = provide_cleanup
        .iter()
        .position(|event| {
            matches!(
                event.kind,
                RuntimeEventKind::InverseFinished {
                    owner: linorun_core::CleanupOwner::Effect(effect),
                    ..
                } if effect == provide.id()
            )
        })
        .unwrap();
    assert!(removed < started && started < finished);
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn e16_effect_forward_panic_is_an_error_and_requests_cleanup_once() {
    let scenario = Scenario::new();
    let effect = scenario
        .run(scenario.runtime().root().effect(
            "panic",
            effect::sync(|| -> Result<_, linorun_core::EffectError> {
                panic!("effect poll panic")
            }),
        ))
        .unwrap()
        .unwrap();
    let error = scenario.run(effect.wait_forward()).unwrap().unwrap_err();
    assert_eq!(error.cause.kind, FailureKind::Panic);
    assert_eq!(error.cause.message.as_ref(), "effect poll panic");
    let report = scenario.run(effect.dispose()).unwrap();
    assert_eq!(report.forward_error, Some(error));

    let events = scenario.trace().drain();
    let failed = events
        .iter()
        .position(|event| {
            matches!(event.kind, RuntimeEventKind::EffectForwardFailed { effect: id, .. } if id == effect.id())
        })
        .unwrap();
    let requests: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(event.kind, RuntimeEventKind::EffectDisposeRequested { effect: id } if id == effect.id())
        })
        .collect();
    assert_eq!(requests.len(), 1);
    assert!(failed < requests[0].0);
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn f18_synchronous_activate_panic_rolls_back_to_failed() {
    let scenario = Scenario::new();
    let fiber = scenario
        .run(scenario.runtime().root().mount(PanicComponent, ()))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(fiber.wait_settled()).unwrap(),
        FiberState::Failed
    );
    let snapshot = scenario.snapshot().unwrap();
    let failure = snapshot
        .fibers
        .iter()
        .find(|candidate| candidate.id == fiber.id())
        .unwrap()
        .failure
        .as_ref()
        .unwrap();
    assert_eq!(failure.cause.kind, FailureKind::Panic);
    assert_eq!(
        failure.cause.message.as_ref(),
        "synchronous activation panic"
    );
    scenario.run(fiber.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct PublishContext {
    output: Arc<Mutex<Option<(linorun_core::Context, linorun_core::Scope)>>>,
}

impl Component for PublishContext {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CONTEXT_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        *self.output.lock().unwrap() = Some((context.context().clone(), context.scope().clone()));
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

#[test]
fn mount_from_a_retired_scope_reports_fiber_disposed_before_scope_errors() {
    let scenario = Scenario::new();
    let output = Arc::new(Mutex::new(None));
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            PublishContext {
                output: Arc::clone(&output),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(fiber.wait_settled()).unwrap();
    let (context, scope) = output.lock().unwrap().clone().unwrap();
    scenario.run(fiber.dispose()).unwrap();
    assert!(matches!(
        scenario.run(scope.mount(&context, Noop, ())).unwrap(),
        Err(MountError::FiberDisposed)
    ));
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn s04_shutdown_is_idempotent_and_running_only_operations_are_rejected() {
    let scenario = Scenario::new();
    let (release, cleanup) = controlled();
    let effect = scenario
        .run(scenario.runtime().root().effect(
            "blocked-cleanup",
            effect::sync(move || {
                Ok(Some(effect::inverse(move || async move {
                    cleanup.await;
                    Err(CleanupError::user("shutdown cleanup"))
                })))
            }),
        ))
        .unwrap()
        .unwrap();
    scenario.run(effect.wait_forward()).unwrap().unwrap();

    let stalled = scenario.run(scenario.runtime().shutdown()).unwrap_err();
    assert!(stalled.active_tasks > 0);
    assert!(matches!(
        scenario
            .run(scenario.runtime().root().effect(
                "late",
                effect::sync(|| Ok::<_, linorun_core::EffectError>(None)),
            ))
            .unwrap(),
        Err(EffectStartError::RuntimeShuttingDown)
    ));
    assert!(matches!(
        scenario
            .run(scenario.runtime().root().mount(Noop, ()))
            .unwrap(),
        Err(MountError::RuntimeShuttingDown)
    ));
    assert!(matches!(
        scenario
            .run(scenario.runtime().root().provide::<UnitContract>(()))
            .unwrap(),
        Err(ProvideError::RuntimeShuttingDown)
    ));
    let _realm = scenario.runtime().root().create_realm();

    release.complete(()).unwrap();
    let first = scenario.run(scenario.runtime().shutdown()).unwrap();
    assert_eq!(first.cleanup_errors.len(), 1);
    let second = scenario.run(scenario.runtime().shutdown()).unwrap();
    assert_eq!(second, first);

    let events = scenario.trace().drain();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, RuntimeEventKind::RuntimeShuttingDown))
            .count(),
        1
    );
    assert_eq!(
        event_tag(&events.last().unwrap().kind),
        EventTag::RuntimeStopped
    );
}
