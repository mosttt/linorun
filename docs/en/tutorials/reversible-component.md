# Resource lifecycle: setup, failure, and cleanup

[English](reversible-component.md) | [简体中文](../../zh/tutorials/reversible-component.md)

Components open connections, start tasks, and publish services. As soon as each resource is ready,
register how to close it. If the second step fails, the first resource already has a cleanup owner.

## Start with an ordinary async function

Import component, ComponentContext, and ActivationFailure from `linorun_core`:

```rust
#[component(id = "app/workspace@1")]
async fn workspace(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    println!("Open workspace");
    ctx.on_cleanup("close workspace", || async {
        println!("Close workspace");
        Ok(())
    }).await?;

    println!("Start worker");
    ctx.on_cleanup("stop worker", || async {
        println!("Stop worker");
        Ok(())
    }).await?;
    Ok(())
}
```

Output illustrates ordering. In a real application, move the resource or stop signal into the cleanup
closure and await shutdown before returning. Stopping prints Stop worker before Close workspace.
Returning Err partway through also runs previously registered cleanup. A cleanup error enters the
report and does not stop other cleanup; the runtime does not retry the same callback automatically.

on_cleanup waits for registration before returning. Its EffectHandle supports early disposal.
If shutdown begins during resource acquisition, an already running activation or Effect step can
still finish registering cleanup. Acquire and register within the same tracked step; an expired
Context cannot accept new registrations.
Register each resource immediately after acquiring it rather than completing several fallible steps
before registering the first resource. For a complete background-task implementation, see
[application.rs](../../../crates/linorun-core/examples/application.rs): cleanup signals the worker
to stop and awaits it, while consumers can still call their old service during cleanup.

## 1. When to use explicit steps

Several resources do not require Steps. An ordinary async component's entire setup function is one
inertial lifecycle step. Multiple on_cleanup calls register individual resources without splitting
that setup into multiple lifecycle steps.

Use Activation::steps(stream) or the original Activation::Steps when the runtime should check for
dependency changes or restart requests between setup stages. The complete advanced example remains
in [reversible_component.rs](../../../crates/linorun-core/examples/reversible_component.rs):

~~~bash
cargo run -p linorun-core --example reversible_component
~~~


~~~text
step 1: open workspace  -> yield close workspace
step 2: start worker    -> yield stop worker
stream end              -> Fiber Active
~~~


Each yield is a checking boundary: record this stage's cleanup, then decide whether to continue or unwind.

## 2. Construct a step stream

This is the main part of a hand-written Component::activate. stream::unfold carries the stage,
configuration, and execution trace:

~~~rust
let state = (0_u8, config.name.clone(), Arc::clone(&self.trace));

Activation::Steps(Box::pin(stream::unfold(
    state,
    |(step, name, trace)| async move {
        match step {
            0 => {
                trace.lock().unwrap().push(format!("open {name}"));
                let cleanup_name = name.clone();
                let cleanup_trace = Arc::clone(&trace);
                let inverse = effect::inverse(move || async move {
                    cleanup_trace.lock().unwrap()
                        .push(format!("close {cleanup_name}"));
                    Ok(())
                });
                Some((Ok::<_, ActivationFailure>(inverse), (1, name, trace)))
            }
            1 => {
                trace.lock().unwrap().push(format!("start {name} worker"));
                let cleanup_name = name.clone();
                let cleanup_trace = Arc::clone(&trace);
                let inverse = effect::inverse(move || async move {
                    cleanup_trace.lock().unwrap()
                        .push(format!("stop {cleanup_name} worker"));
                    Ok(())
                });
                Some((Ok(inverse), (2, name, trace)))
            }
            _ => None,
        }
    },
)))
~~~


Yield the matching inverse only after acquisition succeeds. If the first stage succeeds and the
second fails, the first inverse still runs. component_descriptor! defines static identity, config,
and service declarations; ordinary Rust code expresses steps and resource ownership.

## 3. Cleanup order and the accumulator

The accumulator is the cleanup list for this activation. seq records registration order. At most
one inverse runs at a time within a Fiber.

| seq | Acquisition | Cleanup | Disposal order |
| --- | --- | --- | --- |
| 1 | open workspace | close workspace | 2 |
| 2 | start worker | stop worker | 1 |

~~~text
settled at Active
after activation: ["open personal-agent", "start personal-agent worker"]
after disposal:   ["open personal-agent", "start personal-agent worker", "stop personal-agent worker", "close personal-agent"]
~~~


The second inverse must complete or fail before the first starts. This guarantee applies across
all Effects in the component, not only within an individual list.

## 4. Restart during setup

An already-started async step finishes first; the runtime does not start another version alongside
it. This is called inertia:

~~~text
Loading(revision 0, step 1)
    restart: latest revision becomes 1
    step 1 continues running
    step 1 lands and its inverse is committed
Unloading(revision 0)
    inverse runs
Loading(revision 1, new episode)
~~~


After completion, the runtime records the inverse and compares the latest ActivationKey. A changed
revision triggers cleanup of the old activation before a new one begins. Stepwise components check
at each step boundary; ordinary async functions check after the whole function completes.

## 5. Startup failure and recovery

The stream may return:

~~~rust
Err(ActivationFailure::user("worker failed to start"))
~~~


The runtime stops polling the stream, cleans up registered resources in reverse order, and then
enters Failed. wait_settled returns FiberState::Failed; read the failure from a snapshot rather
than expecting an Err from the waiting API.

~~~rust
if fiber.wait_settled().await == FiberState::Failed {
    fiber.restart().await?;
    let retried = fiber.wait_settled().await;
    assert!(matches!(retried, FiberState::Active | FiberState::Failed));
}
~~~


restart, update_config, or replace explicitly clear failure. A service appearing repeatedly does
not automatically retry a Failed component.

## 6. Cleanup failures

An inverse returns Result<(), CleanupError>:

~~~rust
effect::inverse(move || async move {
    stop_worker().await
        .map_err(|error| CleanupError::user(error.to_string()))?;
    Ok(())
})
~~~


Errors enter DisposeReport.cleanup_errors while other cleanup continues. The runtime attempts a
registered inverse at most once. A resource-specific retry policy belongs in its shutdown function.
A failure report is not a guarantee that external state was restored.

## 7. Resource ownership

Output only demonstrates order. A real inverse should own the resource it closes:

~~~rust
let worker = start_worker().await?;

Ok(effect::inverse(move || async move {
    worker.stop().await?;
    Ok(())
}))
~~~


Drop cannot replace asynchronous shutdown. Applications must await dispose or shutdown; signaling
and joining background tasks belong in their cleanup functions.

Continue with [services](typed-service.md), [component authoring](../api/component-authoring.md), or
[Effect semantics](../semantics/effects.md). Custom EffectDriver, synchronous/asynchronous iteration,
and the original Once/Steps interfaces all remain available.
