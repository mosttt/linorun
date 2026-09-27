use std::{
    any::Any,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures_util::stream;
use linorun_core::{
    Activation, ActivationFailure, Component, ComponentContext, ComponentDescriptor,
    ErasedEndpoint, FiberState, Requirement, RuntimeEventKind, ServiceContract, ServiceRef,
    UpdateError, effect,
};
use linorun_testkit::{Controlled, Scenario, controlled};

static FAILURE_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<u32>("conformance/failure-revision@1", vec![], vec![])
});
static LOADING_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>("conformance/loading-dispose@1", vec![], vec![])
});
static VERSIONED_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<String>("conformance/versioned@1", vec![], vec![]));
static OTHER_VERSIONED_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<String>("conformance/versioned@1", vec![], vec![]));

struct FailureThenSuccess {
    activations: Arc<AtomicUsize>,
    cleanup_gate: Mutex<Option<Controlled<()>>>,
    trace: Arc<Mutex<Vec<String>>>,
}

impl Component for FailureThenSuccess {
    type Config = u32;

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &FAILURE_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, config: Arc<Self::Config>) -> Activation {
        let attempt = self.activations.fetch_add(1, Ordering::SeqCst);
        self.trace
            .lock()
            .unwrap()
            .push(format!("activate:{config}"));
        if attempt != 0 {
            return Activation::Once(Box::pin(async { Ok(None) }));
        }
        let gate = self.cleanup_gate.lock().unwrap().take();
        let trace = Arc::clone(&self.trace);
        let value = *config;
        Activation::Steps(Box::pin(stream::iter([
            Ok(effect::inverse(move || async move {
                if let Some(gate) = gate {
                    gate.await;
                }
                trace.lock().unwrap().push(format!("cleanup:{value}"));
                Ok(())
            })),
            Err(ActivationFailure::user("first activation failed")),
        ])))
    }
}

#[test]
fn r09_update_during_failure_unload_invalidates_old_failure() {
    let scenario = Scenario::new();
    let activations = Arc::new(AtomicUsize::new(0));
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (release, gate) = controlled();
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            FailureThenSuccess {
                activations: Arc::clone(&activations),
                cleanup_gate: Mutex::new(Some(gate)),
                trace: Arc::clone(&trace),
            },
            1,
        ))
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(scenario.run(fiber.state()).unwrap(), FiberState::Unloading);

    scenario.run(fiber.update_config(2_u32)).unwrap().unwrap();
    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(fiber.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert_eq!(activations.load(Ordering::SeqCst), 2);
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["activate:1", "cleanup:1", "activate:2"]
    );
    let snapshot = scenario.snapshot().unwrap();
    let fiber_snapshot = snapshot
        .fibers
        .iter()
        .find(|candidate| candidate.id == fiber.id())
        .unwrap();
    assert_eq!(fiber_snapshot.activation_revision, 1);
    assert!(fiber_snapshot.failure.is_none());
    scenario.run(fiber.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn f10_failed_fiber_retries_only_after_explicit_restart() {
    let scenario = Scenario::new();
    let activations = Arc::new(AtomicUsize::new(0));
    let trace = Arc::new(Mutex::new(Vec::new()));
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            FailureThenSuccess {
                activations: Arc::clone(&activations),
                cleanup_gate: Mutex::new(None),
                trace,
            },
            1,
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(fiber.wait_settled()).unwrap(),
        FiberState::Failed
    );
    scenario.executor().run_until_stalled();
    assert_eq!(activations.load(Ordering::SeqCst), 1);
    assert!(scenario.snapshot().unwrap().quiescent);

    scenario.run(fiber.restart()).unwrap().unwrap();
    assert_eq!(
        scenario.run(fiber.wait_settled()).unwrap(),
        FiberState::Active
    );
    assert_eq!(activations.load(Ordering::SeqCst), 2);
    scenario.run(fiber.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct LoadingComponent {
    gate: Mutex<Option<Controlled<()>>>,
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for LoadingComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &LOADING_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let gate = self.gate.lock().unwrap().take().unwrap();
        let landed = Arc::clone(&self.trace);
        let cleanup = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            gate.await;
            landed.lock().unwrap().push("landed");
            Ok(Some(effect::inverse(move || async move {
                cleanup.lock().unwrap().push("cleanup");
                Ok(())
            })))
        }))
    }
}

#[test]
fn f11_dispose_loading_waits_for_step_then_diverts_and_removes() {
    let scenario = Scenario::new();
    let (release, gate) = controlled();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            LoadingComponent {
                gate: Mutex::new(Some(gate)),
                trace: Arc::clone(&trace),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(scenario.run(fiber.state()).unwrap(), FiberState::Loading);
    assert!(scenario.run(fiber.dispose()).is_err());
    release.complete(()).unwrap();
    scenario.run(fiber.dispose()).unwrap();
    assert_eq!(trace.lock().unwrap().as_slice(), ["landed", "cleanup"]);
    let events = scenario.trace().drain();
    assert!(events.iter().any(|event| {
        matches!(event.kind, RuntimeEventKind::LDivert { fiber: id, .. } if id == fiber.id())
    }));
    assert!(!events.iter().any(|event| {
        matches!(event.kind, RuntimeEventKind::LFinish { fiber: id, .. } if id == fiber.id())
    }));
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct Versioned {
    name: &'static str,
    trace: Arc<Mutex<Vec<String>>>,
}

impl Component for Versioned {
    type Config = String;

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &VERSIONED_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, config: Arc<Self::Config>) -> Activation {
        let name = self.name;
        let value = config.as_str().to_owned();
        self.trace
            .lock()
            .unwrap()
            .push(format!("activate:{name}:{value}"));
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                trace
                    .lock()
                    .unwrap()
                    .push(format!("cleanup:{name}:{value}"));
                Ok(())
            })))
        }))
    }
}

struct WrongDescriptor;

impl Component for WrongDescriptor {
    type Config = String;

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &OTHER_VERSIONED_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        unreachable!()
    }
}

struct WrongConfig;

impl Component for WrongConfig {
    type Config = u32;

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &VERSIONED_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        unreachable!()
    }
}

#[test]
fn f07_f13_f14_updates_validate_then_create_complete_new_episodes() {
    let scenario = Scenario::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            Versioned {
                name: "first",
                trace: Arc::clone(&trace),
            },
            "A".to_owned(),
        ))
        .unwrap()
        .unwrap();
    scenario.run(fiber.wait_settled()).unwrap();
    let initial = scenario.snapshot().unwrap();
    let initial_revision = initial
        .fibers
        .iter()
        .find(|candidate| candidate.id == fiber.id())
        .unwrap()
        .activation_revision;

    assert_eq!(
        scenario.run(fiber.update_config(9_u32)).unwrap(),
        Err(UpdateError::DescriptorMismatch)
    );
    assert_eq!(
        scenario.run(fiber.replace(WrongDescriptor)).unwrap(),
        Err(UpdateError::DescriptorMismatch)
    );
    assert_eq!(
        scenario.run(fiber.replace(WrongConfig)).unwrap(),
        Err(UpdateError::DescriptorMismatch)
    );
    let unchanged = scenario.snapshot().unwrap();
    assert_eq!(
        unchanged
            .fibers
            .iter()
            .find(|candidate| candidate.id == fiber.id())
            .unwrap()
            .activation_revision,
        initial_revision
    );

    scenario
        .run(fiber.update_config("B".to_owned()))
        .unwrap()
        .unwrap();
    scenario.run(fiber.wait_settled()).unwrap();
    scenario
        .run(fiber.replace(Versioned {
            name: "second",
            trace: Arc::clone(&trace),
        }))
        .unwrap()
        .unwrap();
    scenario.run(fiber.wait_settled()).unwrap();
    scenario.run(fiber.restart()).unwrap().unwrap();
    scenario.run(fiber.wait_settled()).unwrap();
    scenario.run(fiber.dispose()).unwrap();
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        [
            "activate:first:A",
            "cleanup:first:A",
            "activate:first:B",
            "cleanup:first:B",
            "activate:second:B",
            "cleanup:second:B",
            "activate:second:B",
            "cleanup:second:B",
        ]
    );
    assert_eq!(
        scenario.run(fiber.update_config("C".to_owned())).unwrap(),
        Err(UpdateError::FiberDisposed)
    );
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct MissingContract;
struct MissingEndpoint;

impl ErasedEndpoint for MissingEndpoint {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

impl ServiceContract for MissingContract {
    const ID: &'static str = "conformance/missing@1";
    type Provider = ();
    type Intercept = ();
    type Client = ServiceRef<Self>;

    fn identity_intercept() -> Self::Intercept {}
    fn merge_intercept(_: &Self::Intercept, _: &Self::Intercept) -> Self::Intercept {}
    fn erase(_: Self::Provider) -> Arc<dyn ErasedEndpoint> {
        Arc::new(MissingEndpoint)
    }
    fn client(reference: ServiceRef<Self>) -> Self::Client {
        reference
    }
}

static PENDING_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/pending@1",
        vec![Requirement::new::<MissingContract>(None)],
        vec![],
    )
});

struct NeverActivated(Arc<AtomicUsize>);

impl Component for NeverActivated {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PENDING_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        self.0.fetch_add(1, Ordering::SeqCst);
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

#[test]
fn f15_f16_failed_and_pending_fibers_dispose_without_reactivation() {
    let scenario = Scenario::new();
    let pending_calls = Arc::new(AtomicUsize::new(0));
    let pending = scenario
        .run(
            scenario
                .runtime()
                .root()
                .mount(NeverActivated(Arc::clone(&pending_calls)), ()),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(pending.wait_settled()).unwrap(),
        FiberState::Pending
    );
    let snapshot = scenario.snapshot().unwrap();
    assert!(
        snapshot
            .fibers
            .iter()
            .find(|candidate| candidate.id == pending.id())
            .unwrap()
            .episode
            .is_none()
    );
    scenario.run(pending.dispose()).unwrap();
    assert_eq!(pending_calls.load(Ordering::SeqCst), 0);

    let failed_calls = Arc::new(AtomicUsize::new(0));
    let failed = scenario
        .run(scenario.runtime().root().mount(
            FailureThenSuccess {
                activations: Arc::clone(&failed_calls),
                cleanup_gate: Mutex::new(None),
                trace: Arc::new(Mutex::new(Vec::new())),
            },
            1,
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(failed.wait_settled()).unwrap(),
        FiberState::Failed
    );
    scenario.run(failed.dispose()).unwrap();
    assert_eq!(failed_calls.load(Ordering::SeqCst), 1);
    scenario.run(scenario.runtime().shutdown()).unwrap();
}
