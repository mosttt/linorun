use std::{
    any::Any,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use linorun_core::{
    Activation, ActivationFailure, Component, ComponentContext, ComponentDescriptor,
    ErasedEndpoint, FiberState, Requirement, ServiceContract, ServiceRef, effect,
};
use linorun_testkit::Scenario;

macro_rules! unit_contract {
    ($contract:ident, $endpoint:ident, $id:literal) => {
        struct $contract;
        struct $endpoint;

        impl ErasedEndpoint for $endpoint {
            fn as_any(&self) -> &(dyn Any + Send + Sync) {
                self
            }
        }

        impl ServiceContract for $contract {
            const ID: &'static str = $id;
            type Provider = ();
            type Intercept = ();
            type Client = ServiceRef<Self>;

            fn identity_intercept() -> Self::Intercept {}
            fn merge_intercept(_: &Self::Intercept, _: &Self::Intercept) -> Self::Intercept {}
            fn erase(_: Self::Provider) -> Arc<dyn ErasedEndpoint> {
                Arc::new($endpoint)
            }
            fn client(reference: ServiceRef<Self>) -> Self::Client {
                reference
            }
        }
    };
}

unit_contract!(ContractA, EndpointA, "conformance/chain-a@1");
unit_contract!(ContractB, EndpointB, "conformance/chain-b@1");

static A_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/chain-a-provider@1",
        vec![],
        vec![ContractA::ID],
    )
});
static B_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/chain-b-provider@1",
        vec![Requirement::new::<ContractA>(None)],
        vec![ContractB::ID],
    )
});
static C_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/chain-c-consumer@1",
        vec![Requirement::new::<ContractB>(None)],
        vec![],
    )
});
static A_CONSUMER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/a-consumer@1",
        vec![Requirement::new::<ContractA>(None)],
        vec![],
    )
});
static JOIN_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/chain-join@1",
        vec![
            Requirement::new::<ContractA>(None),
            Requirement::new::<ContractB>(None),
        ],
        vec![],
    )
});
static CYCLE_A_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/cycle-a@1",
        vec![Requirement::new::<ContractB>(None)],
        vec![ContractA::ID],
    )
});
static CYCLE_B_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "conformance/cycle-b@1",
        vec![Requirement::new::<ContractA>(None)],
        vec![ContractB::ID],
    )
});

struct AProvider {
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for AProvider {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &A_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            scope
                .provide::<ContractA>(&route, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                trace.lock().unwrap().push("A-cleanup");
                Ok(())
            })))
        }))
    }
}

struct BProvider {
    trace: Arc<Mutex<Vec<&'static str>>>,
    activations: Arc<AtomicUsize>,
}

impl Component for BProvider {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &B_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let _a = context.context().service::<ContractA>().unwrap();
        self.activations.fetch_add(1, Ordering::SeqCst);
        self.trace.lock().unwrap().push("B-start");
        let scope = context.scope().clone();
        let route = context.context().clone();
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            scope
                .provide::<ContractB>(&route, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                trace.lock().unwrap().push("B-cleanup");
                Ok(())
            })))
        }))
    }
}

struct CConsumer {
    trace: Arc<Mutex<Vec<&'static str>>>,
    activations: Arc<AtomicUsize>,
}

impl Component for CConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &C_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let _b = context.context().service::<ContractB>().unwrap();
        self.activations.fetch_add(1, Ordering::SeqCst);
        self.trace.lock().unwrap().push("C-start");
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                trace.lock().unwrap().push("C-cleanup");
                Ok(())
            })))
        }))
    }
}

fn mount_chain(
    scenario: &Scenario,
    trace: &Arc<Mutex<Vec<&'static str>>>,
    b_activations: &Arc<AtomicUsize>,
    c_activations: &Arc<AtomicUsize>,
) -> (
    linorun_core::FiberHandle,
    linorun_core::FiberHandle,
    linorun_core::FiberHandle,
) {
    let a = scenario
        .run(scenario.runtime().root().mount(
            AProvider {
                trace: Arc::clone(trace),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(a.wait_settled()).unwrap();
    let b = scenario
        .run(scenario.runtime().root().mount(
            BProvider {
                trace: Arc::clone(trace),
                activations: Arc::clone(b_activations),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(b.wait_settled()).unwrap();
    let c = scenario
        .run(scenario.runtime().root().mount(
            CConsumer {
                trace: Arc::clone(trace),
                activations: Arc::clone(c_activations),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(c.wait_settled()).unwrap();
    (a, b, c)
}

#[test]
fn d05_dependency_chain_cleans_consumer_to_provider() {
    let scenario = Scenario::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let b_activations = Arc::new(AtomicUsize::new(0));
    let c_activations = Arc::new(AtomicUsize::new(0));
    let (a, b, c) = mount_chain(&scenario, &trace, &b_activations, &c_activations);
    trace.lock().unwrap().clear();

    scenario.run(a.dispose()).unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["C-cleanup", "B-cleanup", "A-cleanup"]
    );
    assert_eq!(scenario.run(b.state()).unwrap(), FiberState::Pending);
    assert_eq!(scenario.run(c.state()).unwrap(), FiberState::Pending);
    scenario.run(b.dispose()).unwrap();
    scenario.run(c.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct AConsumer {
    name: &'static str,
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for AConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &A_CONSUMER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let _a = context.context().service::<ContractA>().unwrap();
        let name = self.name;
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                trace.lock().unwrap().push(name);
                Ok(())
            })))
        }))
    }
}

#[test]
fn d06_provider_waits_for_every_branch_consumer() {
    let scenario = Scenario::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let a = scenario
        .run(scenario.runtime().root().mount(
            AProvider {
                trace: Arc::clone(&trace),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(a.wait_settled()).unwrap();
    let b = scenario
        .run(scenario.runtime().root().mount(
            AConsumer {
                name: "B-cleanup",
                trace: Arc::clone(&trace),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    let c = scenario
        .run(scenario.runtime().root().mount(
            AConsumer {
                name: "C-cleanup",
                trace: Arc::clone(&trace),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(b.wait_settled()).unwrap();
    scenario.run(c.wait_settled()).unwrap();
    trace.lock().unwrap().clear();

    scenario.run(a.dispose()).unwrap();
    let trace = trace.lock().unwrap();
    let a_position = trace
        .iter()
        .position(|entry| *entry == "A-cleanup")
        .unwrap();
    let b_position = trace
        .iter()
        .position(|entry| *entry == "B-cleanup")
        .unwrap();
    let c_position = trace
        .iter()
        .position(|entry| *entry == "C-cleanup")
        .unwrap();
    assert!(b_position < a_position);
    assert!(c_position < a_position);
    drop(trace);
    scenario.run(b.dispose()).unwrap();
    scenario.run(c.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct JoinConsumer {
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for JoinConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &JOIN_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let _a = context.context().service::<ContractA>().unwrap();
        let _b = context.context().service::<ContractB>().unwrap();
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                trace.lock().unwrap().push("join-cleanup");
                Ok(())
            })))
        }))
    }
}

#[test]
fn d11_join_leaves_if_either_provider_is_withdrawn() {
    let scenario = Scenario::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let a = scenario
        .run(scenario.runtime().root().provide::<ContractA>(()))
        .unwrap()
        .unwrap();
    let b = scenario
        .run(scenario.runtime().root().provide::<ContractB>(()))
        .unwrap()
        .unwrap();
    let join = scenario
        .run(scenario.runtime().root().mount(
            JoinConsumer {
                trace: Arc::clone(&trace),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(join.wait_settled()).unwrap();
    scenario.run(a.dispose()).unwrap();
    assert_eq!(
        scenario.run(join.wait_settled()).unwrap(),
        FiberState::Pending
    );
    assert_eq!(trace.lock().unwrap().as_slice(), ["join-cleanup"]);
    assert_eq!(
        scenario.run(b.wait_forward()).unwrap().unwrap(),
        linorun_core::EffectForwardOutcome::Completed
    );
    scenario.run(join.dispose()).unwrap();
    scenario.run(b.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

struct NeverCycleA;
struct NeverCycleB;

impl Component for NeverCycleA {
    type Config = ();
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CYCLE_A_DESCRIPTOR
    }
    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        panic!("cycle A must remain pending")
    }
}

impl Component for NeverCycleB {
    type Config = ();
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CYCLE_B_DESCRIPTOR
    }
    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        panic!("cycle B must remain pending")
    }
}

#[test]
fn d12_mutual_requirements_stay_pending_without_committed_edges() {
    let scenario = Scenario::new();
    let a = scenario
        .run(scenario.runtime().root().mount(NeverCycleA, ()))
        .unwrap()
        .unwrap();
    let b = scenario
        .run(scenario.runtime().root().mount(NeverCycleB, ()))
        .unwrap()
        .unwrap();
    assert_eq!(scenario.run(a.wait_settled()).unwrap(), FiberState::Pending);
    assert_eq!(scenario.run(b.wait_settled()).unwrap(), FiberState::Pending);
    let snapshot = scenario.snapshot().unwrap();
    assert!(snapshot.bindings.is_empty());
    assert!(
        snapshot
            .fibers
            .iter()
            .filter(|fiber| fiber.id == a.id() || fiber.id == b.id())
            .all(|fiber| fiber.episode.is_none() && fiber.relied_by.is_empty())
    );
    scenario.run(a.dispose()).unwrap();
    scenario.run(b.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn d13_intermediate_provider_restart_drains_downstream_then_reactivates_once() {
    let scenario = Scenario::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let b_activations = Arc::new(AtomicUsize::new(0));
    let c_activations = Arc::new(AtomicUsize::new(0));
    let (a, b, c) = mount_chain(&scenario, &trace, &b_activations, &c_activations);
    trace.lock().unwrap().clear();

    scenario.run(b.restart()).unwrap().unwrap();
    assert_eq!(scenario.run(b.wait_settled()).unwrap(), FiberState::Active);
    assert_eq!(scenario.run(c.wait_settled()).unwrap(), FiberState::Active);
    assert_eq!(b_activations.load(Ordering::SeqCst), 2);
    assert_eq!(c_activations.load(Ordering::SeqCst), 2);
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["C-cleanup", "B-cleanup", "B-start", "C-start"]
    );
    scenario.run(c.dispose()).unwrap();
    scenario.run(b.dispose()).unwrap();
    scenario.run(a.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn s02_dependency_chain_shutdown_preserves_reverse_dependency_order() {
    let scenario = Scenario::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let b_activations = Arc::new(AtomicUsize::new(0));
    let c_activations = Arc::new(AtomicUsize::new(0));
    let _handles = mount_chain(&scenario, &trace, &b_activations, &c_activations);
    trace.lock().unwrap().clear();

    let report = scenario.run(scenario.runtime().shutdown()).unwrap();
    assert!(report.cleanup_errors.is_empty());
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["C-cleanup", "B-cleanup", "A-cleanup"]
    );
    assert_eq!(
        scenario.snapshot().unwrap().runtime_state,
        linorun_core::RuntimeState::Stopped
    );
}

static NODE_A_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("conformance/node-a@1", vec![], vec![]));
static NODE_B_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("conformance/node-b@1", vec![], vec![]));
static NODE_C_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("conformance/node-c@1", vec![], vec![]));

struct NodeC;
struct NodeB;
struct NodeA;

static DEPENDENCY_PARENT_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>("conformance/dependency-parent@1", vec![], vec![])
});

struct DependencyParent {
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for DependencyParent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DEPENDENCY_PARENT_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            scope
                .mount(
                    &route,
                    AConsumer {
                        name: "child-cleanup",
                        trace,
                    },
                    (),
                )
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(None)
        }))
    }
}

impl Component for NodeC {
    type Config = ();
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &NODE_C_DESCRIPTOR
    }
    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

impl Component for NodeB {
    type Config = ();
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &NODE_B_DESCRIPTOR
    }
    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        Activation::Once(Box::pin(async move {
            scope
                .mount(&route, NodeC, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(None)
        }))
    }
}

impl Component for NodeA {
    type Config = ();
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &NODE_A_DESCRIPTOR
    }
    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        Activation::Once(Box::pin(async move {
            scope
                .mount(&route, NodeB, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(None)
        }))
    }
}

#[test]
fn p03_nested_children_are_all_removed_before_the_parent_completion() {
    let scenario = Scenario::new();
    let a = scenario
        .run(scenario.runtime().root().mount(NodeA, ()))
        .unwrap()
        .unwrap();
    let active = scenario.run(scenario.runtime().wait_quiescent()).unwrap();
    assert_eq!(active.fibers.len(), 4);
    scenario.run(a.dispose()).unwrap();
    let removed = scenario.snapshot().unwrap();
    assert_eq!(removed.fibers.len(), 1);
    assert!(removed.fibers[0].parent.is_none());
    scenario.run(scenario.runtime().shutdown()).unwrap();
}

#[test]
fn p02_parent_tree_does_not_replace_declared_dependency_ordering() {
    let scenario = Scenario::new();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let provider = scenario
        .run(scenario.runtime().root().mount(
            AProvider {
                trace: Arc::clone(&trace),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(provider.wait_settled()).unwrap();
    let parent = scenario
        .run(scenario.runtime().root().mount(
            DependencyParent {
                trace: Arc::clone(&trace),
            },
            (),
        ))
        .unwrap()
        .unwrap();
    scenario.run(scenario.runtime().wait_quiescent()).unwrap();
    trace.lock().unwrap().clear();

    scenario.run(provider.dispose()).unwrap();
    assert_eq!(scenario.run(parent.state()).unwrap(), FiberState::Active);
    assert_eq!(
        trace.lock().unwrap().as_slice(),
        ["child-cleanup", "A-cleanup"]
    );
    scenario.run(parent.dispose()).unwrap();
    scenario.run(scenario.runtime().shutdown()).unwrap();
}
