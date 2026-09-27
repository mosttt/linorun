use std::{
    any::Any,
    future::Future,
    sync::{Arc, Mutex},
    task::{Context as TaskContext, Waker},
};

use futures_channel::oneshot;
use linorun_core::{
    Activation, CallContext, Component, ComponentContext, ComponentDescriptor,
    EffectForwardOutcome, EffectStartError, ErasedEndpoint, FiberState, Runtime, RuntimeState,
    ServiceContract, ServiceRef, TaskSpawner, effect,
};
use linorun_macros::component_descriptor;
use linorun_testkit::{DeterministicExecutor, Scenario, controlled};

component_descriptor! {
    static CLEANUP_OWNER: () {
        id: "authoring/cleanup-owner@1",
        requires: [],
        provides: [],
    }
}

struct PublishContext {
    descriptor: &'static ComponentDescriptor,
    output: Mutex<Option<oneshot::Sender<ComponentContext>>>,
}

impl Component for PublishContext {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        self.descriptor
    }

    fn activate(&self, context: ComponentContext, _: Arc<()>) -> Activation {
        let _ = self.output.lock().unwrap().take().unwrap().send(context);
        Activation::run(async { Ok(()) })
    }
}

#[test]
fn registered_cleanup_is_serial_lifo_and_early_disposal_is_not_repeated() {
    let scenario = Scenario::new();
    let (send_context, receive_context) = oneshot::channel();
    let owner = scenario
        .run(scenario.runtime().mount(
            PublishContext {
                descriptor: &CLEANUP_OWNER,
                output: Mutex::new(Some(send_context)),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(owner.wait_settled()).unwrap(),
        FiberState::Active
    );
    let context = scenario.run(receive_context).unwrap().unwrap();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for label in ["first", "early"] {
        let trace = Arc::clone(&trace);
        handles.push(
            scenario
                .run(context.on_cleanup(label, move || async move {
                    trace.lock().unwrap().push(label);
                    Ok(())
                }))
                .unwrap()
                .unwrap(),
        );
    }
    let (release, gate) = controlled();
    let last_trace = Arc::clone(&trace);
    let last = scenario
        .run(context.on_cleanup("last", move || async move {
            last_trace.lock().unwrap().push("last-start");
            gate.await;
            last_trace.lock().unwrap().push("last-finish");
            Ok(())
        }))
        .unwrap()
        .unwrap();

    assert!(trace.lock().unwrap().is_empty());
    scenario.run(handles[1].dispose()).unwrap();
    scenario.run(handles[1].dispose()).unwrap();
    assert_eq!(*trace.lock().unwrap(), ["early"]);

    assert!(scenario.run(owner.dispose()).is_err());
    assert_eq!(*trace.lock().unwrap(), ["early", "last-start"]);
    assert_eq!(scenario.run(owner.state()).unwrap(), FiberState::Unloading);
    release.complete(()).unwrap();
    assert!(
        scenario
            .run(owner.dispose())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
    for handle in handles.iter().chain(std::iter::once(&last)) {
        scenario.run(handle.dispose()).unwrap();
    }
    assert_eq!(
        *trace.lock().unwrap(),
        ["early", "last-start", "last-finish", "first"]
    );
    assert!(matches!(
        scenario.run(context.on_cleanup("stale", || async {
            panic!("a disposed component cannot register new cleanup")
        })),
        Ok(Err(EffectStartError::InactiveScope))
    ));
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct Probe;
struct ProbeEndpoint(&'static str);

impl ErasedEndpoint for ProbeEndpoint {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

impl ServiceContract for Probe {
    const ID: &'static str = "authoring/probe@1";

    type Provider = &'static str;
    type Intercept = Vec<&'static str>;
    type Client = ServiceRef<Self>;

    fn identity_intercept() -> Self::Intercept {
        Vec::new()
    }

    fn merge_intercept(base: &Self::Intercept, overlay: &Self::Intercept) -> Self::Intercept {
        base.iter().chain(overlay).copied().collect()
    }

    fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint> {
        Arc::new(ProbeEndpoint(provider))
    }

    fn client(reference: ServiceRef<Self>) -> Self::Client {
        reference
    }
}

fn observe(client: &ServiceRef<Probe>) -> (&'static str, CallContext<Vec<&'static str>>) {
    let (endpoint, call) = client.dispatch_parts();
    (
        endpoint.as_any().downcast_ref::<ProbeEndpoint>().unwrap().0,
        call,
    )
}

component_descriptor! {
    static ROUTED_OWNER: () {
        id: "authoring/routed-owner@1",
        requires: [Probe],
        provides: [Probe],
    }
}

component_descriptor! {
    static ROUTED_CHILD: () {
        id: "authoring/routed-child@1",
        requires: [Probe => vec!["declared"]],
        provides: [],
    }
}

type Observations = Arc<Mutex<Vec<(&'static str, CallContext<Vec<&'static str>>)>>>;

struct RoutedChild {
    observations: Observations,
    cleanup: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for RoutedChild {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &ROUTED_CHILD
    }

    fn activate(&self, context: ComponentContext, _: Arc<()>) -> Activation {
        let observations = Arc::clone(&self.observations);
        let cleanup = Arc::clone(&self.cleanup);
        Activation::run(async move {
            let client = context.service::<Probe>()?;
            observations.lock().unwrap().push(observe(&client));
            context
                .on_cleanup("child", move || async move {
                    cleanup.lock().unwrap().push(observe(&client).0);
                    Ok(())
                })
                .await?;
            Ok(())
        })
    }
}

#[test]
fn derived_context_routes_children_and_publications_without_changing_the_owner() {
    let scenario = Scenario::new();
    let runtime = scenario.runtime();
    scenario
        .run(runtime.provide::<Probe>("host"))
        .unwrap()
        .unwrap();
    let (send_context, receive_context) = oneshot::channel();
    let owner = scenario
        .run(runtime.root().intercept::<Probe>(vec!["outer"]).mount(
            PublishContext {
                descriptor: &ROUTED_OWNER,
                output: Mutex::new(Some(send_context)),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(owner.wait_settled()).unwrap(),
        FiberState::Active
    );
    let context = scenario.run(receive_context).unwrap().unwrap();
    let realm = runtime.root().create_realm();
    let derived = context
        .isolate::<Probe>(realm.clone())
        .unwrap()
        .intercept::<Probe>(vec!["inner"]);

    let (committed_value, committed_call) = observe(&derived.service::<Probe>().unwrap());
    assert_eq!(committed_value, "host");
    assert_eq!(committed_call.intercept, ["outer", "inner"]);
    let cleanup = Arc::new(Mutex::new(Vec::new()));
    let owner_cleanup = Arc::clone(&cleanup);
    scenario
        .run(derived.on_cleanup("provider", move || async move {
            owner_cleanup.lock().unwrap().push("provider");
            Ok(())
        }))
        .unwrap()
        .unwrap();
    scenario
        .run(derived.provide::<Probe>("tenant"))
        .unwrap()
        .unwrap();

    let observations = Arc::new(Mutex::new(Vec::new()));
    let restored = derived.isolate::<Probe>(context.default_realm()).unwrap();
    let mut children = Vec::new();
    for route in [&derived, &restored] {
        let child = scenario
            .run(route.mount(
                RoutedChild {
                    observations: Arc::clone(&observations),
                    cleanup: Arc::clone(&cleanup),
                },
                (),
            ))
            .unwrap()
            .unwrap();
        assert_eq!(
            scenario.run(child.wait_settled()).unwrap(),
            FiberState::Active
        );
        children.push(child);
    }

    let observed = observations.lock().unwrap().clone();
    assert_eq!(
        observed.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        ["tenant", "host"]
    );
    for (observation, child) in observed.iter().zip(&children) {
        assert_eq!(observation.1.caller, child.id());
        assert_eq!(observation.1.intercept, ["declared", "outer", "inner"]);
    }
    assert_eq!(observed[0].1.provider, owner.id());
    let snapshot = scenario.snapshot().unwrap();
    assert!(
        snapshot
            .bindings
            .iter()
            .any(|binding| { binding.realm == realm && binding.provider == owner.id() })
    );
    for child in &children {
        assert_eq!(
            snapshot
                .fibers
                .iter()
                .find(|fiber| fiber.id == child.id())
                .unwrap()
                .parent,
            Some(owner.id())
        );
    }

    assert!(
        scenario
            .run(owner.dispose())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
    for child in children {
        assert_eq!(scenario.run(child.state()).unwrap(), FiberState::Disposed);
    }
    let cleanup = cleanup.lock().unwrap();
    assert_eq!(cleanup.len(), 3);
    assert!(cleanup.contains(&"host"));
    assert!(
        cleanup.iter().position(|entry| *entry == "tenant").unwrap()
            < cleanup
                .iter()
                .position(|entry| *entry == "provider")
                .unwrap()
    );
    assert_eq!(scenario.snapshot().unwrap().bindings.len(), 1);
    scenario.run(runtime.shutdown()).unwrap();
}

#[test]
fn shutdown_finishes_cleanup_registration_already_submitted_to_a_closure_spawner() {
    let executor = DeterministicExecutor::new();
    let task_executor = executor.clone();
    let runtime = Runtime::new(move |task| task_executor.spawn(task));
    let trace = Arc::new(Mutex::new(Vec::new()));
    let effect_trace = Arc::clone(&trace);
    let effect = executor
        .drive(runtime.effect(
            "host effect",
            effect::sync(move || {
                Ok(Some(effect::inverse(move || async move {
                    effect_trace.lock().unwrap().push("effect");
                    Ok(())
                })))
            }),
        ))
        .unwrap()
        .unwrap();
    executor.drive(effect.wait_forward()).unwrap().unwrap();

    let cleanup_trace = Arc::clone(&trace);
    let mut registration = Box::pin(runtime.on_cleanup("racing cleanup", move || async move {
        cleanup_trace.lock().unwrap().push("cleanup");
        Ok(())
    }));
    assert!(
        registration
            .as_mut()
            .poll(&mut TaskContext::from_waker(Waker::noop()))
            .is_pending()
    );
    assert!(trace.lock().unwrap().is_empty());
    assert!(
        executor
            .drive(runtime.shutdown())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );

    let cleanup = executor.drive(registration).unwrap().unwrap();
    assert_eq!(
        executor.drive(cleanup.wait_forward()).unwrap().unwrap(),
        EffectForwardOutcome::Disposed
    );
    executor.drive(cleanup.dispose()).unwrap();
    executor.drive(effect.dispose()).unwrap();
    assert_eq!(*trace.lock().unwrap(), ["cleanup", "effect"]);
    assert!(matches!(
        executor.drive(runtime.on_cleanup("late", || async {
            panic!("shutdown must reject new cleanup registration")
        })),
        Ok(Err(EffectStartError::RuntimeShuttingDown))
    ));
    assert_eq!(
        executor.drive(runtime.snapshot()).unwrap().runtime_state,
        RuntimeState::Stopped
    );
}
