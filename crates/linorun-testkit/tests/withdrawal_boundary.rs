use std::sync::{Arc, Mutex};

use linorun_core::{
    ActivationFailure, CallContext, ComponentContext, FiberHandle, FiberState, component,
    service_contract, service_impl,
};
use linorun_testkit::{Controlled, Scenario, controlled};

service_contract! {
    contract Signal {
        id: "withdrawal-boundary/signal@1",
        methods: [async fn ping() -> ();],
    }
}

struct SignalService;

#[service_impl]
impl SignalProvider for SignalService {
    async fn ping(&self, _: CallContext<()>) {}
}

struct ProviderConfig {
    context: Arc<Mutex<Option<ComponentContext>>>,
}

#[component(id = "withdrawal-boundary/provider@1", provides = [Signal])]
async fn provider(
    context: ComponentContext,
    config: Arc<ProviderConfig>,
) -> Result<(), ActivationFailure> {
    *config.context.lock().unwrap() = Some(context);
    Ok(())
}

struct WatcherConfig {
    cleanup: Mutex<Option<Controlled<()>>>,
    provider_context: Arc<Mutex<Option<ComponentContext>>>,
    child: Arc<Mutex<Option<FiberHandle>>>,
}

#[component(id = "withdrawal-boundary/watcher@1", requires = [Signal])]
async fn watcher(
    context: ComponentContext,
    config: Arc<WatcherConfig>,
) -> Result<(), ActivationFailure> {
    let child = context
        .mount(
            provider(),
            ProviderConfig {
                context: Arc::clone(&config.provider_context),
            },
        )
        .await?;
    assert_eq!(child.wait_settled().await, FiberState::Active);
    *config.child.lock().unwrap() = Some(child);
    let cleanup = config.cleanup.lock().unwrap().take().unwrap();
    // This later inverse keeps the child alive until the parent has started leaving.
    context
        .on_cleanup("pause before retiring child", move || async move {
            cleanup.await;
            Ok(())
        })
        .await?;
    Ok(())
}

#[test]
fn manual_withdrawal_ignores_uncommitted_parent_watchers() {
    let scenario = Scenario::new();
    let (release_parent, parent_cleanup) = controlled();
    let provider_context = Arc::new(Mutex::new(None));
    let child_handle = Arc::new(Mutex::new(None));
    let root_binding = scenario
        .run(
            scenario
                .runtime()
                .root()
                .provide::<Signal>(Arc::new(SignalService)),
        )
        .unwrap()
        .unwrap();
    let parent = scenario
        .run(scenario.runtime().root().mount(
            watcher(),
            WatcherConfig {
                cleanup: Mutex::new(Some(parent_cleanup)),
                provider_context: Arc::clone(&provider_context),
                child: Arc::clone(&child_handle),
            },
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(parent.wait_settled()).unwrap(),
        FiberState::Active
    );
    let child = child_handle.lock().unwrap().clone().unwrap();
    let child_context = provider_context.lock().unwrap().clone().unwrap();

    // Root's service disappears; the parent leaves but pauses before retiring its child.
    assert!(scenario.run(root_binding.dispose()).is_err());
    assert!(scenario.run(parent.dispose()).is_err());
    assert_eq!(scenario.run(parent.state()).unwrap(), FiberState::Unloading);
    assert_eq!(scenario.run(child.state()).unwrap(), FiberState::Active);

    // The parent still watches this slot, but committed Root's old binding, not the child's.
    // The child must withdraw immediately without acquiring a wait on parent removal.
    let child_binding = scenario
        .run(child_context.provide::<Signal>(Arc::new(SignalService)))
        .unwrap()
        .unwrap();
    assert!(
        scenario
            .run(child_binding.dispose())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
    assert_eq!(scenario.run(parent.state()).unwrap(), FiberState::Unloading);
    assert!(scenario.run(root_binding.dispose()).is_err());

    // Root's original withdrawal still waits for the parent's captured Episode to finish.
    release_parent.complete(()).unwrap();
    assert!(
        scenario
            .run(parent.dispose())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
    assert_eq!(scenario.run(child.state()).unwrap(), FiberState::Disposed);
    assert!(
        scenario
            .run(child_binding.dispose())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
    assert!(
        scenario
            .run(root_binding.dispose())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
    assert!(
        scenario
            .run(scenario.runtime().shutdown())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
}
