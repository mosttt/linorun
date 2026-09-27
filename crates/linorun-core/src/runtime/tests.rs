use std::{
    any::Any,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use futures_channel::oneshot;
use futures_executor::block_on;
use futures_util::stream;

use crate::{CleanupError, Component, ComponentDescriptor, ServiceContract, ServiceRef, effect};

use super::*;

#[derive(Clone, Copy)]
struct ThreadSpawner;

impl TaskSpawner for ThreadSpawner {
    fn spawn(&self, task: RuntimeTask) {
        thread::spawn(move || block_on(task));
    }
}

static DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>("test/runtime-component@1", Vec::new(), Vec::new())
});

struct OnceComponent {
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for OnceComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        self.trace.lock().unwrap().push("activate");
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                trace.lock().unwrap().push("cleanup");
                Ok(())
            })))
        }))
    }
}

#[test]
fn mount_activate_dispose_is_a_complete_episode() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let fiber = runtime
            .root()
            .mount(
                OnceComponent {
                    trace: Arc::clone(&trace),
                },
                (),
            )
            .await
            .unwrap();

        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        assert_eq!(fiber.dispose().await, DisposeReport::default());
        assert_eq!(fiber.state().await, FiberState::Disposed);
        assert_eq!(*trace.lock().unwrap(), ["activate", "cleanup"]);
        assert_eq!(runtime.shutdown().await, ShutdownReport::default());
    });
}

struct StepsComponent {
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for StepsComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let first = Arc::clone(&self.trace);
        let second = Arc::clone(&self.trace);
        Activation::Steps(Box::pin(stream::iter([
            Ok(effect::inverse(move || async move {
                first.lock().unwrap().push("first");
                Ok(())
            })),
            Ok(effect::inverse(move || async move {
                second.lock().unwrap().push("second");
                Ok(())
            })),
        ])))
    }
}

#[test]
fn activation_steps_cleanup_in_strict_lifo_order() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let fiber = runtime
            .root()
            .mount(
                StepsComponent {
                    trace: Arc::clone(&trace),
                },
                (),
            )
            .await
            .unwrap();

        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        fiber.dispose().await;
        assert_eq!(*trace.lock().unwrap(), ["second", "first"]);
        runtime.shutdown().await;
    });
}

struct FailingComponent {
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for FailingComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let trace = Arc::clone(&self.trace);
        Activation::Steps(Box::pin(stream::iter([
            Ok(effect::inverse(move || async move {
                trace.lock().unwrap().push("rolled-back");
                Ok(())
            })),
            Err(ActivationFailure::user("activation failed")),
        ])))
    }
}

#[test]
fn activation_failure_rolls_back_before_failed_state() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let fiber = runtime
            .root()
            .mount(
                FailingComponent {
                    trace: Arc::clone(&trace),
                },
                (),
            )
            .await
            .unwrap();

        assert_eq!(fiber.wait_settled().await, FiberState::Failed);
        assert_eq!(*trace.lock().unwrap(), ["rolled-back"]);
        fiber.dispose().await;
        runtime.shutdown().await;
    });
}

struct CleanupFailureComponent;

impl Component for CleanupFailureComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        Activation::Once(Box::pin(async {
            Ok(Some(effect::inverse(|| async {
                Err(CleanupError::user("cleanup failed"))
            })))
        }))
    }
}

#[test]
fn cleanup_failure_is_reported_without_blocking_disposal() {
    block_on(async {
        let runtime = Runtime::builder(ThreadSpawner).build();
        let fiber = runtime
            .root()
            .mount(CleanupFailureComponent, ())
            .await
            .unwrap();
        assert_eq!(fiber.wait_settled().await, FiberState::Active);

        let report = fiber.dispose().await;
        assert_eq!(report.cleanup_errors.len(), 1);
        assert_eq!(report.cleanup_errors[0].cause.to_string(), "cleanup failed");
        runtime.shutdown().await;
    });
}

struct InertialComponent {
    first_gate: Mutex<Option<oneshot::Receiver<()>>>,
    activations: AtomicUsize,
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for InertialComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        if self.activations.fetch_add(1, Ordering::SeqCst) == 0 {
            let gate = self
                .first_gate
                .lock()
                .unwrap()
                .take()
                .expect("the first activation owns the gate");
            let forward_trace = Arc::clone(&self.trace);
            let cleanup_trace = Arc::clone(&self.trace);
            return Activation::Once(Box::pin(async move {
                forward_trace.lock().unwrap().push("first-started");
                let _ = gate.await;
                forward_trace.lock().unwrap().push("first-landed");
                Ok(Some(effect::inverse(move || async move {
                    cleanup_trace.lock().unwrap().push("first-cleaned");
                    Ok(())
                })))
            }));
        }

        self.trace.lock().unwrap().push("second-activated");
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

#[test]
fn restart_waits_for_the_running_step_then_diverts_at_its_boundary() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (release, gate) = oneshot::channel();
        let runtime = Runtime::builder(ThreadSpawner).build();
        let fiber = runtime
            .root()
            .mount(
                InertialComponent {
                    first_gate: Mutex::new(Some(gate)),
                    activations: AtomicUsize::new(0),
                    trace: Arc::clone(&trace),
                },
                (),
            )
            .await
            .unwrap();

        assert_eq!(fiber.state().await, FiberState::Loading);
        fiber.restart().await.unwrap();
        assert_eq!(fiber.state().await, FiberState::Loading);
        release.send(()).unwrap();

        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        assert_eq!(
            *trace.lock().unwrap(),
            [
                "first-started",
                "first-landed",
                "first-cleaned",
                "second-activated"
            ]
        );
        fiber.dispose().await;
        runtime.shutdown().await;
    });
}

struct ScopedEffectComponent {
    trace: Arc<Mutex<Vec<&'static str>>>,
    handle: Mutex<Option<oneshot::Sender<EffectHandle>>>,
}

impl Component for ScopedEffectComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let forward_trace = Arc::clone(&self.trace);
        let cleanup_trace = Arc::clone(&self.trace);
        let handle = self
            .handle
            .lock()
            .unwrap()
            .take()
            .expect("activation publishes one effect handle");
        Activation::Once(Box::pin(async move {
            let effect = scope
                .effect(
                    "worker",
                    effect::once(move || async move {
                        forward_trace.lock().unwrap().push("effect-forward");
                        Ok(Some(effect::inverse(move || async move {
                            cleanup_trace.lock().unwrap().push("effect-cleanup");
                            Ok(())
                        })))
                    }),
                )
                .await?;
            effect.wait_forward().await?;
            let _ = handle.send(effect);
            Ok(None)
        }))
    }
}

#[test]
fn scope_effect_can_be_disposed_without_unloading_its_fiber() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (send_handle, receive_handle) = oneshot::channel();
        let runtime = Runtime::builder(ThreadSpawner).build();
        let fiber = runtime
            .root()
            .mount(
                ScopedEffectComponent {
                    trace: Arc::clone(&trace),
                    handle: Mutex::new(Some(send_handle)),
                },
                (),
            )
            .await
            .unwrap();
        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        let effect = receive_handle.await.unwrap();

        assert_eq!(
            effect.wait_forward().await,
            Ok(EffectForwardOutcome::Completed)
        );
        assert_eq!(effect.dispose().await, EffectDisposeReport::default());
        assert_eq!(fiber.state().await, FiberState::Active);
        assert_eq!(*trace.lock().unwrap(), ["effect-forward", "effect-cleanup"]);

        fiber.dispose().await;
        assert_eq!(*trace.lock().unwrap(), ["effect-forward", "effect-cleanup"]);
        runtime.shutdown().await;
    });
}

struct LogContract;

#[derive(Clone)]
struct LogClient(ServiceRef<LogContract>);

struct LogEndpoint(Arc<Mutex<Vec<String>>>);

impl ErasedEndpoint for LogEndpoint {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

impl ServiceContract for LogContract {
    const ID: &'static str = "test/runtime-log@1";

    type Provider = Arc<Mutex<Vec<String>>>;
    type Intercept = Vec<&'static str>;
    type Client = LogClient;

    fn identity_intercept() -> Self::Intercept {
        Vec::new()
    }

    fn merge_intercept(base: &Self::Intercept, overlay: &Self::Intercept) -> Self::Intercept {
        base.iter().chain(overlay).copied().collect()
    }

    fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint> {
        Arc::new(LogEndpoint(provider))
    }

    fn client(reference: ServiceRef<Self>) -> Self::Client {
        LogClient(reference)
    }
}

impl LogClient {
    fn record(&self, event: &'static str) {
        let (endpoint, call) = self.0.dispatch_parts();
        let endpoint = endpoint
            .as_any()
            .downcast_ref::<LogEndpoint>()
            .expect("contract endpoint type is fixed");
        endpoint
            .0
            .lock()
            .unwrap()
            .push(format!("{event}:{:?}", call.intercept));
    }
}

static PROVIDER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>("test/log-provider@1", Vec::new(), vec![LogContract::ID])
});
static CONSUMER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "test/log-consumer@1",
        vec![crate::Requirement::new::<LogContract>(Some(vec![
            "declared",
        ]))],
        Vec::new(),
    )
});

struct LogProvider {
    trace: Arc<Mutex<Vec<String>>>,
}

impl Component for LogProvider {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PROVIDER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        let provider = Arc::clone(&self.trace);
        let cleanup = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            let binding = scope
                .provide::<LogContract>(&route, provider)
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            binding.wait_forward().await?;
            Ok(Some(effect::inverse(move || async move {
                cleanup.lock().unwrap().push("provider-cleanup".to_owned());
                Ok(())
            })))
        }))
    }
}

struct LogConsumer;

impl Component for LogConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CONSUMER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let client = context
            .context()
            .service::<LogContract>()
            .expect("declared service is committed before activation");
        client.record("consumer-start");
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                client.record("consumer-cleanup");
                Ok(())
            })))
        }))
    }
}

#[test]
fn provider_waits_for_committed_consumer_cleanup() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let provider = runtime
            .root()
            .mount(
                LogProvider {
                    trace: Arc::clone(&trace),
                },
                (),
            )
            .await
            .unwrap();
        let consumer = runtime.root().mount(LogConsumer, ()).await.unwrap();

        assert_eq!(provider.wait_settled().await, FiberState::Active);
        assert_eq!(consumer.wait_settled().await, FiberState::Active);
        provider.dispose().await;
        assert_eq!(consumer.wait_settled().await, FiberState::Pending);

        {
            let trace = trace.lock().unwrap();
            let consumer_cleanup = trace
                .iter()
                .position(|entry| entry.starts_with("consumer-cleanup"))
                .unwrap();
            let provider_cleanup = trace
                .iter()
                .position(|entry| entry == "provider-cleanup")
                .unwrap();
            assert!(consumer_cleanup < provider_cleanup);
            assert!(trace[0].contains("declared"));
        }

        consumer.dispose().await;
        runtime.shutdown().await;
    });
}

static PARENT_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("test/parent@1", Vec::new(), Vec::new()));
static CHILD_DESCRIPTOR: LazyLock<ComponentDescriptor> =
    LazyLock::new(|| ComponentDescriptor::new::<()>("test/child@1", Vec::new(), Vec::new()));

struct ChildComponent {
    trace: Arc<Mutex<Vec<&'static str>>>,
}

impl Component for ChildComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CHILD_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                trace.lock().unwrap().push("child-cleanup");
                Ok(())
            })))
        }))
    }
}

struct ParentComponent {
    trace: Arc<Mutex<Vec<&'static str>>>,
    child: Mutex<Option<oneshot::Sender<FiberHandle>>>,
}

impl Component for ParentComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PARENT_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        let trace = Arc::clone(&self.trace);
        let sender = self
            .child
            .lock()
            .unwrap()
            .take()
            .expect("parent publishes one child");
        Activation::Once(Box::pin(async move {
            let child = scope
                .mount(&route, ChildComponent { trace }, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            let _ = sender.send(child);
            Ok(None)
        }))
    }
}

#[test]
fn parent_disposal_retires_child_and_waits_for_its_removal() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (send_child, receive_child) = oneshot::channel();
        let runtime = Runtime::builder(ThreadSpawner).build();
        let parent = runtime
            .root()
            .mount(
                ParentComponent {
                    trace: Arc::clone(&trace),
                    child: Mutex::new(Some(send_child)),
                },
                (),
            )
            .await
            .unwrap();
        assert_eq!(parent.wait_settled().await, FiberState::Active);
        let child = receive_child.await.unwrap();
        assert_eq!(child.wait_settled().await, FiberState::Active);

        parent.dispose().await;
        assert_eq!(parent.state().await, FiberState::Disposed);
        assert_eq!(child.state().await, FiberState::Disposed);
        assert_eq!(*trace.lock().unwrap(), ["child-cleanup"]);
        runtime.shutdown().await;
    });
}

static VERSIONED_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<String>("test/versioned@1", Vec::new(), Vec::new())
});

struct VersionedComponent {
    implementation: &'static str,
    trace: Arc<Mutex<Vec<String>>>,
}

impl Component for VersionedComponent {
    type Config = String;

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &VERSIONED_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, config: Arc<Self::Config>) -> Activation {
        let implementation = self.implementation;
        let value = config.as_str().to_owned();
        self.trace
            .lock()
            .unwrap()
            .push(format!("activate:{implementation}:{value}"));
        let trace = Arc::clone(&self.trace);
        Activation::Once(Box::pin(async move {
            Ok(Some(effect::inverse(move || async move {
                trace
                    .lock()
                    .unwrap()
                    .push(format!("cleanup:{implementation}:{value}"));
                Ok(())
            })))
        }))
    }
}

#[test]
fn config_and_implementation_updates_create_new_episodes() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let fiber = runtime
            .root()
            .mount(
                VersionedComponent {
                    implementation: "first",
                    trace: Arc::clone(&trace),
                },
                "A".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(fiber.wait_settled().await, FiberState::Active);

        assert_eq!(
            fiber.update_config(7_u32).await,
            Err(UpdateError::DescriptorMismatch)
        );
        fiber.update_config("B".to_owned()).await.unwrap();
        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        fiber
            .replace(VersionedComponent {
                implementation: "second",
                trace: Arc::clone(&trace),
            })
            .await
            .unwrap();
        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        fiber.dispose().await;

        assert_eq!(
            *trace.lock().unwrap(),
            [
                "activate:first:A",
                "cleanup:first:A",
                "activate:first:B",
                "cleanup:first:B",
                "activate:second:B",
                "cleanup:second:B",
            ]
        );
        runtime.shutdown().await;
    });
}

#[test]
fn snapshot_and_wait_quiescent_project_actor_state_atomically() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (release, gate) = oneshot::channel();
        let runtime = Runtime::builder(ThreadSpawner).build();
        let initial = runtime.snapshot().await;
        assert!(initial.quiescent);
        assert_eq!(initial.runtime_state, RuntimeState::Running);
        assert_eq!(initial.fibers.len(), 1);

        let fiber = runtime
            .root()
            .mount(
                InertialComponent {
                    first_gate: Mutex::new(Some(gate)),
                    activations: AtomicUsize::new(0),
                    trace,
                },
                (),
            )
            .await
            .unwrap();
        let loading = runtime.snapshot().await;
        assert!(!loading.quiescent);
        let fiber_snapshot = loading
            .fibers
            .iter()
            .find(|snapshot| snapshot.id == fiber.id())
            .unwrap();
        assert_eq!(fiber_snapshot.state, FiberState::Loading);
        assert_eq!(
            fiber_snapshot.episode.as_ref().unwrap().phase,
            EpisodePhase::Loading
        );

        release.send(()).unwrap();
        let settled = runtime.wait_quiescent().await;
        assert!(settled.quiescent);
        assert!(settled.sequence > loading.sequence);
        assert_eq!(
            settled
                .fibers
                .iter()
                .find(|snapshot| snapshot.id == fiber.id())
                .unwrap()
                .state,
            FiberState::Active
        );

        fiber.dispose().await;
        runtime.shutdown().await;
    });
}

#[test]
fn event_stream_is_monotonic_and_runtime_stopped_is_last() {
    block_on(async {
        let (events, receiver) = std::sync::mpsc::channel();
        let runtime = Runtime::builder(ThreadSpawner).event_sender(events).build();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let fiber = runtime
            .root()
            .mount(
                OnceComponent {
                    trace: Arc::clone(&trace),
                },
                (),
            )
            .await
            .unwrap();
        assert_eq!(fiber.wait_settled().await, FiberState::Active);
        fiber.dispose().await;
        runtime.shutdown().await;

        let mut observed = Vec::new();
        loop {
            let event = receiver
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("RuntimeStopped event must be delivered");
            let stopped = matches!(event.kind, RuntimeEventKind::RuntimeStopped);
            observed.push(event);
            if stopped {
                break;
            }
        }
        assert!(matches!(
            observed[0].kind,
            RuntimeEventKind::FiberInserted {
                fiber: ROOT_FIBER,
                parent: None,
                ..
            }
        ));
        assert!(
            observed
                .windows(2)
                .all(|events| events[1].sequence == events[0].sequence + 1)
        );
        assert!(matches!(
            observed.last().unwrap().kind,
            RuntimeEventKind::RuntimeStopped
        ));
        let retired = observed.iter().position(
            |event| matches!(event.kind, RuntimeEventKind::FiberRetired { fiber: id } if id == fiber.id()),
        );
        let removed = observed.iter().position(
            |event| matches!(event.kind, RuntimeEventKind::FiberRemoved { fiber: id } if id == fiber.id()),
        );
        assert!(retired < removed);
    });
}

#[test]
fn root_effect_dispose_is_idempotent_and_inverse_runs_once() {
    block_on(async {
        let cleanups = Arc::new(AtomicUsize::new(0));
        let cleanup_counter = Arc::clone(&cleanups);
        let runtime = Runtime::builder(ThreadSpawner).build();
        let effect = runtime
            .root()
            .effect(
                "idempotent",
                effect::once(move || async move {
                    Ok(Some(effect::inverse(move || async move {
                        cleanup_counter.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })))
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            effect.wait_forward().await,
            Ok(EffectForwardOutcome::Completed)
        );

        let (first, second) = futures_util::future::join(effect.dispose(), effect.dispose()).await;
        assert_eq!(first, second);
        assert_eq!(cleanups.load(Ordering::SeqCst), 1);
        runtime.shutdown().await;
    });
}

#[test]
fn disposing_inflight_effect_waits_for_step_and_reports_disposed() {
    block_on(async {
        let (release, gate) = oneshot::channel();
        let cleanups = Arc::new(AtomicUsize::new(0));
        let cleanup_counter = Arc::clone(&cleanups);
        let runtime = Runtime::builder(ThreadSpawner).build();
        let effect = runtime
            .root()
            .effect(
                "inflight",
                effect::once(move || async move {
                    let _ = gate.await;
                    Ok(Some(effect::inverse(move || async move {
                        cleanup_counter.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })))
                }),
            )
            .await
            .unwrap();

        let disposer = effect.clone();
        let (send_report, receive_report) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = send_report.send(block_on(disposer.dispose()));
        });
        loop {
            let snapshot = runtime.snapshot().await;
            if snapshot.effects.iter().any(|snapshot| {
                snapshot.id == effect.id()
                    && snapshot.state == EffectSnapshotState::Disposing
                    && snapshot.running_step.is_some()
            }) {
                break;
            }
            thread::yield_now();
        }
        release.send(()).unwrap();

        let report = receive_report
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(report, EffectDisposeReport::default());
        assert_eq!(
            effect.wait_forward().await,
            Ok(EffectForwardOutcome::Disposed)
        );
        assert_eq!(cleanups.load(Ordering::SeqCst), 1);
        runtime.shutdown().await;
    });
}

#[test]
fn independent_effect_failure_rolls_back_without_failing_runtime() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let cleanup = Arc::clone(&trace);
        let runtime = Runtime::builder(ThreadSpawner).build();
        let effect = runtime
            .root()
            .effect(
                "failure",
                effect::steps(move || {
                    stream::iter([
                        Ok(effect::inverse(move || async move {
                            cleanup.lock().unwrap().push("rollback");
                            Ok(())
                        })),
                        Err(EffectError::user("forward failed")),
                    ])
                }),
            )
            .await
            .unwrap();

        let forward = effect.wait_forward().await.unwrap_err();
        assert_eq!(forward.to_string(), "forward failed");
        let report = effect.dispose().await;
        assert_eq!(
            report.forward_error.as_ref().unwrap().to_string(),
            "forward failed"
        );
        assert!(report.cleanup_errors.is_empty());
        assert_eq!(*trace.lock().unwrap(), ["rollback"]);
        assert!(runtime.wait_quiescent().await.quiescent);
        runtime.shutdown().await;
    });
}

#[test]
fn inverse_panic_is_reported_and_earlier_inverse_continues() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let earlier = Arc::clone(&trace);
        let runtime = Runtime::builder(ThreadSpawner).build();
        let effect = runtime
            .root()
            .effect(
                "panic",
                effect::steps(move || {
                    stream::iter([
                        Ok(effect::inverse(move || async move {
                            earlier.lock().unwrap().push("earlier");
                            Ok(())
                        })),
                        Ok(effect::inverse(|| async move {
                            panic!("cleanup panic");
                        })),
                    ])
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            effect.wait_forward().await,
            Ok(EffectForwardOutcome::Completed)
        );

        let report = effect.dispose().await;
        assert_eq!(report.cleanup_errors.len(), 1);
        assert_eq!(
            report.cleanup_errors[0].cause.cause.kind,
            crate::FailureKind::Panic
        );
        assert_eq!(*trace.lock().unwrap(), ["earlier"]);
        runtime.shutdown().await;
    });
}

#[test]
fn zero_inverse_effect_completes_without_placeholder_cleanup() {
    block_on(async {
        let runtime = Runtime::builder(ThreadSpawner).build();
        let effect = runtime
            .root()
            .effect("empty", effect::once(|| async { Ok(None) }))
            .await
            .unwrap();
        assert_eq!(
            effect.wait_forward().await,
            Ok(EffectForwardOutcome::Completed)
        );
        assert_eq!(effect.dispose().await, EffectDisposeReport::default());
        assert!(runtime.snapshot().await.effects.is_empty());
        runtime.shutdown().await;
    });
}

#[test]
fn isolation_selects_binding_and_interception_folds_into_client() {
    block_on(async {
        let default_trace = Arc::new(Mutex::new(Vec::new()));
        let tenant_trace = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let root = runtime.root();
        let tenant = root.create_realm();
        let tenant_root = root
            .isolate::<LogContract>(tenant)
            .unwrap()
            .intercept::<LogContract>(vec!["tenant-overlay"]);

        let default_binding = root
            .provide::<LogContract>(Arc::clone(&default_trace))
            .await
            .unwrap();
        let tenant_binding = tenant_root
            .provide::<LogContract>(Arc::clone(&tenant_trace))
            .await
            .unwrap();
        let default_consumer = root.mount(LogConsumer, ()).await.unwrap();
        let tenant_consumer = tenant_root.mount(LogConsumer, ()).await.unwrap();
        assert_eq!(default_consumer.wait_settled().await, FiberState::Active);
        assert_eq!(tenant_consumer.wait_settled().await, FiberState::Active);

        assert_eq!(
            default_trace.lock().unwrap()[0],
            "consumer-start:[\"declared\"]"
        );
        assert_eq!(
            tenant_trace.lock().unwrap()[0],
            "consumer-start:[\"declared\", \"tenant-overlay\"]"
        );

        tenant_binding.dispose().await;
        assert_eq!(tenant_consumer.wait_settled().await, FiberState::Pending);
        assert_eq!(default_consumer.state().await, FiberState::Active);

        let foreign_runtime = Runtime::builder(ThreadSpawner).build();
        let foreign_realm = foreign_runtime.root().create_realm();
        assert!(root.isolate::<LogContract>(foreign_realm).is_err());

        default_consumer.dispose().await;
        tenant_consumer.dispose().await;
        default_binding.dispose().await;
        runtime.shutdown().await;
        foreign_runtime.shutdown().await;
    });
}

#[test]
fn shutdown_report_collects_root_cleanup_failures() {
    block_on(async {
        let runtime = Runtime::builder(ThreadSpawner).build();
        let effect = runtime
            .root()
            .effect(
                "shutdown-failure",
                effect::once(|| async {
                    Ok(Some(effect::inverse(|| async {
                        Err(CleanupError::user("shutdown cleanup failed"))
                    })))
                }),
            )
            .await
            .unwrap();
        effect.wait_forward().await.unwrap();

        let report = runtime.shutdown().await;
        assert_eq!(report.cleanup_errors.len(), 1);
        assert_eq!(
            report.cleanup_errors[0].cause.to_string(),
            "shutdown cleanup failed"
        );
    });
}

struct FailingChild;

impl Component for FailingChild {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CHILD_DESCRIPTOR
    }

    fn activate(&self, _: ComponentContext, _: Arc<Self::Config>) -> Activation {
        Activation::Once(Box::pin(async {
            Ok(Some(effect::inverse(|| async {
                Err(CleanupError::user("child cleanup failed"))
            })))
        }))
    }
}

struct ParentWithFailingChild;

impl Component for ParentWithFailingChild {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PARENT_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let scope = context.scope().clone();
        let route = context.context().clone();
        Activation::Once(Box::pin(async move {
            scope
                .mount(&route, FailingChild, ())
                .await
                .map_err(|error| ActivationFailure::user(error.to_string()))?;
            Ok(None)
        }))
    }
}

#[test]
fn parent_dispose_report_includes_descendant_cleanup_failure() {
    block_on(async {
        let runtime = Runtime::builder(ThreadSpawner).build();
        let parent = runtime
            .root()
            .mount(ParentWithFailingChild, ())
            .await
            .unwrap();
        assert_eq!(parent.wait_settled().await, FiberState::Active);

        let report = parent.dispose().await;
        assert_eq!(report.cleanup_errors.len(), 1);
        assert_eq!(
            report.cleanup_errors[0].cause.to_string(),
            "child cleanup failed"
        );
        runtime.shutdown().await;
    });
}

struct EpochContract;

#[derive(Clone)]
struct EpochClient(ServiceRef<EpochContract>);

struct EpochProvider {
    name: &'static str,
    trace: Arc<Mutex<Vec<String>>>,
}

impl ErasedEndpoint for EpochProvider {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

impl ServiceContract for EpochContract {
    const ID: &'static str = "test/epoch@1";

    type Provider = EpochProvider;
    type Intercept = ();
    type Client = EpochClient;

    fn identity_intercept() -> Self::Intercept {}

    fn merge_intercept(_: &Self::Intercept, _: &Self::Intercept) -> Self::Intercept {}

    fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint> {
        Arc::new(provider)
    }

    fn client(reference: ServiceRef<Self>) -> Self::Client {
        EpochClient(reference)
    }
}

impl EpochClient {
    fn record(&self, phase: &'static str) {
        let (endpoint, _) = self.0.dispatch_parts();
        let endpoint = endpoint
            .as_any()
            .downcast_ref::<EpochProvider>()
            .expect("epoch endpoint type is fixed");
        endpoint
            .trace
            .lock()
            .unwrap()
            .push(format!("{phase}:{}", endpoint.name));
    }
}

static EPOCH_PROVIDER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>("test/epoch-provider@1", Vec::new(), vec![EpochContract::ID])
});
static EPOCH_CONSUMER_DESCRIPTOR: LazyLock<ComponentDescriptor> = LazyLock::new(|| {
    ComponentDescriptor::new::<()>(
        "test/epoch-consumer@1",
        vec![crate::Requirement::new::<EpochContract>(None)],
        Vec::new(),
    )
});

struct EpochProviderComponent {
    control: Mutex<Option<oneshot::Sender<(Scope, Context)>>>,
}

impl Component for EpochProviderComponent {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &EPOCH_PROVIDER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let _ = self
            .control
            .lock()
            .unwrap()
            .take()
            .expect("provider publishes one control context")
            .send((context.scope().clone(), context.context().clone()));
        Activation::Once(Box::pin(async { Ok(None) }))
    }
}

struct GatedEpochConsumer {
    gate: Mutex<Option<oneshot::Receiver<()>>>,
}

impl Component for GatedEpochConsumer {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &EPOCH_CONSUMER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        let client = context.context().service::<EpochContract>().unwrap();
        let gate = self
            .gate
            .lock()
            .unwrap()
            .take()
            .expect("first episode owns the gate");
        client.record("started");
        Activation::Once(Box::pin(async move {
            let _ = gate.await;
            client.record("landed");
            Ok(Some(effect::inverse(move || async move {
                client.record("cleanup");
                Ok(())
            })))
        }))
    }
}

#[test]
fn same_provider_rebind_recovers_loading_episode_with_old_committed_endpoint() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let runtime = Runtime::builder(ThreadSpawner).build();
        let (send_control, receive_control) = oneshot::channel();
        let provider = runtime
            .root()
            .mount(
                EpochProviderComponent {
                    control: Mutex::new(Some(send_control)),
                },
                (),
            )
            .await
            .unwrap();
        assert_eq!(provider.wait_settled().await, FiberState::Active);
        let (scope, context) = receive_control.await.unwrap();
        let first_binding = scope
            .provide::<EpochContract>(
                &context,
                EpochProvider {
                    name: "first",
                    trace: Arc::clone(&trace),
                },
            )
            .await
            .unwrap();
        let first_binding_id = runtime.snapshot().await.bindings[0].id;

        let (release, gate) = oneshot::channel();
        let consumer = runtime
            .root()
            .mount(
                GatedEpochConsumer {
                    gate: Mutex::new(Some(gate)),
                },
                (),
            )
            .await
            .unwrap();
        assert_eq!(consumer.state().await, FiberState::Loading);

        let old = first_binding.clone();
        let (send_old_report, receive_old_report) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = send_old_report.send(block_on(old.dispose()));
        });
        loop {
            if runtime.snapshot().await.bindings.is_empty() {
                break;
            }
            thread::yield_now();
        }
        let second_binding = scope
            .provide::<EpochContract>(
                &context,
                EpochProvider {
                    name: "second",
                    trace: Arc::clone(&trace),
                },
            )
            .await
            .unwrap();
        let second_binding_id = runtime.snapshot().await.bindings[0].id;
        assert_ne!(first_binding_id, second_binding_id);

        release.send(()).unwrap();
        assert_eq!(consumer.wait_settled().await, FiberState::Active);
        receive_old_report
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        let snapshot = runtime.snapshot().await;
        let committed = snapshot
            .fibers
            .iter()
            .find(|snapshot| snapshot.id == consumer.id())
            .unwrap()
            .episode
            .as_ref()
            .unwrap()
            .committed_bindings[0];
        assert_eq!(committed, first_binding_id);
        assert_eq!(*trace.lock().unwrap(), ["started:first", "landed:first"]);

        second_binding.dispose().await;
        assert_eq!(consumer.wait_settled().await, FiberState::Pending);
        assert_eq!(trace.lock().unwrap().last().unwrap(), "cleanup:first");
        consumer.dispose().await;
        provider.dispose().await;
        runtime.shutdown().await;
    });
}

#[test]
fn disposing_inflight_step_stream_collects_current_item_and_stops_continuation() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (sender, receiver) = futures_channel::mpsc::unbounded();
        let runtime = Runtime::builder(ThreadSpawner).build();
        let effect = runtime
            .root()
            .effect("stream", effect::steps(move || receiver))
            .await
            .unwrap();

        let first_trace = Arc::clone(&trace);
        sender
            .unbounded_send(Ok(effect::inverse(move || async move {
                first_trace.lock().unwrap().push(1);
                Ok(())
            })))
            .unwrap();
        loop {
            let snapshot = runtime.snapshot().await;
            let effect_snapshot = snapshot
                .effects
                .iter()
                .find(|snapshot| snapshot.id == effect.id())
                .unwrap();
            if effect_snapshot.inverses.len() == 1 && effect_snapshot.running_step.is_some() {
                break;
            }
            thread::yield_now();
        }

        let disposer = effect.clone();
        let (send_report, receive_report) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = send_report.send(block_on(disposer.dispose()));
        });
        loop {
            if runtime.snapshot().await.effects.iter().any(|snapshot| {
                snapshot.id == effect.id()
                    && snapshot.state == EffectSnapshotState::Disposing
                    && snapshot.running_step.is_some()
            }) {
                break;
            }
            thread::yield_now();
        }
        let second_trace = Arc::clone(&trace);
        sender
            .unbounded_send(Ok(effect::inverse(move || async move {
                second_trace.lock().unwrap().push(2);
                Ok(())
            })))
            .unwrap();

        receive_report
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            effect.wait_forward().await,
            Ok(EffectForwardOutcome::Disposed)
        );
        assert_eq!(*trace.lock().unwrap(), [2, 1]);
        runtime.shutdown().await;
    });
}

#[test]
fn fiber_wide_cleanup_orders_entries_across_effects_by_global_sequence() {
    block_on(async {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let (outer_sender, outer_receiver) = futures_channel::mpsc::unbounded();
        let runtime = Runtime::builder(ThreadSpawner).build();
        let outer = runtime
            .root()
            .effect("outer", effect::steps(move || outer_receiver))
            .await
            .unwrap();

        let first = Arc::clone(&trace);
        outer_sender
            .unbounded_send(Ok(effect::inverse(move || async move {
                first.lock().unwrap().push(1);
                Ok(())
            })))
            .unwrap();
        loop {
            if runtime
                .snapshot()
                .await
                .effects
                .iter()
                .any(|effect| effect.id == outer.id() && effect.inverses.len() == 1)
            {
                break;
            }
            thread::yield_now();
        }

        let middle = Arc::clone(&trace);
        let inner = runtime
            .root()
            .effect(
                "inner",
                effect::once(move || async move {
                    Ok(Some(effect::inverse(move || async move {
                        middle.lock().unwrap().push(2);
                        Ok(())
                    })))
                }),
            )
            .await
            .unwrap();
        inner.wait_forward().await.unwrap();

        let last = Arc::clone(&trace);
        outer_sender
            .unbounded_send(Ok(effect::inverse(move || async move {
                last.lock().unwrap().push(3);
                Ok(())
            })))
            .unwrap();
        drop(outer_sender);
        outer.wait_forward().await.unwrap();

        runtime.shutdown().await;
        assert_eq!(*trace.lock().unwrap(), [3, 2, 1]);
    });
}
