use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use linorun_core::{
    ActivationFailure, Component, ComponentContext, EffectError, EffectStartError, FiberState,
    component, effect,
};
use linorun_testkit::{Controlled, Scenario, controlled};

struct Acquisition {
    gate: Mutex<Option<Controlled<()>>>,
    acquired: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
}

#[component(id = "cleanup-registration/activation@1")]
async fn activation_acquires(
    context: ComponentContext,
    config: Arc<Acquisition>,
) -> Result<(), ActivationFailure> {
    let gate = config.gate.lock().unwrap().take().unwrap();
    gate.await;
    config.acquired.fetch_add(1, Ordering::SeqCst);
    let closed = Arc::clone(&config.closed);
    context
        .on_cleanup("close activation resource", move || async move {
            closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await?;
    Ok(())
}

#[component(id = "cleanup-registration/effect@1")]
async fn effect_acquires(
    context: ComponentContext,
    config: Arc<Acquisition>,
) -> Result<(), ActivationFailure> {
    let gate = config.gate.lock().unwrap().take().unwrap();
    let effect_context = context.clone();
    context
        .effect(
            "acquire resource",
            effect::once(move || async move {
                gate.await;
                config.acquired.fetch_add(1, Ordering::SeqCst);
                let closed = Arc::clone(&config.closed);
                effect_context
                    .on_cleanup("close effect resource", move || async move {
                        closed.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
                    .map_err(|error| EffectError::user(error.to_string()))?;
                Ok(None)
            }),
        )
        .await?;
    Ok(())
}

fn stop_after_acquisition_started<C: Component<Config = Acquisition>>(
    component: C,
    initial_state: FiberState,
    shutdown: bool,
) {
    let scenario = Scenario::new();
    let (release, gate) = controlled();
    let acquired = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            component,
            Acquisition {
                gate: Mutex::new(Some(gate)),
                acquired: Arc::clone(&acquired),
                closed: Arc::clone(&closed),
            },
        ))
        .unwrap()
        .unwrap();
    scenario.executor().run_until_stalled();
    assert_eq!(scenario.run(fiber.state()).unwrap(), initial_state);
    assert_eq!(acquired.load(Ordering::SeqCst), 0);

    // Stopping must wait for the inertial acquisition to land and register its inverse.
    if shutdown {
        assert!(scenario.run(scenario.runtime().shutdown()).is_err());
    } else {
        assert!(scenario.run(fiber.dispose()).is_err());
    }
    release.complete(()).unwrap();
    let cleanup_errors = if shutdown {
        scenario
            .run(scenario.runtime().shutdown())
            .unwrap()
            .cleanup_errors
    } else {
        scenario.run(fiber.dispose()).unwrap().cleanup_errors
    };
    assert!(cleanup_errors.is_empty());
    assert_eq!(acquired.load(Ordering::SeqCst), 1);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(scenario.run(fiber.state()).unwrap(), FiberState::Disposed);
    assert!(
        scenario
            .run(scenario.runtime().shutdown())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
}

#[test]
fn shutdown_keeps_cleanup_for_in_flight_activation_acquisition() {
    stop_after_acquisition_started(activation_acquires(), FiberState::Loading, true);
}

#[test]
fn shutdown_keeps_cleanup_for_in_flight_effect_acquisition() {
    stop_after_acquisition_started(effect_acquires(), FiberState::Active, true);
}

#[test]
fn fiber_dispose_keeps_cleanup_for_in_flight_activation_acquisition() {
    stop_after_acquisition_started(activation_acquires(), FiberState::Loading, false);
}

#[test]
fn fiber_dispose_keeps_cleanup_for_in_flight_effect_acquisition() {
    stop_after_acquisition_started(effect_acquires(), FiberState::Active, false);
}

struct CapturedContexts {
    contexts: Arc<Mutex<Vec<ComponentContext>>>,
}

#[component(id = "cleanup-registration/captured-context@1")]
async fn capture_context(
    context: ComponentContext,
    config: Arc<CapturedContexts>,
) -> Result<(), ActivationFailure> {
    config.contexts.lock().unwrap().push(context);
    Ok(())
}

#[test]
fn cleanup_registration_rejects_context_from_a_finished_episode() {
    let scenario = Scenario::new();
    let contexts = Arc::new(Mutex::new(Vec::new()));
    let fiber = scenario
        .run(scenario.runtime().root().mount(
            capture_context(),
            CapturedContexts {
                contexts: Arc::clone(&contexts),
            },
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        scenario.run(fiber.wait_settled()).unwrap(),
        FiberState::Active
    );
    let old_context = contexts.lock().unwrap()[0].clone();
    scenario.run(fiber.restart()).unwrap().unwrap();
    assert_eq!(
        scenario.run(fiber.wait_settled()).unwrap(),
        FiberState::Active
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = Arc::clone(&calls);
    let result = scenario
        .run(
            old_context.on_cleanup("stale callback", move || async move {
                callback_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        )
        .unwrap();
    assert!(matches!(result, Err(EffectStartError::InactiveScope)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        scenario
            .run(scenario.runtime().shutdown())
            .unwrap()
            .cleanup_errors
            .is_empty()
    );
}
