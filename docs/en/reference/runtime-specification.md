[简体中文](../../zh/reference/runtime-specification.md) | [English](runtime-specification.md)

# Runtime specification

- Applies to: Core 0.1
- Specification level: Normative
- Scope: A single-process dynamic composition runtime for trusted components

## Abstract

Linorun Core is a Rust component runtime that manages dependencies, asynchronous lifecycles,
and resource cleanup:

1. Component side effects must carry inverses tracked by the runtime.
2. Components activate only when their declared dependencies are satisfied.
3. Dependency changes are determined by Provider Fiber identity, not by comparing service values.
4. An activation uses a fixed CommittedView.
5. After a Provider leaves resolution, Consumers are undone before the Provider is cleaned up.
6. Asynchronous steps have inertia: steps that have started must complete, and new targets are handled at boundaries.
7. Each Fiber executes at most one lifecycle transition at a time.

This specification fixes Core 0.1 data structures, state transitions, and seven invariants.

## 1. Design constraints

This specification defines the Core 0.1 runtime contract. API documentation explains how to use these
rules, architecture documentation describes the implementation, and conformance tests check
observable behavior.

- Components must declare service dependencies explicitly; parent/child relationships do not grant access to undeclared services.
- Target records Provider Fiber identities in Requirement order, without comparing service values.
- Each Episode retains fixed dependency, configuration, and implementation snapshots through cleanup.
- An asynchronous step that has started must complete; new ActivationKeys are handled only at step boundaries.
- If the target changes temporarily during Loading and returns to the same ActivationKey, the current Episode continues.
- Cleanup within a Fiber runs serially in reverse registration order; provider-wide cleanup waits for committed consumers to leave.
- Derived Contexts remain immutable; service isolation and call metadata follow separate rules.

## 2. Normative terminology

- MUST: a mandatory requirement.
- MUST NOT: prohibited behavior.
- Forward: an operation producing a reversible change.
- Inverse: an operation restoring that change.
- Installed Fiber: a Fiber in Reloading, Active, or Unloading.
- Visible Binding: a Binding currently resolvable by a new Consumer.
- Committed view: dependency resolution fixed at the beginning of an episode.
- Settled Fiber: a Fiber that has completed immediately runnable lifecycle reconciliation and has public state Pending, Active, Failed, or Disposed. Independent Scope effects need not have completed.
- Quiescent: every Fiber is settled; the registry contains no retired-but-unremoved Fiber; and there is no lifecycle step, effect forward step, inverse, reconcile work, or pending O-Remove.

## 3. Goals

Core 0.1 MUST support:

- Separation of Component definitions from Fiber instances.
- Required dependencies.
- Single-Provider Bindings.
- Reversible effects, manual disposal, and automatic Fiber-wide cleanup.
- Synchronous, asynchronous, and iterative effects.
- Ordered Provider/Consumer unloading.
- Immutable Context derivation.
- Service isolation Realms.
- Interception metadata.
- Child Fiber mounting and retirement.
- Local recovery after activation failure.
- Runtime shutdown.
- State snapshots suitable for conformance testing.

## 4. Semantic kernel

Core 0.1 consists of five connected mechanisms:

- The Episode accumulator provides temporal composability.
- Reactive Target drives Fiber lifecycle.
- CommittedView fixes dependencies for one episode.
- Inertia coordinates asynchronous steps.
- Withdrawal guards order Consumer and Provider cleanup.

## 5. Core objects

### 5.1 Component

Component is an immutable definition containing:

~~~rust
struct ComponentDescriptor {
    id: ComponentId,
    config_type: TypeId,
    requirements: Vec<Requirement>,
    provides: Set<ServiceId>,
}

struct Requirement {
    service: ServiceId,
    contract: ContractVTable,
    declared_intercept: Option<InterceptValue>,
}

struct InterceptValue {
    contract_type: TypeId,
    intercept_type: TypeId,
    value: Arc<dyn Any + Send + Sync>,
}

struct ContractVTable {
    contract_type: TypeId,
    intercept_type: TypeId,
    identity_intercept: fn() -> InterceptValue,
    merge_intercept: fn(
        &ServiceId,
        &InterceptValue,
        &InterceptValue,
    ) -> Result<InterceptValue, ServiceTypeMismatch>,
}
~~~

Requirements have unique ServiceIds and stable order, determining the order of Target and
CommittedView. `None` uses the corresponding ServiceContract's `identity_intercept()`.
All nonempty values must be constructed through that contract's generic Core erasure;
descriptor macros call only the doc-hidden `Requirement::new::<C>`.
`Requirement.contract` is a Core-generated erased vtable carrying ServiceContract and Intercept
TypeIds plus identity/merge functions. InterceptValue records both its source contract and
Intercept type. Core does not accept bare `Arc<dyn Any>` as a valid intercept.

The macro establishes the descriptor as a unique process-wide `'static` instance; `config_type`
must be `TypeId::of::<C::Config>()`. All Requirements in Core 0.1 are Required. A Component may
provide only ServiceIds declared by its descriptor.

ComponentId identifies the definition, not a running instance. FiberId identifies an instance.

### 5.2 Fiber

A Fiber is one instantiation of a Component:

~~~rust
struct FiberRecord {
    parent: Option<FiberId>,
    mount_effect: Option<EffectId>,
    children: Set<FiberId>,
    descriptor: &'static ComponentDescriptor,
    implementation: Arc<dyn DynComponent>,
    config: ErasedConfig,
    route: Arc<ContextRoute>,
    lifecycle: Lifecycle<EpisodePayload>,
    effects: Map<EffectId, EffectRecord>,
    next_inverse_seq: u64,
    cleanup_running: Option<RunningInverse>,
    unload_started: bool,
    cleanup_errors: Vec<CleanupFailure>,
    episode_cleanup_errors: Vec<CleanupFailure>,
    completion: Arc<FiberCompletion>,
}
~~~

FiberId is a Runtime-local identity, allocated monotonically without reuse during a Runtime's lifetime.

`Lifecycle.revision` represents config updates, implementation replacement, and explicit restart.
It is not part of Target but participates in ActivationKey. FiberId is the registry's map key
and is not duplicated inside the record.

### 5.3 Runtime

Runtime stores the Fiber registry, Binding store, and dependency watchers:

~~~rust
struct RuntimeCore {
    fibers: Map<FiberId, FiberRecord>,
    bindings: Map<BindingSlot, BindingRecord>,
    contracts: Map<ServiceId, ContractVTable>,
    watchers: Map<BindingSlot, Set<FiberId>>,
    consumers_by_provider: Map<FiberId, Set<FiberId>>,
    default_realm: RealmId,
    identity_ids: IdentityCounters,
    sequence_ids: SequenceCounters,
    root: FiberId,
}
~~~

Runtime uses one logical Actor as its sole control-plane owner. All lifecycle state mutations
are serialized through this Actor.

`contracts` fixes a unique `ServiceId -> ServiceContract TypeId/vtable` mapping within a Runtime.
Mount stages validation of descriptor Requirements and their associated route intercepts; provide
stages validation of the generic ServiceContract identity. Both then write missing mappings and
Fiber/Effect/Binding changes in the same Actor commit. If the same ServiceId has a different
TypeId, the command returns ServiceTypeMismatch without modifying RuntimeCore. Provide does not
read route intercepts unrelated to that Binding. Mappings grow monotonically throughout the
Runtime's lifetime and are not removed with Fibers/Bindings. L-Begin therefore processes only
validated endpoints, Requirement metadata, and associated route intercepts.

Runtime MUST NOT call or await component code, endpoints, or inverses while holding a mutable
control-plane borrow or lock. Endpoints, Config, implementations, driver continuations, and user
inverses removed from the control plane first enter a post-commit deferred-drop list, and are
dropped after the RuntimeCore borrow is released. Drop must be nonblocking and must not panic.
Resource shutdown that requires waiting belongs in an inverse.

Runtime construction installs a synthetic Root Fiber:

- Root uses a reserved FiberId.
- Root uses reserved ComponentId `linorun/root@1`, unit Config, and a synthetic descriptor with empty requirements/provides. The Host exemption for RootContext::provide does not consult that provides set.
- Its initial state is Active.
- While RuntimeState == Running, Root's latest ActivationKey is fixed to `Target { providers: [] }, revision 0`; in ShuttingDown/Stopped it is fixed to None.
- Its initial Episode uses that empty key, empty Requirements, and an empty CommittedView.
- RootContext provide, mount, and effect belong to the Root Episode accumulator.
- Shutdown moves Root into Unloading and ends it through the same withdrawal and cleanup rules. After completion, Root remains Inactive/Pending, without L-Begin or O-Remove.

Reserved values are FiberId(0), EpisodeId(0), and default Realm local 0. Their identity counters
start at 1. Snapshot/event sequences also start at 1.

### 5.4 Identifiers

Core semantics require only these identifiers:

~~~rust
struct FiberId(u64);
struct EpisodeId(u64);
struct EffectId(u64);
struct BindingId(u64);
struct StepId(u64);
struct ComponentId(Arc<str>);
struct ServiceId(Arc<str>);
struct RealmId {
    runtime: Arc<RuntimeTag>,
    local: u64,
}

struct IdentityCounters {
    fiber: u64,
    episode: u64,
    effect: u64,
    binding: u64,
    step: u64,
}

struct SequenceCounters {
    snapshot_sequence: u64,
    event_sequence: u64,
}
~~~

EpisodeId, EffectId, and BindingId are identities used for diagnostics and asynchronous completion
correlation within a Runtime. They do not participate in Target equality. RealmId is an opaque
handle bound to its Runtime.

Fiber/Episode/Effect/Binding/Step identities and sequences are allocated by monotonically
increasing Actor counters and never reused. The default Realm uses local 0.
RootContext::create_realm synchronously allocates from 1 through RuntimeHandle's AtomicU64,
without changing RuntimeCore. RootContext/Context::default_realm purely constructs local 0 from
the same RuntimeTag, allowing derived routes to reset explicitly to the default Realm.
ComponentId and ServiceId are stable string names, outside this u64 identity space.

## 6. LifecycleState

The lifecycle uses four internal states:

~~~rust
struct Lifecycle<P> {
    retired: bool,
    revision: u64,
    state: LifecycleState<P>,
}

enum LifecycleState<P> {
    Inactive {
        failure: Option<ActivationFailure>,
    },
    Reloading {
        episode: Episode<P>,
        step: StepId,
    },
    Active {
        episode: Episode<P>,
    },
    Unloading {
        episode: Episode<P>,
        outcome: UnloadOutcome,
    },
}

enum UnloadOutcome {
    Normal,
    ActivationFailed {
        failure: ActivationFailure,
        revision: u64,
    },
}
~~~

`FiberRecord.cleanup_running` belongs to FiberRecord, not the Unloading variant. Manual effect
disposal, manual provide disposal, and Fiber-wide unload all run inverses through this same
serial scheduler. EffectState::Disposing represents a request for one effect;
LifecycleState::Unloading represents a request for the entire Episode.

The public API projects states in the following priority order; removed and retired conditions
precede ordinary Inactive projection:

| Internal state | Public state |
|---|---|
| Removed from registry | Disposed |
| Inactive, retired, still in registry | Unloading |
| Inactive, without failure, not retired | Pending |
| Reloading | Loading |
| Active | Active |
| Unloading | Unloading |
| Inactive, with failure | Failed |

Even `Inactive { failure: Some(_) }` follows the retired rule when retired, appearing as Unloading
until it leaves the registry. Failed does not retry automatically, but it is settled and may
exist in a quiescent Runtime.

Disposed is not a fifth running state. It is the terminal outcome after a Fiber completes
retirement and leaves the active registry.

### 6.1 Episode

~~~rust
struct Episode<P> {
    id: EpisodeId,
    key: ActivationKey,
    committed: Arc<CommittedView>,
    payload: P,
}

struct EpisodePayload {
    implementation: Arc<dyn DynComponent>,
    config: ErasedConfig,
    inverses: Vec<InverseEntry>,
}
~~~

The same Episode continues from Reloading through Active and into Unloading. Entering Inactive
drops the Episode; the next Reloading creates a new EpisodeId, CommittedView, and accumulator.

Component implementations and Config obey an immutable snapshot contract. External aliases must
not bypass replace/update through interior mutability to change activation semantics.

## 7. Target, ActivationKey, and CommittedView

### 7.1 BindingSlot

~~~rust
struct BindingSlot {
    service: ServiceId,
    realm: RealmId,
}
~~~

A Slot has at most one registered BindingRecord at a time. Once BindingRecord leaves the store,
the Slot is immediately available for registration again.

### 7.2 BindingRecord

~~~rust
struct BindingRecord {
    id: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    owner_effect: EffectId,
}
~~~

A Binding is visible to new Consumers if and only if its record remains in the store and its
Provider Fiber is Active and not retired.

Bindings created by a Reloading Fiber are not externally visible. When the Provider enters
Unloading, Core removes its registered BindingRecords from the store. Manually disposing a
provide effect removes its corresponding BindingRecord. CommittedView owns its own endpoint
Arc, allowing old Consumers to finish their current episodes while the Slot accepts a new Binding.

### 7.3 Target

Target calculation rules:

1. A retired Fiber has Target None.
2. Resolve each BindingSlot in requirements order.
3. Every Slot must contain a visible Binding with matching contract_type.
4. If any Requirement is unsatisfied, Target is None.
5. Otherwise, Target is the sequence of Provider FiberIds.

~~~rust
struct Target {
    providers: Vec<FiberId>,
}
~~~

Target equality compares only providers. It MUST NOT compare:

- BindingId.
- EpisodeId.
- Endpoint addresses.
- Service business values.
- Rust PartialEq results.
- Interception metadata.

### 7.4 ActivationKey

~~~rust
struct ActivationKey {
    target: Target,
    revision: u64,
}
~~~

Revision represents config updates, implementation updates, and explicit restart. Target
represents dependency changes; revision represents restarts unrelated to dependency changes.

### 7.5 CommittedView

~~~rust
struct CommittedView {
    key: ActivationKey,
    entries: Vec<CommittedEntry>,
}

struct CommittedEntry {
    binding: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    declared_intercept: Option<InterceptValue>,
}
~~~

CommittedView is constructed once at L-Begin and remains unchanged throughout the episode.

When a Consumer accesses a declared service in Reloading, Active, or Unloading, it may read only
that CommittedView; it must not query the current Binding store again.

## 8. Effect model

### 8.1 Effect

An effect is a forward and the inverse it produces:

~~~text
forward(context) -> updated context + inverse
inverse(updated context) -> recovered context
~~~

Runtime cannot prove that an inverse is correct. It is responsible only for tracking, ordering,
and invocation.

### 8.2 EffectRecord

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

struct RunningMeta {
    episode: EpisodeId,
    seq: u64,
    owner: InverseOwner,
    label: Option<Arc<str>>,
}

enum InverseOwner {
    Activation,
    Effect(EffectId),
}

enum EffectState {
    Applying,
    Armed,
    Disposing,
}

enum RunningInverse {
    User(RunningMeta),
    Withdraw {
        meta: RunningMeta,
        endpoint: Arc<dyn ErasedEndpoint>,
        consumers: Vec<(FiberId, EpisodeId)>,
    },
}
~~~

The Episode accumulator exclusively owns InverseEntries. Entries produced by activation steps
are marked Activation; entries produced by Scope effects are marked with their EffectId.
EffectRecord tracks only driver state and shared completion. Manual disposal selects accumulator
entries by InverseOwner.

User activation/effects produce User actions. Mount produces RetireFiber; provide produces
WithdrawBinding. Entries in the accumulator have not executed. After the Actor takes an action,
cross-turn User or Withdraw state moves into `cleanup_running` and is not returned to the
accumulator after completion. RetireFiber is a synchronous reducer action. For WithdrawBinding,
the Actor removes topology, notifies relevant watchers, and waits only for Consumer Episodes
committed to that exact BindingId,
then releases the endpoint. Removal occurs only when the Slot's current BindingId matches the
action, preventing deletion of a replacement Binding. Neither Core action executes user code.

Whenever a synchronous or asynchronous iterator yields an inverse, Runtime allocates a
monotonically increasing Fiber-local seq and appends it to the episode accumulator. The Actor's
Yield acceptance commit is Core's observable application linearization point; concurrent forward
completions receive seq values in Actor commit order. Components must return/yield the inverse
immediately after a change becomes observable. An unrelated await between change and Yield
violates the component's application-order contract. Concurrent effects in one Fiber whose
external changes do not commute must be serialized by component program order, such as
`wait_forward`. Core cannot observe earlier real-world write ordering inside Futures.

Scope accepts effects belonging to the current Episode while Reloading or Active. When the
Fiber enters Unloading, Scope stops accepting new effects and moves every live Applying/Armed
EffectRecord to Disposing. Applying drivers may complete only their current step and cannot continue.

`on_cleanup` uses a dedicated `RegisterCleanup` command to register the inverse for an acquired
resource. After shutdown or retirement begins, registration remains allowed when it belongs to
the same Episode, that Episode has not started unload cleanup, and an activation or effect step
is still in flight. This finishes an existing step; it does not permit new external work.
A Scope from an expired Episode remains rejected. Registration uses the same seq, accumulator,
and cleanup scheduler. If the owner is already Unloading, it immediately joins pending cleanup.

### 8.3 Supported effect forms

The semantics support:

- Synchronously returning one inverse.
- A Future returning one inverse.
- An Iterator producing multiple inverses in sequence.
- An AsyncIterator, or the equivalent Rust step stream, producing multiple inverses in sequence.

Activation exposes `Once` and `Steps` inputs. Scope effects accept once, steps, and synchronous
adapters through `IntoEffectDriver`. The Actor normalizes both to Yield/Done step events while
preserving their distinct error types.

Each step task exclusively owns its driver and polls only one step. On completion, the task
transfers its event and the still-existing driver to the Actor:

~~~rust
struct ActivationCompletion {
    result: Result<DriverEvent, ActivationFailure>,
    driver: ActivationDriver,
}

struct EffectStepCompletion {
    result: Result<DriverEvent, EffectError>,
    driver: ErasedEffectDriver,
}
~~~

`Yield { has_next: true }` means the returned driver is a resumable continuation. Drivers returned
with final Yield, Done, or Err exist only for post-commit drop and MUST NOT be polled again.
After accepting completion, the Actor either moves the resumable driver into the next RuntimeTask
or drops it after state commits. It does not reconstruct drivers from a boolean or let tasks
independently cross the Actor decision boundary to poll another step. The Actor stores only
completion identity in `LifecycleState::Reloading.step` or `EffectRecord.running_step`; it does
not share driver ownership with a task.

A step has semantically started when the Actor writes StepId and transfers its driver to
RuntimeTask in one commit. TaskSpawner not having physically polled the task yet does not create
a cancellable intermediate state. Target changes or disposal committed afterward must wait for
this step to complete. A driver is at a boundary only after completion returns to the Actor and
before the Actor transfers it to another RuntimeTask. The Actor makes its continue/stop decision
within that turn, without exposing a “Reloading without an in-flight step” state into which
another command could interleave.

Provide and mount are Core-owned effects committed atomically within the Actor. They directly
allocate inverse seq, create EffectRecord and topology, and do not create a user EffectDriver or
StepId. Successful commit naturally completes forward, leaving EffectRecord Armed. They still
use the same accumulator, LIFO scheduler, and EffectCompletion, not a separate cleanup mechanism.

An ordinary async Future is one coarse-grained step. If Target changes while it runs, the Future
still completes. Runtime collects its resulting inverse, then decides to commit or divert at
the boundary.

Iterative effects create a boundary after every yield. After Target changes:

- A subsequent iteration that has not started does not execute.
- An asynchronous iteration that has already started must complete.
- Its resulting inverse must enter the accumulator first.
- Unloading follows.

If Target returns to the original ActivationKey before the current step completes, the current
episode proceeds according to that step's continuation. This is the inertia rule at a step boundary.

If an independent Scope effect's forward step fails, Runtime stops that driver, undoes its
already-produced inverses in descending seq order, and writes the error into EffectHandle
completion. Fiber lifecycle remains unchanged. Only explicit propagation by activation code
turns the error into an activation failure that triggers L-Raise.

### 8.4 Manual disposal

EffectHandle::dispose is idempotent:

- The first call requests that further iterations stop and existing inverses from that effect be undone.
- An iteration already running still completes.
- Inverses execute in descending seq order.
- Later disposal calls wait for the same result.
- Fiber unload MUST NOT call completed inverses again.

While a Fiber is Reloading or Active, a manual request may run after its effect driver reaches
a boundary. Once the Fiber enters Unloading, all manual cleanup requests that have not started
only queue. The same scheduler may select them only when both L-Unload's forward boundary and
withdrawal guard are satisfied. Manual APIs cannot bypass Provider/Consumer ordering.

When a requested EffectRecord has reached its forward boundary and has no entry in either the
accumulator or cleanup_running, the Actor publishes shared completion and removes it from live
EffectRecords in the same turn. This includes effects that never produced an inverse or whose
forward failed before the first Yield; no inverse is fabricated. The same rule applies after
all existing entries finish and are taken. EffectHandle returns the same final result through
shared completion.

### 8.5 Episode accumulator

Fiber unload uses the episode accumulator. It selects the largest unexecuted seq, waits for
that inverse to complete, and only then selects the next one.

Fiber-wide cleanup starts once every EffectRecord's current step has completed and all have
stopped producing new inverses. The cleanup set and application order are therefore fixed
before the first inverse starts.

If an inverse returns an error or panics:

- Record the error.
- Clear that inverse from `cleanup_running`.
- Continue with earlier inverses.
- Do not retry inverses automatically.

## 9. Context, isolate, and intercept

### 9.1 Context

Context is an immutable, cheaply cloned derived value:

~~~rust
struct Context {
    runtime: RuntimeHandle,
    fiber: FiberId,
    episode: EpisodeId,
    route: Arc<ContextRoute>,
    committed: Arc<CommittedView>,
}
~~~

Fields remain private. Cloning Context creates no Fiber, Binding, or effect.

Context handles service reads and route derivation; Scope handles mutations such as effect,
provide, and mount. Context's read-only lookup uses its stored CommittedView without synchronously
querying the Actor. Components must not use Context or typed service clients created from it after
their Episode finishes. Scope's asynchronous mutation interfaces validate Runtime/Fiber/Episode
ownership and return InactiveContext.

### 9.2 isolate

~~~rust
struct ContextRoute {
    parent: Option<Arc<ContextRoute>>,
    isolate_delta: Map<ServiceId, RealmId>,
    intercept_delta: Map<ServiceId, InterceptValue>,
}
~~~

Context::isolate returns a new route node. It neither modifies the original Context nor immediately
changes an existing Fiber's Target. The current Fiber's service lookup still reads its existing
CommittedView. The new isolate affects only subsequent mount/provide calls using that derived
Context. To read a service from another Realm, a child Fiber must mount under that route and
commit the Realm's Binding through its own Requirement. `default_realm()` returns that Runtime's
local 0 RealmId. Passing it to an inner isolate overrides the outer route's Realm for the same ServiceId.

A Fiber mounted on a derived Context resolves Requirements through that route. Scope::provide
also explicitly accepts Context and uses its route.

### 9.3 intercept

Context::intercept returns a new Context carrying a metadata overlay.

intercept:

- It may affect service call context.
- It does not change BindingSlot.
- It does not change Target.
- It does not reload existing Fibers.

ServiceContract defines metadata merging. Core only retains immutable values and passes them
through the Context hierarchy. `identity_intercept` must be both the left and right identity
of `merge_intercept`, and merge must be associative. The effective value folds identity,
declared metadata, and route overlays from outermost to innermost.

### 9.4 Service confinement

`Context::service` resolves only the current Fiber Episode's CommittedView. An undeclared service
returns `ServiceAccessError::UndeclaredDependency`. Core does not implicitly search parent Fibers
or the mount Context for capabilities. Every service read therefore has a corresponding
Requirement: a component may read only its declared service dependencies.

A Fiber's `parent` still represents only mount/removal ownership, not a service lookup chain.

## 10. Provider withdrawal

### 10.1 Installed and relied

Reloading, Active, and Unloading Fibers are all installed.

A Provider is relied upon if an installed Consumer's CommittedView points to it:

~~~text
relied(provider) =
    exists consumer:
        consumer.id != provider
        and consumer is installed
        and some consumer.committed entry.provider == provider
~~~

RuntimeCore maintains this relation through consumers_by_provider; installed CommittedViews
define its source of truth.

### 10.2 Leave

When a Provider leaves Active, it must do the following before starting inverses:

1. Change state to Unloading.
2. Remove its registered BindingRecords from the store, allowing immediate Slot reuse.
3. Notify corresponding watchers and recompute Consumer Targets.

A Consumer enters its own Unloading when it observes a different Target. Consumer cleanup still
uses the old CommittedView.

### 10.3 Withdrawal guard

A Provider may start its accumulator only once no installed Consumer relies on it and all
forward/effect drivers in its current episode have reached boundaries.

“Starting the accumulator” includes any previously requested manual Effect cleanup that has not
started. After entering Unloading, no second inverse-start path bypasses this guard.

For manual provide disposal, after the cleanup scheduler selects that provide inverse, the Actor
removes the Slot's record only if its current BindingId still matches the inverse's BindingId.
An already-withdrawn old Binding must not delete a replacement in the same Slot. The Actor
refreshes every watcher of that Slot, but captures only installed Consumers whose CommittedView
retains the withdrawn BindingId, recording their `(FiberId, EpisodeId)`. A watcher indicates only
potential sensitivity to topology changes; it must not independently create a withdrawal wait
edge. Otherwise, Fibers that never used the Binding could form a purely Core-owned cleanup wait
cycle with its Provider.

For each captured Consumer, the wait ends when either the captured Episode has ended (including
internally Inactive, a later Episode, or Fiber removal), or that same Episode is Active with
episode.key equal to the latest ActivationKey. Later Episodes do not extend the old Binding's
wait. This uses neither the public settled condition of `FiberHandle::wait_settled` nor child
O-Remove, and does not wait for unrelated independent Scope effects on Active Fibers. A retired
Consumer that finishes its own L-Unload has ended the captured Episode even if it still awaits
children. Core releases its endpoint reference when all captured Consumers satisfy these
conditions. If the same Provider quickly provides the same Slot
again, a Consumer's ActivationKey may recover and continue the old Episode, whose CommittedView
still retains the old endpoint. The Provider's other Bindings and effects remain unchanged.
Fiber-wide unload has already removed Binding topology upon entering Unloading; its withdrawal
guard then waits for every committed Consumer to actually leave.

## 11. State transitions

The following rules are the normative Core 0.1 state machine.

### 11.1 O-Insert

When the Host or a parent effect creates a Fiber:

1. Validate that descriptor is a `'static` instance and config_type matches Component::Config. Stage Requirement contract mappings and validation of route intercepts associated with those Requirements. Any failure leaves RuntimeCore unchanged for the entire command.
2. Allocate FiberId, mount EffectId, and a new inverse seq in the parent Fiber.
3. Write an Armed mount EffectRecord and pending accumulator entry into the parent Episode.
4. Write the child's parent, mount_effect, descriptor, implementation, config, and route.
5. Set retired to false, activation_revision to 0, and next_inverse_seq to 1.
6. Set lifecycle to Inactive without failure and `cleanup_running` to None.
7. Start with empty children/effects and create FiberCompletion.
8. Add the child to parent.children, including Root children.
9. Register Requirement watchers.
10. Reconcile.

Mounting a child is itself an effect of the parent Fiber. An ordinary non-retired Fiber stores
its parent's `mount_effect`; the synthetic Root and retired Fibers have None. During natural
parent cleanup, the mount inverse performs O-Retire.

### 11.2 O-Retire

O-Retire may be triggered by a mount inverse or the Host's `FiberHandle::dispose`. It sets the
child's retired flag to true and clears child.mount_effect. The Host path may satisfy only the
corresponding Core-generated mount InverseEntry that is still Armed and whose action exactly
matches `RetireFiber(child)`. The Actor takes that entry and requests EffectRecord removal.
Because Host orchestration has already established the same O-Retire outcome, it does not invoke
that InverseAction again. It cannot skip any other inverse on this basis. If the parent scheduler
already selected the entry, synchronous RetireFiber completed in that same Actor turn, so the Host
only waits for FiberCompletion. Both entry points share the same reducer. O-Retire neither removes
the Fiber directly nor waits for or skips child cleanup. The FiberHandle waiter and parent's
O-Remove condition separately wait for FiberCompletion and an empty children set.

Retirement immediately makes Target None.

### 11.3 O-Remove

When a Fiber is:

- Retired.
- Inactive.
- Without children.
- Without unfinished effects or Bindings.

Runtime removes it from the registry and publicly reports Disposed. Before removal, the Actor
publishes Disposed and the final DisposeReport through FiberCompletion, so Runtime does not need
to retain a tombstone record for FiberHandle.

### 11.4 L-Begin

Preconditions:

- The Fiber is Inactive without failure.
- `cleanup_running` is None.
- The current ActivationKey is Some.

Actions:

1. Capture currently visible Bindings.
2. Construct CommittedView, retaining validated declared interception metadata.
3. Create an Episode.
4. Start with an empty accumulator.
5. Enter Reloading.
6. Construct an Init driver without calling user code, then start the first activation step after commit.

The Init driver owns implementation, ComponentContext, and config. Only the first RuntimeTask
poll calls `Component::activate` and installs its Once/Steps driver. Before calling, the wrapper
is set to Exhausted, so a synchronous panic in that method still returns a valid wrapper to the
Actor with ActivationFailure completion.

### 11.5 L-Iter

Preconditions:

- The Fiber is Reloading.
- The current ActivationKey equals episode.key.
- Completion is `Yield { inverse, has_next: true }` and returns the activation driver.

Actions:

1. Add the inverse to the accumulator.
2. Move the returned driver as a continuation into a RuntimeTask with a new StepId.
3. Remain Reloading.
4. Schedule the next step.

### 11.6 L-Finish

Preconditions:

- The Fiber is Reloading.
- The current ActivationKey equals episode.key.
- Completion is final `Yield { inverse, has_next: false }` or `Done`.

Actions:

1. Add the inverse to the accumulator if completion carries one.
2. Enter Active.
3. Registered Provider Bindings become visible because the Provider is Active.
4. Notify watchers.

### 11.7 L-Divert

Preconditions:

- The Fiber is Reloading.
- A successful completion has returned to the Actor.
- The current ActivationKey differs from episode.key.

Runtime first collects any inverse carried by completion, then commits Unloading and releases
the returned driver outside the Actor. It schedules no next RuntimeTask even if `has_next: true`.
The step started when StepId and task ownership committed, so a Target change before the first
executor poll cannot undo that step. Step errors always follow L-Raise.

Runtime MUST NOT enter Active temporarily.

Entering Unloading removes this Fiber's registered BindingRecords and refreshes their watchers,
while marking every live EffectRecord in the Episode Disposing. Applying drivers still complete
their current steps according to inertia.

### 11.8 L-Raise

If an activation step returns an error or a catchable panic:

1. Release the activation driver returned by completion after state commits.
2. Retain the accumulator already collected.
3. Enter Unloading.
4. Remove this Fiber's registered BindingRecords and refresh their watchers.
5. Mark every live EffectRecord in this Episode Disposing. Applying drivers finish only their current step.
6. Record activation failure and `episode.key.revision` in the outcome.

Effects installed before the failure must be undone first.

### 11.9 L-Leave

Preconditions:

- The Fiber is Active.
- The current ActivationKey differs from episode.key.

Actions:

1. Enter Unloading.
2. Remove this Fiber's registered BindingRecords.
3. Notify watchers.
4. Move all live EffectRecords to Disposing. Applying drivers finish only their current step.
5. Wait for in-flight step completion and the withdrawal guard.

### 11.10 L-Unload

Start preconditions:

- The Fiber is Unloading.
- No installed Consumer remains committed to this Provider.
- Activation/effect forward drivers have all reached boundaries.

Start actions:

1. Unloading state itself requests cleanup of the entire Episode.
2. The cleanup scheduler takes accumulator entries in descending seq order and executes them serially.
3. The Fiber remains Unloading during cleanup.

L-Unload completes when the accumulator is empty, `cleanup_running` is None, and this Episode's
live EffectRecords have been cleared:

1. Clear episode-local failures already exposed through InverseFinished events and applicable operation collectors.
2. Remove this Consumer's committed dependencies from consumers_by_provider.
3. Clear CommittedView and the episode.
4. Enter Inactive.
5. Retain failure only if the outcome's failure revision still equals the Fiber's current activation_revision.
6. Reconcile the latest Target.

If there is no failure and the latest Target exists, perform L-Begin again as the current
reconciliation converges.

### 11.11 Restart and update

A config update first replaces the latest config snapshot. An implementation update first replaces
the latest implementation snapshot. Restart changes neither snapshot. All three then follow the
same process:

1. Increment activation_revision and clear any existing activation failure.
2. If the Fiber is Unloading, invalidate activation failure in the outcome that has not yet committed.
3. Reconcile.

An Active Fiber performs L-Leave because ActivationKey changed. An Inactive Fiber performs L-Begin
when an ActivationKey exists.

Descriptor and route remain unchanged throughout a Fiber's lifetime. `replace` accepts only an
implementation returning the same static descriptor instance with a matching Config TypeId.
When requirements, provides, Config type, or route changes, retire the old Fiber and create a new one.

## 12. Inertia and asynchronous coordination

Each Fiber may have only one lifecycle transition at a time.

The logical Actor records only the current EpisodeId and step completion token in state.
Asynchronous user code executes outside the control plane and sends its result on completion.

When completion arrives:

- It may advance only if EpisodeId matches the current Reloading/Unloading episode.
- Completion from an old episode that no longer exists performs only necessary local resource release; it MUST NOT modify a new episode.
- Target changes update only the latest intent and do not start a second transition in parallel.

Core does not pretend to roll back by dropping Futures. A step that never finishes keeps the
Fiber Loading or Unloading.

## 13. Seven core invariants

### I1: At-most-once inverse invocation

Every successfully collected inverse must be called at most once across all racing traces of
manual disposal, Fiber unload, parent retirement, and Runtime shutdown.

Only after the Host itself has committed O-Retire for the same child may it treat an exactly
matching Armed `RetireFiber(child)` entry as satisfied without invoking it. This is not a channel
for skipping arbitrary inverses.

### I2: Strict reverse order and serial cleanup

At most one inverse runs in a Fiber at a time. The next inverse must have the largest seq in
the currently requested, unfinished cleanup set. Errors do not cause earlier inverses to be skipped.

### I3: Leave before Provider cleanup

Before the first Fiber-wide unload inverse starts, the Provider must no longer be Active, its
BindingRecords must have left the store, and watchers must have been refreshed. When manual
provide disposal selects WithdrawBinding, the Actor performs the same Binding topology removal
before publishing InverseStarted.

### I4: Consumer before Provider

Before a Provider's Fiber-wide accumulator cleanup starts its first inverse, there must be no
installed Consumer whose CommittedView still points to that Provider. Manual Effect cleanup
that has not started when Unloading begins joins this wait. A manual inverse already started
during Active/Reloading completes according to inertia; L-Unload does not start another inverse
in parallel.

### I5: Fixed CommittedView

An Episode's CommittedView MUST NOT be replaced between L-Begin and L-Unload. Consumer cleanup
continues reading the endpoint committed during activation.

### I6: Target compares only Provider identity

Target consists of Provider FiberIds in Requirement order. BindingId, EpisodeId, service values,
and endpoint addresses MUST NOT participate in Target equality.

### I7: Transitions have inertia

Each Fiber has at most one transition at a time. An asynchronous step that has started must
complete. Target changes decide continuation, diversion, or chaining of the next transition
only at boundaries.

“Started” means the Actor commit that writes StepId and transfers its driver to RuntimeTask,
not the first executor poll. Another command cannot split that commit from the preceding
boundary decision.

## 14. Derived constraints

Core 0.1 must additionally satisfy:

- A Disposed Fiber does not reenter the registry.
- A BindingSlot has at most one registered Binding at a time.
- Provide and mount must belong to effects.
- A Provider is visible to new Consumers only while Active.
- A Failed Fiber does not retry automatically until restart, update, or implementation replacement.
- Cleanup failures are recorded without preventing remaining inverses.
- Isolate changes only a derived Context's Realm resolution.
- Intercept does not change Target.
- The Actor or control plane must not await user code.
- User inverses must not start new effects, provisions, or mounts.
- A parent may O-Remove only after every child is removed.
- Before Fiber-wide cleanup starts, every effect driver in the episode has stopped producing new inverses.

For mutually requiring Components without a visible Provider outside the cycle, Fibers in the
cycle cannot satisfy all Requirements. They remain Pending and produce no committed dependency
edges. Core does not break or implicitly rewrite dependency cycles at runtime. For bidirectional interaction, an independent integration component can depend on both sides and coordinate their calls. Committed dependency edges do not form cycles, so the withdrawal guard
cannot structurally deadlock on a committed dependency cycle. It may still wait for user Futures
that never finish.


## 15. Failure

### 15.1 Activation failure

Activation failure belongs to one Fiber:

1. Enter Unloading.
2. Undo completed steps.
3. Reach Inactive.
4. Retain failure.
5. Expose Failed publicly.

Siblings are unaffected.

A Failed Fiber does not retry automatically because of dependency churn. Failure is cleared when
the Host calls restart, updates config, or replaces implementation.

### 15.2 Error data structures

User code returns leaf errors. Runtime adds location context to cleanup errors:

~~~rust
enum FailureKind {
    User,
    Panic,
}

struct FailureCause {
    kind: FailureKind,
    message: Arc<str>,
}

struct ActivationFailure {
    cause: FailureCause,
}

struct EffectError {
    cause: FailureCause,
}

struct CleanupError {
    cause: FailureCause,
}

struct CleanupFailure {
    owner: CleanupOwner,
    fiber: FiberId,
    episode: EpisodeId,
    seq: u64,
    label: Option<Arc<str>>,
    cause: CleanupError,
}

enum CleanupOwner {
    Activation,
    Effect(EffectId),
}
~~~

`ActivationFailure`, `EffectError`, and `CleanupError` are leaf errors. `CleanupFailure` is an
aggregation record with CleanupOwner, Fiber, Episode, seq, and optional label. `RealmMismatch`
carries a RealmId from another Runtime. ServiceTypeMismatch carries ServiceId, the
Contract/Intercept channel, and expected/actual TypeIds, and is wrapped by MountError,
ProvideError, or ServiceAccessError.

### 15.3 Cleanup failure

When an inverse returns `CleanupError` or panics:

- Wrap it in `CleanupFailure`, recording CleanupOwner, seq, optional label, and error.
- Continue remaining inverses.
- Return aggregated results in the dispose/shutdown report.
- Do not automatically retry the failed inverse.

Cleanup failure does not change the four-state lifecycle. Actual restoration of external state
is the component contract's responsibility.

EffectDisposeReport covers only its EffectId. DisposeReport covers cleanup failures produced by
the target Fiber and descendants in its subtree during the window from the target's O-Retire
through O-Remove. ShutdownReport covers Root and all non-Root Fibers from Running -> ShuttingDown
through Stopped. Historical failures already published during normal reload are not aggregated
again in later reports.

### 15.4 panic

In unwind builds, component task boundaries capture panics. Activation panics become
ActivationFailure, independent effect forward panics become EffectError, and inverse panics
become CleanupError, then CleanupFailure.

## 16. Runtime shutdown

Shutdown uses ordinary lifecycle rules:

1. Runtime rejects new Root/Scope effects, mount, provide, and Fiber restart/update/replace.
2. The synthetic Root enters Unloading and withdraws Root Bindings.
3. Withdrawal guards and the Root Episode accumulator's global LIFO order clean up Root effects.
4. Each mount inverse performs O-Retire at its seq. Child cleanup may interleave with inverses at earlier Root seq values.
5. Wait for every non-Root Fiber to become Disposed and complete the Root Episode.
6. Return aggregated cleanup failures.

Rejecting new work does not discard cleanup registration needed by a step that is finishing.
The `RegisterCleanup` finishing conditions in Section 8.2 still apply during ShuttingDown or after
owner retirement. A Scope from an old Episode, or one whose unload cleanup has started, does not
thereby regain permission to create resources.

This is graceful shutdown. Core does not discard running user Futures, inverse Futures, or
continuations. If user code never returns, shutdown may remain pending indefinitely.

`RuntimeShuttingDown` means RuntimeState is not Running, including Stopped. State/snapshot,
wait/dispose, repeated shutdown, and operations that do not modify RuntimeCore, such as
default_realm/create_realm/isolate/intercept, remain available.

## 17. Rust API boundary

See the [Core API overview](../api/core-api.md) for complete public signatures and error enums.
The following code fixes the main types and their responsibilities without repeating every
snapshot DTO field:

~~~rust
pub struct Runtime;
pub struct RuntimeBuilder;
pub struct RootContext;
pub struct Context;
pub struct Scope;
pub struct ComponentContext;
pub struct FiberHandle;
pub struct EffectHandle;
pub struct ComponentDescriptor;
#[doc(hidden)]
pub struct Requirement;
pub struct ServiceRef<C: ServiceContract>;
pub struct CallContext<I>;
pub trait ErasedEndpoint: Send + Sync + 'static { /* macro adapter */ }
pub struct RuntimeSnapshot;
pub struct RuntimeEvent;
pub enum RuntimeEventKind { /* see observability */ }
pub struct BindingEventRef;
pub type RuntimeTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
pub trait TaskSpawner: Send + Sync + 'static { /* non-blocking spawn */ }

pub enum FiberState {
    Pending,
    Loading,
    Active,
    Unloading,
    Failed,
    Disposed,
}

pub enum EffectForwardOutcome {
    Completed,
    Disposed,
}

pub enum Activation {
    Once(ActivationFuture),
    Steps(ActivationStream),
}

pub type ActivationFuture = Pin<Box<
    dyn Future<Output = Result<Option<InverseFn>, ActivationFailure>> + Send
>>;

pub type ActivationStream = Pin<Box<
    dyn Stream<Item = Result<InverseFn, ActivationFailure>> + Send
>>;

pub trait EffectDriver: Send + 'static { /* poll_step */ }
pub enum DriverEvent { /* Yield or Done */ }
pub trait IntoEffectDriver { /* adapter */ }

pub struct EffectDisposeReport;
pub struct DisposeReport;
pub struct ShutdownReport;
pub struct ActivationFailure;
pub struct EffectError;
pub struct CleanupError;
pub struct CleanupFailure;
pub enum CleanupOwner { /* Activation or Effect */ }
pub struct RealmMismatch { /* foreign RealmId */ }
pub enum ServiceTypeChannel { /* Contract or Intercept */ }
pub struct ServiceTypeMismatch { /* service/channel/expected/actual */ }
pub enum ServiceAccessError { /* see core-api */ }

pub trait Component: Send + Sync + 'static {
    type Config: Send + Sync + 'static;

    fn descriptor(&self) -> &'static ComponentDescriptor;
    fn activate(
        &self,
        ctx: ComponentContext,
        config: Arc<Self::Config>,
    ) -> Activation;
}
~~~

- Context: service lookup, isolate, intercept; `service::<C>()` returns `C::Client`.
- Scope: effect, provide, mount, and cleanup registration.
- ComponentContext: a unified component-facing entry point delegating service reads and resource operations to its Context/Scope.
- FiberHandle: state, wait_settled, restart, update_config, replace, dispose.
- EffectHandle: idempotent disposal.
- ServiceRef: a base wrapper for committed endpoints, provided only to contract adapters.
- Runtime: root operations, snapshot, shutdown.

`Activation::run`, `Activation::once`, and `Activation::steps` are constructors over the existing
Once/Steps execution forms. ComponentContext's convenience methods retain the same declared
dependency and Episode ownership rules. They introduce no additional lifecycle. See the
[Core API overview](../api/core-api.md) for these authoring entry points.

The entire async body of `Activation::run` is one Once step. Awaiting `on_cleanup` inside that
body registers an inverse without splitting the activation boundary. Target changes are decided
when the whole body returns; use `Activation::steps` to check Target between steps.

Each ServiceContract declares a generated `Client` type defined in the contract's own crate.
Context first constructs a ServiceRef with private fields, then the contract adapter wraps it
in that Client. Call methods are defined on Client and always use the committed endpoint and
interception metadata merged by the Context that constructed it. This avoids requiring
downstream crates to add inherent methods to a generic type owned by `linorun-core`.

RuntimeSnapshot and its Fiber/Binding/Effect/Inverse DTOs are public read-only projections.
Internal LifecycleState, EffectState, RunningInverse, and accumulator entries do not become
public through snapshots. `Runtime::new(spawner)` and `Runtime::builder(spawner).build()` both
construct Runtime; `new` delegates to the same builder/build path. `Runtime::builder` requires
TaskSpawner and optionally accepts a standard-library `Sender<RuntimeEvent>`.
`FiberHandle::wait_settled` waits for one Fiber's settled boundary, while
`Runtime::wait_quiescent` waits for the whole Runtime to become quiescent. Detailed internal
enums, records, and Actor commands are crate-private implementation structures. Minimal
definitions of `ErasedConfig`, `DynComponent`, `ErasedEndpoint`, RuntimeHandle, and completion
cells appear in the [core data model](../architecture/data-model.md#12-erased-implementation-types).

## 18. Acceptance criteria

A Core 0.1 implementation must:

1. Pass automated tests for the seven invariants.
2. Pass scenario tests for Effects, inertia, service withdrawal, isolation, and failure handling.
3. Pass reference-model tests for L-Begin, L-Iter, L-Finish, L-Divert, L-Raise, L-Leave, and L-Unload.
4. Demonstrate that Target does not read service values.
5. Demonstrate that cleanup uses the original CommittedView.
6. Demonstrate that Provider inverses do not precede Consumer unload.
7. Demonstrate that asynchronous Target churn does not create parallel transitions.

## 19. Related documentation

- [Core API](../api/core-api.md)
- [Architecture overview](../architecture/overview.md)
- [Seven invariants](invariants.md)
- [Conformance testing strategy](../testing/conformance-strategy.md)
- [Scenario catalog](../testing/scenario-catalog.md)
