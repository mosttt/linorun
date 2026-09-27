[简体中文](../../zh/semantics/effects.md) | [English](effects.md)

# Effects and cleanup

## 1. Definition

An effect is a change to the running environment together with an inverse that can restore it:

    effect : Context -> Context × Inverse

The component supplies the inverse; Runtime tracks and invokes it.

Typical effects include:

- Registering an event listener.
- Opening a listening port.
- Starting a background task.
- Providing a service.
- Mounting a child component.
- Installing a route or timer.

An irreversible network send, log output, or payment is not an exactly reversible effect.
It can only be treated as an emission, or compensated by business logic.

## 2. Rust representation

Conceptual interface:

~~~rust
type InverseFuture =
    Pin<Box<dyn Future<Output = Result<(), CleanupError>> + Send>>;

type InverseFn =
    Box<dyn FnOnce() -> InverseFuture + Send>;

struct AppliedEffect {
    inverse: InverseFn,
}
~~~

FnOnce expresses that an inverse can be consumed only once. The runtime state machine must
still guarantee at-most-once invocation under concurrent disposal.

## 3. EffectRecord

~~~rust
struct EffectRecord {
    id: EffectId,
    episode: EpisodeId,
    label: Arc<str>,
    state: EffectState,
    running_step: Option<StepId>,
    forward_error: Option<EffectError>,
    cleanup_errors: Vec<CleanupFailure>,
    completion: Arc<EffectCompletion>,
}

struct InverseEntry {
    seq: u64,
    owner: InverseOwner,
    label: Option<Arc<str>>,
    action: InverseAction,
}

enum InverseAction {
    User(InverseFn),
    RetireFiber(FiberId),
    WithdrawBinding {
        binding: BindingId,
        slot: BindingSlot,
        endpoint: Arc<dyn ErasedEndpoint>,
    },
}

enum InverseOwner {
    Activation,
    Effect(EffectId),
}
~~~

The Episode accumulator exclusively owns InverseEntries. Activation output uses
InverseOwner::Activation. Each Scope::effect call corresponds to one EffectRecord, whose output
uses InverseOwner::Effect(id). EffectRecord does not duplicate the seq list; manual disposal
and snapshots filter the accumulator by owner.

Ordinary component inverses use User. Mount and provide use the Core-owned RetireFiber and
WithdrawBinding actions respectively. Core actions participate in the same seq/LIFO selection,
but do not run through user Futures.

Each successfully yielded inverse receives an increasing Fiber-local seq. Seq represents actual
application order, not effect creation order. Application order is defined precisely as the order
in which the Actor accepts Yield. Components must yield the inverse immediately after the
external change succeeds; Runtime does not infer earlier, unobservable changes inside user Futures.

If two concurrent forwards in one Fiber modify external state through noncommutative operations,
the component must await the first EffectHandle::wait_forward before starting the second.
Only commutative changes may run concurrently without ordering. Otherwise, the Actor's Yield
acceptance order may differ from actual write times inside Futures, and Core cannot restore
the correct order on behalf of the component.

A step task polls exactly one driver step and transfers Yield/Done/error together with the driver
to the Actor in its completion. Only `Yield { has_next: true }` makes that driver a resumable
continuation. If the Actor chooses to continue, it moves the driver into the next RuntimeTask.
Other outcomes, or dispose/divert, drop it after state commits. A task does not independently
cross the Actor boundary to poll the next step.

Once the Actor commits StepId to the record and transfers the driver to RuntimeTask, that step
has started. Whether the executor has physically polled it yet does not introduce another
cancellable state. Subsequent dispose/Target changes wait for the step to complete and only
prevent the Actor from transferring its continuation again.

Provide and mount are Core-owned atomic effects. Within one Actor turn, they directly create
EffectRecord, inverse entry, and topology. Their forward completes with the commit, without
creating a user driver or StepId; cleanup still enters the same Episode accumulator.

## 4. Supported execution forms

### 4.1 Synchronous single step

    forward()
      -> inverse

The effect is Armed when forward returns.

### 4.2 Asynchronous single step

    await forward()
      -> inverse

The entire Future is one inertial step. Dispose or a Target change cannot simply drop it.
Once the Future completes, Runtime collects its inverse and then enters cleanup.

### 4.3 Synchronous iterator

    yield inverse 1
    yield inverse 2
    yield inverse 3

Each yield creates an effect boundary.

### 4.4 Asynchronous iterator

    await step 1
    yield inverse 1
    await step 2
    yield inverse 2

When Target changes or manual disposal occurs:

- An await step that has already started must complete.
- Its inverse is collected.
- No subsequent iteration starts.
- Collected inverses enter LIFO cleanup.

The Actor checks the latest target before committing another iteration. An already committed
iteration still completes and hands back its cleanup.

## 5. Accumulator

The Episode accumulator stores produced inverses:

    apply e1 -> g1
    apply e2 -> g2
    apply e3 -> g3

Recovery order:

    g3
    g2
    g1

Formally, the composed accumulator is:

    g1 ∘ g2 ∘ g3

When applying the composed function, the most recently produced inverse acts first.

## 6. Serial cleanup

At most one inverse runs in a Fiber at a time.

Selection algorithm:

1. Collect accumulator entries covered by the target disposal requests.
2. Select the entry with the largest seq.
3. Take the entry and store cross-turn state in `cleanup_running`.
4. Await outside the Actor.
5. Clear `cleanup_running` when completion returns.
6. Repeat.

A Fiber's cleanup scheduler always waits for the current inverse to finish before selecting
the next inverse.

If an effect requested for cleanup has reached its forward boundary and has no inverse in
either the accumulator or cleanup_running, complete EffectCompletion and remove EffectRecord
directly. Empty effects, effects that fail before their first Yield, and effects whose entries
have all finished follow this rule. Runtime does not create no-op inverses.

## 7. Manual disposal

EffectHandle::dispose:

- It is idempotent.
- The first call closes subsequent iterations of that effect.
- It waits for the current step to complete.
- It undoes inverses already produced by that effect.
- Later calls wait for the same final outcome.
- Fiber unload does not see inverses that have already completed and been removed.

EffectHandle::wait_forward completes at the driver's terminal boundary: natural exhaustion gives
Completed, manual dispose/Fiber unload gives Disposed, and a forward error gives Err.
It does not wait for inverse cleanup.

Each synchronous iterator yield is collected in application order. Once the Actor observes
disposal, it starts no further iteration.

An asynchronous iterator may have a step in flight. Runtime cannot claim that forward has been
undone before receiving its inverse.

Fiber-wide unload closes every effect driver in the Episode. Each driver completes only its
current step, without starting a continuation. Runtime begins Fiber-wide LIFO cleanup only
after every driver reaches its boundary.

After a Fiber enters Unloading, manual disposal that has not yet started only adds a cleanup
request. It must wait for the withdrawal guard together with the entire Episode and cannot
start an inverse early.

## 8. Nested effects

Nested effects still receive seq values in actual yield/registration order.

For example:

    outer yields g1       seq 1
    inner yields g2       seq 2
    outer yields g3       seq 3

Fiber unload runs g3, g2, then g1.

Manually disposing inner selects only inverses owned by inner. A subsequent Fiber unload
processes only the remaining accumulator inverses.

## 9. provide and mount

Provide and mount are ordinary effects, not separate lifecycle systems:

### provide

The Core-generated EffectRecord label is `linorun:provide:{ServiceId}`.

forward:

1. Scope provide checks the Component descriptor's declarations. RootContext provide is a Host-owned Root effect and is not constrained by an ordinary Component's provides set.
2. Check that the BindingSlot is free.
3. The ServiceContract adapter produces an endpoint Arc and its ContractVTable.
4. In the same Actor commit, insert BindingRecord and add the unregistering inverse to the Episode accumulator, retaining the endpoint Arc.
5. EffectRecord enters Armed directly, and wait_forward yields Completed.

Fiber-wide topology withdrawal:

1. When the Provider enters Unloading, Core immediately removes BindingRecord from the store.
2. The Actor notifies Consumers.
3. The withdrawal guard waits for every committed Consumer to leave.
4. When the cleanup scheduler reaches this seq, the provide inverse releases Core's endpoint reference.

Manual topology withdrawal:

1. The cleanup scheduler selects this seq.
2. The Actor removes BindingRecord and notifies Consumers only if the Slot still contains that BindingId. If the old Binding was withdrawn and a new Binding occupies the Slot, it does not remove the new record.
3. The provide inverse captures `(FiberId, EpisodeId)` pairs committed to the exact withdrawn BindingId. It waits for those Episodes to end, or for the same Episode to be Active with a current key.
4. The inverse releases Core's endpoint reference.

This is a soft withdrawal boundary: if the same Provider returns, a Consumer may continue using
the old endpoint Arc in its old Episode. This wait does not require unrelated independent Scope
effects on an Active Fiber to finish. An affected Fiber's own activation failure does not change
the provide inverse's outcome.

The Actor notifies every Slot watcher, but watchers without a commitment to this Binding do not
join the wait. Watcher membership alone could create a purely Core-owned cleanup wait cycle
between two Fibers that never used each other's services. Once a captured Episode ends,
internally Inactive, a later Episode, or Fiber removal all satisfy the wait. Later Episodes do
not extend the old Binding's wait. A retired Consumer that finishes its own L-Unload has ended
the old Episode even if it still awaits children and has public state Unloading. This differs
from the public settled condition of `FiberHandle::wait_settled` and does not wait for child O-Remove.

A separate effect owns the underlying service resource. Its inverse runs at its seq during
Fiber-wide cleanup and completes asynchronous shutdown of that resource.

### mount

The Core-generated EffectRecord label is `linorun:mount:{ComponentId}`.

forward:

1. Atomically create the parent's mount EffectRecord/InverseEntry and the child Fiber.
2. The child records this mount EffectId.
3. EffectRecord enters Armed directly.
4. Return FiberHandle.

inverse:

1. O-Retire the child.
2. Clear child.mount_effect.
3. Complete the mount effect's cleanup without waiting for child cleanup.

Natural parent cleanup executes this inverse. When the Host calls FiberHandle::dispose, it does
not install a second inverse. Instead, it directly invokes the same O-Retire reducer, and may
directly take only the Core-generated entry whose action exactly matches `RetireFiber(child)`.
Its own waiter returns DisposeReport only after FiberCompletion.

## 10. Forward failure

Failure before forward produces an inverse means that the step left no installed change.

When an independent Scope effect succeeds in earlier steps and fails in a later step:

- The failing step produces no inverse.
- The driver enters Disposing.
- Only collected inverses owned by that EffectId are selected.
- Those inverses run in descending seq order.
- EffectHandle completion retains forward and cleanup errors.
- The Fiber's lifecycle stays unchanged.

When an Activation driver succeeds in earlier steps and fails in a later step:

- Collected inverses remain in the Episode accumulator.
- The failing step produces no inverse.
- Activation performs L-Raise.
- The accumulator undergoes normal cleanup.

A forward that changes external state but cannot return an inverse violates EffectContract.
Core cannot repair this automatically.

## 11. Inverse failure

When an inverse returns CleanupError or panics with unwinding:

- Wrap the error in CleanupFailure with owner/seq/label.
- Treat the inverse as attempted; do not retry it.
- Continue with earlier inverses.
- Aggregate all errors in the dispose report.

Cleanup errors do not prevent the lifecycle from reaching its final outcome.

## 12. Effect API constraints

- Effects may be created only through the current Fiber's Scope.
- Provide and mount must also belong to effects.
- Scope accepts new effects only in the current Episode while Reloading or Active.
- A user inverse must not call Scope/RootContext to start effect, provide, or mount. This is an EffectContract for trusted components, not an additional call token maintained by Core. Temporary asynchronous work needed by an inverse must finish before that inverse's Future returns.
- An inverse must not await disposal of another ordinary EffectHandle in the same Fiber. The Fiber's scheduler is already waiting for the current inverse, so mutual waiting creates a component-level deadlock. FiberHandle::dispose uses separate O-Retire orchestration, but an inverse must not await disposal of its own Fiber or an ancestor: its own removal waits for the current inverse, and ancestor removal also waits for the current Fiber. Both create component-authored wait cycles.
- EffectHandle does not expose InverseFn.
- Runtime snapshots show label, state, seq, and error.
- Dropping EffectHandle is not disposal; Runtime owns its lifecycle.

`Scope::on_cleanup` and the matching ComponentContext convenience method register an inverse
for an acquired resource. They use a dedicated `RegisterCleanup` command. After shutdown or
retirement begins, registration remains allowed when it belongs to the same Episode, unload
cleanup has not started, and an activation or effect step is still running. If the owner is
already Unloading, registration immediately joins pending cleanup. Expired Scopes remain
rejected, as do new effects, provisions, and mounts. This exception preserves resource cleanup
while an existing step finishes.

`Activation::run` treats the entire async body as one Once step. An `on_cleanup` registration
uses the same seq and accumulator without splitting that activation step. Target changes are
decided only when the whole body returns. Use `Activation::steps` to check Target between
activation steps.

The public `effect::sync`, `effect::once`, `effect::iter`, and `effect::steps` constructors adapt
a synchronous single step, Future, Iterator, and Stream respectively. `effect::inverse` boxes a
cleanup closure into InverseFn. These constructors only create drivers; the RuntimeTask must
invoke the forward closure after EffectStarted commits. Custom execution forms implement
EffectDriver/IntoEffectDriver. A custom `into_driver` must also only construct a driver; it must
not block, panic, or perform external changes early.

## 13. Relationship to RAII

RAII suits lexically scoped resources. Linorun effects suit dynamic, long-lived component
resources that can be manually unloaded.

Internally, RAII manages locks, temporary borrows, and task-local objects. Fiber lifecycle
determines when a component's external effects end.
