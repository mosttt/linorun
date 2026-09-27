# Runtime Actor

[简体中文](../../zh/concurrency/runtime-actor.md) | English

The implementation is split across `runtime/mod.rs` (control plane), `api.rs` (command entry points), `completion.rs` (shared results), and `task.rs` (user task boundaries). `runtime/tests.rs` tests this production implementation.

## 1. Role

The Runtime Actor is the sole owner of mutable Core control-plane state:

~~~rust
struct ActorCore {
    runtime_state: RuntimeState,
    contracts: ContractRegistry,
    bindings: BindingTable,
    watchers: Map<BindingSlot, Set<FiberId>>,
    consumers_by_provider: Map<FiberId, Set<FiberId>>,
    fibers: Map<FiberId, FiberRecord>,
    effect_owners: Map<EffectId, FiberId>,
    // identity/sequence counters, waiters and event sender
}

struct TurnOutput {
    events: Vec<RuntimeEvent>,
    tasks: Vec<RuntimeTask>,
    deferred_drops: Vec<Box<dyn Send + 'static>>,
}
~~~

The single activation StepId is stored in `LifecycleState::Reloading.step`; an effect's single StepId is stored in `EffectRecord.running_step`. Only the corresponding RuntimeTask owns a driver at a time.

FiberId, EpisodeId, EffectId, BindingId, and StepId are allocated by the Actor's identity counters. RealmId is allocated by RuntimeHandle's atomic allocator. Snapshot and event sequences use separate sequence counters.

The Actor serializes changes to:

- the Fiber registry;
- lifecycle state;
- the Binding store;
- watcher and committed consumer indexes;
- EffectRecords;
- the snapshot sequence;
- the event sequence.

Initializing RuntimeCore also creates the Active Root Fiber, `ActivationKey { target: Target { providers: [] }, revision: 0 }`, an empty CommittedView, and the Root Episode. Root uses FiberId(0)/EpisodeId(0), and the default Realm has local 0. Other identities and event/snapshot sequences start at 1. When an event Sender is configured, build's initial commit first emits the synthetic Root's FiberInserted event. Root is constructed directly as the Active base and emits no LBegin/LFinish.

## 2. The Actor does not execute user code

The following run outside the Actor:

- Component activation steps;
- effect forward steps;
- inverses;
- service endpoint calls;
- event Receiver processing, logging, and metrics aggregation.

An Actor turn only performs finite in-memory state changes and task scheduling. Endpoints, Configs, implementations, drivers/continuations, and User InverseActions removed by the reducer must enter deferred_drops. They must not be destroyed while the RuntimeCore borrow is held.

## 3. Commands

Internal commands:

~~~rust
enum Command {
    Mount { /* owner, descriptor, implementation, config, route, reply */ },
    StartEffect { /* owner, label, driver, reply */ },
    RegisterCleanup { /* owner, label, inverse, reply */ },
    Provide { /* owner, route, contract, endpoint, reply */ },
    ActivationFinished { /* fiber, episode, step, completion */ },
    EffectStepFinished { /* fiber, episode, effect, step, completion */ },
    InverseFinished { /* fiber, episode, seq, result */ },
    FiberState { /* fiber, reply */ },
    WaitSettled { /* fiber, reply */ },
    WaitEffectForward { /* effect, completion, reply */ },
    DisposeEffect { /* effect, completion, reply */ },
    Restart { /* fiber, reply */ },
    UpdateConfig { /* fiber, config, type, reply */ },
    Replace { /* fiber, descriptor, implementation, reply */ },
    Dispose { /* fiber, reply */ },
    Snapshot { /* reply */ },
    WaitQuiescent { /* reply */ },
    Shutdown { /* reply */ },
}
~~~

Command names are implementation details. Every command must map to the specified state transitions.

## 4. Actor turn

One turn:

1. Receives a Command.
2. Validates current records and EpisodeId.
3. Applies one finite reducer.
4. Repeatedly scans the registry in FiberId order until no state rule can be applied immediately.
5. Completes settled/shutdown/quiescent waiters whose conditions are met.
6. Ends the reducer and releases the RuntimeCore borrow.
7. Destroys deferred_drops in order.
8. Sends events and spawns user tasks.

Direct command replies are sent within their handlers; condition-based replies are sent during waiter flushing. Reply delivery is not the last post-commit step. The current turn still completes its synchronous work before processing the next command. Receiving an API reply does not mean user tasks have completed; use wait_settled, wait_forward, or wait_quiescent to observe those results.

The Actor does not retain a mutable RuntimeCore borrow across await.

`deferred_drops` is only a post-commit ownership bundle, not lifecycle state. Component Drop implementations must be nonblocking, must not panic, and must not perform resource cleanup. Shutdown logic that needs to wait must be an inverse.

All user tasks and the Actor task are submitted through the Builder's TaskSpawner. spawn schedules only: it must not synchronously poll a task on the current Actor call stack or panic.

WaitSettled, WaitQuiescent, and Shutdown retain Reply waiters when their conditions are unmet. The Actor turn still ends immediately, and later reconciliation completes the waiters when their conditions become true.

## 5. Reconciliation fixed point

After each command's local writes, the Actor reconciles Fibers in BTreeMap/FiberId order. If any rule changes state, another pass runs. The loop stops after a whole pass makes no change. This fixed point completes synchronous consequences of Bindings, revisions, parent retirement, and cleanup completion before the next command is processed, without declaring an extra public total order among independent Fibers.

Each `reconcile_one` handles one Fiber's current state:

~~~text
Inactive + key          -> L-Begin
Reloading + boundary    -> L-Iter / L-Finish / L-Divert
Active + key changed    -> L-Leave
Unloading + guard open   -> request episode cleanup
Unloading + cleanup done -> finish L-Unload
retired Inactive        -> O-Remove when child set empty
~~~

Chains of Leave transitions that need no await propagate through later passes in the same turn. Fibers requiring an activation step, inverse, or withdrawal guard remain in the corresponding transitional state until a completion or later command arrives.

## 6. Target calculation

~~~rust
fn activation_key(
    core: &RuntimeCore,
    fiber: &FiberRecord,
) -> Option<ActivationKey>
~~~

Algorithm:

1. For the synthetic Root, Running returns the empty Target/revision 0; ShuttingDown/Stopped returns None.
2. A retired ordinary Fiber returns None.
3. Resolve BindingSlots in Requirement order.
4. Each Slot must contain a registered BindingRecord.
5. Binding.contract_type must match the Requirement contract and Runtime contract map.
6. The Binding's Provider must be Active and not retired.
7. Collect Provider FiberIds.
8. Append the Fiber's activation revision.

The function only reads ActorCore and does not call component code.

## 7. Scheduling L-Begin

The Actor atomically:

1. Recomputes ActivationKey.
2. Captures the CommittedView and validated declared interception metadata.
3. Allocates an EpisodeId.
4. Writes Reloading.
5. Builds an ActivationDriver::Init from the captured implementation/config/ComponentContext, without calling user code.
6. Writes StepId into `LifecycleState::Reloading.step`.

After commit, the Init driver transfers to a RuntimeTask. The first poll calls `Component::activate` and enters Once/Steps. The task converts a synchronous panic to ActivationFailure and returns the still-valid Exhausted wrapper with the completion.

The Consumer becomes installed as soon as Reloading is written, so the Provider withdrawal guard observes it.

## 8. Step completion

~~~rust
struct ActivationCompletion {
    result: Result<DriverEvent, ActivationFailure>,
    driver: ActivationDriver,
}

struct EffectStepCompletion {
    result: Result<DriverEvent, EffectError>,
    driver: ErasedEffectDriver,
}

Command::ActivationFinished { fiber, episode, step, completion }
Command::EffectStepFinished { fiber, episode, effect, step, completion }
~~~

An activation completion is valid only if the Fiber is still Reloading in the matching Episode and `LifecycleState::Reloading.step` equals the returned StepId. An effect completion requires the matching Episode to remain installed, the corresponding EffectRecord to be Applying or Disposing, and `EffectRecord.running_step` to equal the returned StepId. Normal effect completion may occur during Reloading, Active, or Unloading. A completion failing these identity checks must not change state; its returned driver only enters post-commit deferred-drop.

Then:

- Yield allocates a sequence and adds an inverse owned by InverseOwner::Activation to the accumulator.
- Failure applies L-Raise.
- `Yield { has_next: true }` makes the returned driver a continuation. The Actor compares the latest ActivationKey, then moves it into the next RuntimeTask or destroys it after committing L-Divert.
- A driver returned with final Yield, Done, or Err must not be polled again and is destroyed after the corresponding state commit. Done/final Yield compares the latest ActivationKey to choose L-Finish or L-Divert.

A step task polls one step and returns ownership of the remaining driver with the completion. The Actor only stores StepId. Actor and task never hold the driver simultaneously, and a task cannot cross an Actor boundary on its own. Recording StepId and transferring the driver into RuntimeTask occur in the same Actor commit and define the step's start. Even before TaskSpawner's first poll, later Target/dispose commands only record stop intent and wait for that step completion.

## 9. Effect completion

EffectRecord is not removed before its Applying step completes.

A disposal request changes only EffectState:

    Applying | Armed -> Disposing

After the current step completes, `EffectRecord.running_step` is cleared. Yield collects an inverse owned by InverseOwner::Effect(id). Then:

- A step error stores forward_error, switches to Disposing, and requests cleanup only for that EffectId's entries.
- If manual or whole-Fiber disposal has already marked the Effect Disposing, no next iteration starts; cleanup is requested for the effect.
- If it remains Applying and returned `Yield { has_next: true }`, the driver moves to the next effect RuntimeTask.
- If it remains Applying and returned final Yield/Done, EffectRecord enters Armed with forward outcome Completed.

Drivers that do not continue are destroyed after the corresponding state commit. An independent effect error or manual cleanup does not change the Fiber lifecycle. The whole-Fiber path continues waiting for all driver boundaries and the withdrawal guard.

## 10. Entering Unloading

L-Divert, L-Raise, and L-Leave share the `enter_unloading` reducer. In one turn, the Actor:

1. Changes Reloading/Active to Unloading while retaining the corresponding outcome.
2. Removes Provider-owned BindingRecords from the store.
3. Marks the current Episode's Applying/Armed EffectRecords Disposing.
4. Lets later fixed-point passes expose the change to watcher Consumers.
5. Keeps the Provider Unloading while waiting for step boundaries and the guard.

After commit, new Targets cannot select that Provider. The specific lifecycle event remains LDivert, LRaise, or LLeave according to the entry path. Each effect emits EffectDisposeRequested once when it first becomes Disposing.

## 11. Withdrawal guard

~~~rust
fn is_relied_on(
    core: &RuntimeCore,
    provider: FiberId,
) -> bool
~~~

The source-of-truth condition is:

    any installed Fiber
        other than provider
        whose CommittedView contains provider

RuntimeCore maintains:

~~~rust
consumers_by_provider: Map<FiberId, Set<FiberId>>
~~~

Edges are added at L-Begin and removed at L-Unload.

## 12. Cleanup scheduler

Each Fiber has one execution record spanning turns:

~~~rust
enum RunningInverse {
    User(RunningMeta),
    Withdraw {
        meta: RunningMeta,
        consumers: Vec<(FiberId, EpisodeId)>,
        endpoint: Arc<dyn ErasedEndpoint>,
    },
}

struct FiberRecord {
    cleanup_running: Option<RunningInverse>,
    cleanup_errors: Vec<CleanupFailure>,
    episode_cleanup_errors: Vec<CleanupFailure>,
    // lifecycle, effects and accumulator
}
~~~

`EffectState::Disposing` is the cleanup request for that effect. `LifecycleState::Unloading` is the request for the entire Episode. No duplicate requested set or episode flag is stored.

RetireFiber finishes within one turn and does not occupy `cleanup_running`.

When `cleanup_running` is None:

1. If the Fiber is Unloading and L-Unload's withdrawal guard and forward boundaries are not both satisfied, select no entry.
2. Verify that the affected effect drivers have reached their boundaries.
3. For an Unloading Fiber, the request covers every live EffectRecord and the whole accumulator. Otherwise it covers Disposing effects and entries owned by those effects.
4. For requested EffectRecords at a forward boundary with no entry in the accumulator/cleanup_running, publish the shared completion and remove the record in the same turn. An empty effect creates no inverse.
5. If a record was removed, recompute the candidate set. If there is no candidate, finish the applicable manual request or complete L-Unload if its terminal conditions hold.
6. If a candidate exists, select the maximum seq and take its InverseAction.
7. User: write `cleanup_running`, then run the Future through TaskSpawner after commit.
8. RetireFiber: apply O-Retire in this turn and emit InverseStarted/InverseFinished consecutively.
9. WithdrawBinding: delete the still-registered old Binding only if `bindings[slot].id == binding`. If the Slot is empty or occupied by another BindingId, do not change topology. On deletion, emit BindingRemoved, capture `(FiberId, EpisodeId)` pairs whose CommittedView holds that BindingId, then emit InverseStarted and retain the endpoint in RunningInverse. Release the endpoint and emit InverseFinished once each captured Episode is no longer installed or the same Episode reaches Active with a key equal to the latest ActivationKey.

When InverseFinished arrives:

1. Validate and clear the matching `cleanup_running`.
2. Record errors. Append any failure in this Actor order to the Episode, the corresponding Effect collector, and every retired Fiber collector in the self/parent chain. During ShuttingDown also append it to the shutdown collector.
3. Continue the Fiber on the next fixed-point pass.

WithdrawBinding removes the matching Binding by BindingId and captures actual Consumer Episodes before emitting InverseStarted. Later fixed-point passes still advance all affected watchers, but watching the same Slot alone creates no cleanup wait. Once a captured Episode has left, the wait does not follow that Fiber into later Episodes or wait for child O-Remove on a retired Fiber. If the same Provider quickly republishes the same Slot, the original Episode's ActivationKey can recover and it can continue using the old endpoint. A replacement Binding in the Slot is unaffected. Neither RetireFiber nor WithdrawBinding spawns a user Future.

## 13. Binding lifecycle

A provide command creates, in one turn:

- the ServiceId/Contract TypeId mapping if absent;
- an EffectRecord;
- a BindingRecord;
- the owner_effect relationship;
- watcher refresh work.

These writes form one reducer commit. The Actor validates Context, descriptor, Slot, and contract first. Failure of any validation inserts neither the contract mapping nor an EffectRecord or BindingRecord.

Binding topology removal and watcher reactions follow:

    registered
      -> manual provide dispose or Provider enters Unloading
      -> remove BindingRecord
      -> watcher reconcile
      -> captured consumer episodes reach their withdrawal boundary

Whole-Fiber Provider unload removes all BindingRecords on entry to Unloading and runs the accumulator only when relied(provider) is false.

Commands changing Binding visibility process direct watchers and the synchronous Leave closure within the current turn. Fibers requiring a step, inverse, or withdrawal guard remain in the corresponding transitional state.

## 14. Parent/child

O-Insert updates together:

- fibers[child];
- fibers[parent].children;
- the parent Episode's mount EffectRecord/InverseEntry;
- child.mount_effect.

A Dispose command immediately applies O-Retire: it marks retired, clears child.mount_effect, and can take only the parent Episode's mount InverseEntry whose action is exactly `RetireFiber(child)`. It satisfies that entry, completes and removes the EffectRecord, then waits for child FiberCompletion. If the parent scheduler already selected the mount entry, it applies the same reducer; a later Dispose command only adds a completion waiter. Actor serialization prevents duplicate inverse execution.

Within one turn, O-Remove removes the child from the registry, watcher/consumer indexes, and parent.children, then publishes Disposed/final report to FiberCompletion. The next fixed-point pass rechecks the parent. Descendant failures were already appended to retired ancestor collectors when they occurred; O-Remove does not copy reports in bulk. A retired parent whose children are empty can reach O-Remove once its other terminal conditions hold.

## 15. Snapshot

The Snapshot command reads consistent state within an Actor turn:

~~~rust
pub struct RuntimeSnapshot {
    pub sequence: u64,
    pub runtime_state: RuntimeState,
    pub quiescent: bool,
    pub fibers: Vec<FiberSnapshot>,
    pub bindings: Vec<BindingSnapshot>,
    pub effects: Vec<EffectSnapshot>,
}
~~~

A snapshot may show non-quiescent state, but all fields must come from the same Actor turn.

## 16. Shutdown

The first Shutdown:

1. Sets runtime_state to ShuttingDown.
2. Changes Root from Active to Unloading and removes Root BindingRecords.
3. Marks all Root live EffectRecords Disposing; Applying drivers only finish their current step.
4. Cleans up Root Episode effects by accumulator sequence. Mount inverses retire top-level Fibers when selected, without waiting for their lifecycles.
5. Retains the shutdown waiter.
6. Progresses through ordinary reconciliation.

Once every non-Root Fiber is removed and the Root Episode is finished:

1. Set runtime_state to Stopped.
2. Retain the ShutdownReport.
3. Complete all waiters.

## 17. Convenience API commit paths

Direct Runtime operations delegate to RootContext. ComponentContext delegates to its original Context/Scope. A closure TaskSpawner follows the same nonblocking submission contract as a trait implementation. Convenience entry points introduce no alternate state mutation path.

`on_cleanup` submits a User inverse through RegisterCleanup, reuses the Effect path to register its synchronous driver, then awaits wait_forward. Registration does not invoke the cleanup callback. Consecutive awaited registrations establish accumulator order. During a race with shutdown, an already submitted registration step still has inertia and may be cleaned up before the registration call resumes. The returned EffectHandle preserves its terminal state, and repeated dispose does not invoke the inverse again. An ordinary registration submitted after shutdown is rejected with RuntimeShuttingDown.

There is a finishing exception: if the same Episode remains installed, its unload accumulator has not started, and an activation or effect step is still running, on_cleanup may submit the inverse for resources already acquired by that step, including after shutdown/retirement. If the Fiber is already Unloading, the new Effect is immediately marked Disposing and joins cleanup once all forward steps land. This exception does not allow new ordinary effects, provides, or mounts and does not accept a Scope from an old Episode.

Insertion and removal in `effect_owners` and Fiber.effects must execute in the same state commit in both Debug and Release. Debug assertions check results; they must not perform the state updates themselves.
