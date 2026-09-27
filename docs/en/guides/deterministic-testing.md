# Test component lifecycles without sleep

[简体中文](../../zh/guides/deterministic-testing.md) | [English](deterministic-testing.md)

`linorun-testkit` provides an executor advanced by the test. Decide when an async step completes, then
assert component state, cleanup counts, or withdrawal order without guessing how many milliseconds a
background task needs.

Applications depend on `linorun-core`, and tests depend on `linorun-testkit`. With a local checkout, point
the dependencies at those crates. For example, when the application and Linorun checkout are sibling
directories:

```toml
[dependencies]
linorun-core = { path = "../linorun/crates/linorun-core" }

[dev-dependencies]
linorun-testkit = { path = "../linorun/crates/linorun-testkit" }
```

## Verify that a component starts and stops

`Scenario` connects a Runtime, executor, and event recorder. This example can go directly into a test
file:

```rust
use linorun_core::{component, ActivationFailure, ComponentContext, FiberState};
use linorun_testkit::Scenario;

#[component(id = "test/worker@1")]
async fn worker(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    ctx.on_cleanup("stop worker", || async { Ok(()) }).await?;
    Ok(())
}

#[test]
fn worker_starts_and_stops() {
    let scenario = Scenario::new();
    let fiber = scenario.run(scenario.runtime().mount(worker(), ()))
        .unwrap().unwrap();

    assert_eq!(scenario.run(fiber.wait_settled()).unwrap(), FiberState::Active);
    let report = scenario.run(fiber.dispose()).unwrap();
    assert!(report.cleanup_errors.is_empty());
    assert_eq!(scenario.run(fiber.state()).unwrap(), FiberState::Disposed);

    let report = scenario.run(scenario.runtime().shutdown()).unwrap();
    assert!(report.cleanup_errors.is_empty());
}
```

Replace `worker()` and its configuration with the component under test, then assert relevant state or
resource behavior. `scenario.run(future)` advances both the supplied future and tasks spawned by Runtime
until that future completes.

The two `unwrap` calls on mount handle different errors: the outer result contains the executor's
`DriveError`; the inner result contains mount's `MountError`. If the supplied future is still pending
but no task can make progress, `run` returns `DriveError`. This may be expected when a test deliberately
pauses a step.

## Pause async work at a chosen boundary

`controlled()` returns a future and a handle that completes it. This test first checks that work is
waiting, then releases it and verifies cleanup runs exactly once:

```rust
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use linorun_core::{effect, EffectForwardOutcome};
use linorun_testkit::{controlled, Scenario};

#[test]
fn cleanup_runs_after_controlled_start() {
    let scenario = Scenario::new();
    let (release, gate) = controlled();
    let calls = Arc::new(AtomicUsize::new(0));
    let cleanup_calls = Arc::clone(&calls);

    let handle = scenario.run(scenario.runtime().effect(
        "controlled worker",
        effect::once(move || async move {
            gate.await;
            Ok(Some(effect::inverse(move || async move {
                cleanup_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })))
        }),
    )).unwrap().unwrap();

    scenario.executor().run_until_stalled();
    assert!(scenario.run(handle.wait_forward()).is_err());
    assert!(!scenario.snapshot().unwrap().quiescent);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(handle.wait_forward()).unwrap().unwrap(),
        EffectForwardOutcome::Completed,
    );

    let report = scenario.run(handle.dispose()).unwrap();
    assert!(report.cleanup_errors.is_empty());
    scenario.run(handle.dispose()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    scenario.run(scenario.runtime().shutdown()).unwrap();
}
```

Only one clone of `Control` can successfully complete the future through `complete`. Keep a control handle alive
until release. Dropping all handles makes the corresponding future panic, exposing a boundary the test
forgot to release.

When `run` returns `DriveError`, it drops the supplied future. To preserve local state inside that
particular waiting future, pin it first and pass a mutable reference to the executor:

```rust
let mut waiting = Box::pin(handle.wait_forward());
assert!(scenario.run(waiting.as_mut()).is_err());
release.complete(()).unwrap();
let result = scenario.run(waiting.as_mut()).unwrap();
```

Dropping a caller's waiting future does not cancel startup or cleanup work already managed by Runtime.
To test stopping during acquisition, pause a step, request `dispose` or `shutdown`, then release the
step to acquire its resource and call `on_cleanup`. Registration should succeed while the same Episode
remains installed and unload cleanup has not started, with cleanup running before shutdown completes.
Registration from an expired Episode should be rejected.

## Advance multi-step startup one item at a time

Use `step_stream` to control each item of a stream:

```rust
use linorun_core::{effect, ActivationFailure};
use linorun_testkit::step_stream;

let (steps, stream) = step_stream::<Result<linorun_core::InverseFn, ActivationFailure>>();
let activation = linorun_core::Activation::steps(stream);
assert!(steps.push(Ok(effect::inverse(|| async { Ok(()) }))).is_ok());
steps.close();
```

A test component can return this `activation` from `activate`. Each `push` supplies one startup step's
cleanup operation; `close` marks the end of the stream. For an effect stream, change the item error type
to `EffectError` and pass the stream through `effect::steps`. For finer scheduling control, `run_one()`
advances one runnable task, and `run_until_stalled()` runs until all tasks are waiting.

## Inspect state snapshots

`scenario.snapshot()` returns a consistent `RuntimeSnapshot` from one Actor turn and automatically runs
`check_snapshot` to validate parent/child relationships, resource ownership, and dependency indexes.

Tests can inspect fields relevant to their scenario, such as finding the record for `fiber.id()` and
asserting its `state`. `normalize_snapshot` removes some diagnostic detail and produces sorted Fiber,
Binding, and Effect projections for structural comparisons.

Deeper checks include service visibility and provider-resource ownership, CommittedView and reverse
dependency edges, Episode/Effect identities and cleanup ownership, unique cleanup sequence numbers within
a Fiber, and at most one running cleanup per Fiber. Structural checks do not replace assertions about
business results; combine them with resource logs or event ordering.

## Check service withdrawal and cleanup order

`scenario.trace().drain()` obtains Runtime events. `event_tag(&event.kind)` maps concrete events to
`EventTag` for convenient comparison. Match the original `RuntimeEventKind` when component or resource
identity matters.

A whole-provider shutdown scenario typically needs to establish this order:

```text
Binding removed from new resolution
    < Consumer finishes cleanup
    < Provider closes underlying resource
```

Compare the ordering required by the scenario without fixing a global order for unrelated components.
Event `sequence` increases within one Runtime and can be used to compare events.

For manual withdrawal, distinguish notification from waiting: all slot watchers are notified, but only
consumers whose CommittedView actually contains that BindingId are captured, together with their
EpisodeIds at withdrawal time. Once a captured Episode leaves, that waiter is satisfied. A subsequent
consumer activation must not extend withdrawal even if it is still Loading. The same captured Episode
can also satisfy the boundary by remaining Active with its key matching the latest ActivationKey.

A retired instance whose own cleanup is finished has released its old Episode, even while children
remain and its public state is `Unloading`. Do not substitute public `wait_settled`, entire-subtree
`Disposed`, or settlement of all watchers for this condition: they wait for different work. Cover both
a watcher not actually using the Binding and a new consumer Episode still waiting after the old one ends.

Complete examples are available in the workspace:

- [Lifecycle updates](../../../crates/linorun-testkit/tests/lifecycle_updates.rs): restart, configuration, and implementation replacement.
- [Dependencies and contexts](../../../crates/linorun-testkit/tests/dependencies_context.rs): declarations, Realms, and call metadata.
- [Effect concurrency](../../../crates/linorun-testkit/tests/effects_concurrency.rs): manual disposal, ordering, and in-flight steps.
- [Topology chains](../../../crates/linorun-testkit/tests/topology_chains.rs): dependency chains and provider withdrawal.
- [Shutdown and observability](../../../crates/linorun-testkit/tests/shutdown_observability.rs): shutdown reports and snapshots.
- [Conformance edges](../../../crates/linorun-testkit/tests/conformance_edges.rs): scenarios spanning several features.

## Use the independent reference model

`ReferenceModel` is a separately implemented lightweight state machine. Tests explicitly submit mount,
begin, step, failure, unload, Binding, and Effect operations through `ModelCommand`, then read a sorted
`ModelSnapshot`. It rejects invalid transitions, duplicate identities, cleanup order violations, and
removing components that still own resources.

Tests currently arrange model commands manually. The model does not automatically translate Runtime
events into commands or perform full differential replay. Passing model checks means that the explicitly
arranged model operations satisfy its constraints; it cannot alone prove all real Runtime executions
correct. Combine it with real scenarios, snapshot validation, and event-order assertions.

See [Deterministic tests](../../../crates/linorun-testkit/tests/deterministic.rs) for model examples, and
[Conformance strategy](../testing/conformance-strategy.md) and [Scenario catalog](../testing/scenario-catalog.md)
for specification coverage.
