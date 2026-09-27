# Core data model

[简体中文](../../zh/architecture/data-model.md) | English

This page expands the data relationships in the runtime specification. Fields describe semantics, not a fixed Rust memory layout.

## 1. RuntimeCore

~~~rust
struct RuntimeCore {
    fibers: Map<FiberId, FiberRecord>,
    bindings: Map<BindingSlot, BindingRecord>,
    contracts: Map<ServiceId, ContractVTable>,
    watchers: Map<BindingSlot, Set<FiberId>>,
    consumers_by_provider: Map<FiberId, Set<FiberId>>,
    effect_owners: Map<EffectId, FiberId>,
    default_realm: RealmId,
    identity_ids: IdentityCounters,
    sequence_ids: SequenceCounters,
    root: FiberId,
}
~~~

RuntimeCore contains the primary data needed for lifecycle coordination.

- fibers: Component instances;
- bindings: currently registered services;
- contracts: the unique TypeId and erased vtable for each ServiceId in this Runtime; entries are added but never removed during its lifetime;
- watchers: the Fibers whose Requirements are affected by a Slot;
- consumers_by_provider: Provider/Consumer edges formed by installed CommittedViews;
- effect_owners: the reverse index from EffectId to its Fiber, updated in the same Actor commit as Fiber.effects;
- default_realm: this Runtime's resolution Realm where isolate has not overridden it;
- identity_ids/sequence_ids: allocate Actor-owned Runtime-local identities and snapshot/event sequences separately. RealmId uses a separate atomic allocator in RuntimeHandle.

root points to the synthetic Root Fiber. Root starts Active with `ActivationKey { target: Target { providers: [] }, revision: 0 }`, an empty CommittedView, and the Root Episode accumulator. All host top-level effects belong to that Episode.

The Root descriptor uses reserved ComponentId `linorun/root@1`, unit Config, and empty requirements/provides. Host provide is an explicit exemption. Root uses FiberId(0), EpisodeId(0), and default Realm local 0. Identity and sequence counters start at 1.

Root has no `mount_effect`. Each ordinary, non-retired Fiber records the mount EffectId that created it in the parent Episode; O-Retire clears it to None.

Root's latest key is the fixed empty key while RuntimeState is Running and None while ShuttingDown/Stopped. After cleanup, Root remains Inactive. It neither activates again nor leaves the registry.

## 2. Identities

~~~rust
struct FiberId(u64);
struct EpisodeId(u64);
struct EffectId(u64);
struct BindingId(u64);
struct StepId(u64);
struct ComponentId(Arc<str>);
struct RealmId {
    runtime: Arc<RuntimeTag>,
    local: u64,
}
struct ServiceId(Arc<str>);

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

Roles:

| Identity | Role |
|---|---|
| FiberId | Provider identity and instance indexing |
| EpisodeId | Associates one lifecycle episode with asynchronous completions |
| EffectId | Manual disposal and diagnostics |
| BindingId | Identifies the concrete Binding captured by a CommittedView |
| StepId | Identifies one activation or effect forward step completion |
| ComponentId | Stable component definition name |
| RealmId | Service isolation |
| ServiceId | Stable service contract name |

Only FiberId enters Target. The other IDs do not change dependency identity.

A u64 identity is interpreted only within its owning Runtime. ComponentId/ServiceId are names stable across Runtimes. RealmId is an opaque handle carrying Runtime ownership.

## 3. FiberRecord

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
    settled_waiters: Vec<Reply<FiberState>>,
    dispose_waiters: Vec<Reply<DisposeReport>>,
}
~~~

Stable fields:

- the FiberId represented by the map key, and parent;
- descriptor;
- route;
- completion.

Mutable fields:

- implementation;
- config;
- mount_effect: cleared from the parent Episode's EffectId to None at O-Retire;
- lifecycle retirement, activation revision, and state;
- cleanup_running and cleanup reports;
- children;
- effects;
- next_inverse_seq.

The descriptor and route remain fixed throughout the Fiber's lifetime. To change them, retire the old Fiber and mount a new one.

## 4. LifecycleState

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

Reloading, Active, and Unloading are called installed states because all three retain a committed view.

Inactive holds no episode. Failed is the public projection of Inactive with a failure.

`cleanup_running` lives in FiberRecord so manual disposal during Active and whole-Fiber cleanup during Unloading share the same serial inverse scheduler. EffectState::Disposing represents a request for one effect; LifecycleState::Unloading represents a request for the whole Episode. No second request flag is stored.

## 5. Episode

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

An Episode captures implementation and config so updates cannot affect activation or cleanup already in progress. These are immutable value snapshots. External aliases of Component/Config must not change their activation semantics outside the Core API.

Whether an Episode continues is determined by comparison against the latest ActivationKey at a step boundary.

## 6. ComponentDescriptor

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
~~~

Requirement order is part of the semantics:

- ServiceIds are unique within requirements;
- Target item i corresponds to Requirement i;
- CommittedEntries preserve that order;
- snapshots and tests emit them in the same order.

ComponentDescriptor is a process-wide unique `'static` instance generated by a macro. Its `config_type` fixes the Rust Config type allowed for that logical component. Implementation replacement must reuse the same instance.

The descriptor macro calls the public, doc-hidden `Requirement::new::<C>`. This Core generic constructor fills the contract with `ContractVTable::of::<C>()`, including ServiceContract and Intercept TypeIds and erased identity/merge functions. InterceptValue's two TypeIds identify its source contract and value type. Mount therefore need not recover function pointers from a TypeId, and different contracts using the same Rust metadata type cannot be confused.

## 7. Binding

~~~rust
struct BindingSlot {
    service: ServiceId,
    realm: RealmId,
}

struct BindingRecord {
    id: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    owner_effect: EffectId,
}
~~~

Visibility is a derived property:

~~~text
visible(binding) =
    binding is registered
    and provider.state == Active
    and not provider.retired
~~~

- A Reloading Provider's Binding is not visible.
- An Active Provider's Binding is visible while the Provider is not retired.
- When the Provider enters Unloading, Core removes the record from the store.
- Manually disposing a provide effect removes its corresponding record.
- CommittedView retains its own endpoint Arc. Existing Consumers do not depend on the continued existence of BindingRecord.

## 8. Target and CommittedView

~~~rust
struct Target {
    providers: Vec<FiberId>,
}

struct ActivationKey {
    target: Target,
    revision: u64,
}

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

Target expresses who provides a dependency. CommittedView also captures exactly what is read.

BindingId appears in CommittedView to:

- identify the concrete provision captured by that Episode;
- support snapshots and diagnostics;
- distinguish successive Bindings from the same Provider in the same Slot.

It does not participate in Target equality.

## 9. The relied relationship

~~~text
relied(provider P) =
    exists installed fiber F:
        F.id != P
        and F.episode.committed contains entry.provider == P
~~~

The dependency graph is distinct from the parent tree:

    Consumer Fiber
        -- CommittedEntry -->
    Binding
        -- provider -->
    Provider Fiber

The implementation maintains the consumers_by_provider reverse index. CommittedView always remains its source of truth.

## 10. EffectRecord

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
    forward_waiters: Vec<Reply<Result<EffectForwardOutcome, EffectError>>>,
    dispose_waiters: Vec<Reply<EffectDisposeReport>>,
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

enum EffectState {
    Applying,
    Armed,
    Disposing,
}

struct RunningMeta {
    episode: EpisodeId,
    seq: u64,
    owner: InverseOwner,
    label: Option<Arc<str>>,
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

The Episode accumulator is the sole owner of InverseEntries. InverseOwner::Activation identifies output from the activation driver; InverseOwner::Effect(id) identifies output from a Scope effect. EffectRecord stores no second sequence index. Disposal and snapshots filter the accumulator by owner directly.

An InverseEntry still in the accumulator is Armed. After the scheduler takes an entry, the state of a User/Withdraw action spanning turns moves into FiberRecord.cleanup_running. Completed entries never return to the accumulator.

User actions run outside the Actor. RetireFiber applies O-Retire synchronously in the Actor. WithdrawBinding deletes a Binding only if the Slot's current BindingId matches the action and captures `(FiberId, EpisodeId)` pairs whose CommittedView actually holds that BindingId. Watchers locate potentially affected Fibers; watching alone creates no cleanup wait. The Core releases its endpoint reference once each captured Episode is no longer installed or the same Episode reaches Active with a key equal to the latest ActivationKey. Later Episodes do not extend this wait. Republishing the same Slot from the same Provider can restore Target, allowing the original Episode to continue with the old endpoint. If the old Binding has been withdrawn and replaced by a new Binding in the same Slot, the action must not delete that replacement. Core actions do not invoke component code.

seq is the Fiber-local application order. EffectId identifies one effect invocation; seq orders each of its actual inverses across the entire Fiber.

A running inverse is identified by its owning Fiber's map key plus RunningMeta(episode, seq).

An effect iterator may produce multiple InverseEntries. EffectHandle disposal selects entries owned by that EffectId. Fiber unload selects the entire episode accumulator.

A requested EffectRecord completes and is removed in the same Actor turn once its driver reaches a boundary and no entry for its owner remains in the accumulator or cleanup_running. This condition also holds for an empty entry set, so an effect that produced no inverse can finish disposal or whole-Fiber cleanup. Runtime does not manufacture a placeholder entry.

## 11. ContextRoute

~~~rust
struct ContextRoute {
    parent: Option<Arc<ContextRoute>>,
    isolate_delta: Map<ServiceId, RealmId>,
    intercept_delta: Map<ServiceId, InterceptValue>,
}
~~~

A route is an immutable delta chain. Resolution searches from the inside out for the nearest isolate. Intercept metadata is combined according to the ServiceContract's merge rules.

A Fiber's route is fixed when it is mounted. A derived isolate only affects topology when used for later mount/provide operations. The current Fiber's service lookup still reads its existing CommittedView. A derived intercept affects typed clients subsequently constructed from that Context.

## 12. Erased implementation types

These are the minimal erased adapter types. Except for explicitly identified adapter plumbing, they are crate-private in `linorun-core`, not additional runtime mechanisms:

~~~rust
type ErasedConfig = Arc<dyn Any + Send + Sync>;
trait DynComponent: Send + Sync + 'static { /* Component vtable */ }
pub trait ErasedEndpoint: Send + Sync + 'static {
    fn as_any(&self) -> &(dyn Any + Send + Sync); // doc-hidden adapter hook
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
struct RuntimeTag; // Runtime ownership tag for RealmId
struct RuntimeInner {
    runtime_tag: Arc<RuntimeTag>,
    default_realm: RealmId,
    next_realm: AtomicU64,
    sender: UnboundedSender<Command>,
}
struct RuntimeHandle(Arc<RuntimeInner>);
enum ActivationDriver {
    Init { /* implementation + ComponentContext + config */ },
    Once(ActivationFuture),
    Steps(ActivationStream),
    Exhausted,
}
type ErasedEffectDriver = Pin<Box<dyn EffectDriver>>;
struct ActivationCompletion {
    result: Result<DriverEvent, ActivationFailure>,
    driver: ActivationDriver,
}
struct EffectStepCompletion {
    result: Result<DriverEvent, EffectError>,
    driver: ErasedEffectDriver,
}
struct TurnOutput {
    tasks: Vec<RuntimeTask>,
    deferred_drops: Vec<Box<dyn Send + 'static>>,
    events: Vec<RuntimeEvent>,
}
struct EffectCompletion; // Completion cell shared by EffectHandle and Actor
struct FiberCompletion;  // Preserves Disposed/report for FiberHandle after O-Remove
struct ServiceRefInner<C: ServiceContract>; // Private state of the committed call base
~~~

ErasedEndpoint, ServiceRef, CallContext, Requirement, and the ComponentDescriptor constructor are public opaque or doc-hidden macro adapter plumbing. Ordinary components receive and use the contract-local `C::Client`. InterceptValue and ContractVTable remain internal to Core; the other types in this section are also crate-private.

A step task exclusively owns its ActivationDriver or ErasedEffectDriver. The Actor records only completion identity in LifecycleState::Reloading.step or EffectRecord.running_step. Each task polls one step, then transfers the driver together with its DriverEvent/error back to the Actor in the completion. Only a driver returning `Yield { has_next: true }` can continue into the next RuntimeTask; the others are destroyed through deferred-drop. The control plane and task never own the same driver simultaneously.

L-Begin constructs `ActivationDriver::Init` without calling Component user code. On the first task poll, the wrapper temporarily becomes Exhausted, calls `Component::activate`, installs the returned Once/Steps, and continues polling the current step. A synchronous panic therefore leaves a valid Exhausted wrapper that can return to the Actor with an ActivationFailure completion and be dropped after commit.

`ContractVTable::of::<C>()` is Core's internal generic constructor called by `Requirement::new::<C>` and erased provide commands. The vtable contains only TypeIds and function pointers and is copyable; the Runtime contract map retains a copy. Its identity/merge adapters check every InterceptValue's contract_type and intercept_type and tag the result with the same pair of TypeIds.

FiberCompletion retains the subtree DisposeReport collected between O-Retire and O-Remove and shares it among all FiberHandle waiters. The mount inverse does not wait for this completion. When a descendant fails, the Actor directly appends the failure to every already retired collector in the parent chain.

The default Realm has local 0; next_realm starts at 1. The Realm allocator only produces opaque identities tied to RuntimeTag and does not read or modify RuntimeCore. create_realm is therefore synchronous. RootContext/Context/ComponentContext::default_realm likewise only clones the RuntimeTag and uses local 0.

ComponentContext combines the Context and Scope for one Fiber/Episode into the direct component interface. Deriving a route does not change the Scope owner. Runtime's convenience operations use the Root owner. `on_cleanup` only saves a User inverse through an ordinary Effect and waits for registration. All these entry points reuse the data model above.

`UnloadOutcome` is the internal result of `LifecycleState::Unloading`: Normal or ActivationFailed with a revision. `EpisodePhase` is only a stable snapshot DTO projection; see [Observability](../operations/observability.md).
