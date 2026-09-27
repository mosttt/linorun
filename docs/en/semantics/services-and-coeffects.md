[简体中文](../../zh/semantics/services-and-coeffects.md) | [English](services-and-coeffects.md)

# Services, Bindings, and Coeffects

## 1. A service is a runtime coeffect

A Component uses Requirements to declare what it needs from its environment:

~~~rust
struct Requirement {
    service: ServiceId,
    contract: ContractVTable,
    declared_intercept: Option<InterceptValue>,
}
~~~

A Requirement is a reactive coeffect:

- When satisfied, an Inactive Fiber may Reload.
- When unsatisfied, an Active Fiber must Leave.
- When Provider identity changes, the Consumer must activate again.

All Requirements in Core 0.1 are Required.

## 2. Service identity

ServiceId is a stable name, for example:

    linorun/logger@1
    app/database@1

Rust TypeId validates adapters within this build artifact; ServiceId is the service identity.

ServiceContract connects ServiceId, endpoint type, and interception metadata. Runtime fixes one
Contract TypeId/vtable per ServiceId. Mount validates descriptor Requirements and their associated
route intercepts; provide validates the contract identity of generic `C`. Both reject contracts
with the same name but a different TypeId, then atomically commit missing mappings together with
the operation result. Provide does not validate route intercepts unrelated to that Binding.

The component descriptor macro calls the doc-hidden `Requirement::new::<C>`. This generic Core
constructor stores `ContractVTable::of::<C>()` and typed declared metadata in Requirement.
Neither the macro nor Runtime attempts to reflect or look up Rust types from TypeId.

## 3. BindingSlot

~~~rust
struct BindingSlot {
    service: ServiceId,
    realm: RealmId,
}
~~~

The same ServiceId denotes different Slots in different Realms.

Each Slot has at most one registered BindingRecord at a time. Once Core removes the record,
the Slot may be registered again.

## 4. Binding visibility

BindingRecord:

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

A Binding is visible when:

    BindingRecord registered
    and provider lifecycle == Active
    and !provider.retired

Consequently:

- A Reloading Provider may construct a service, but Consumers cannot see it early.
- The service becomes visible at L-Finish.
- When the Provider enters Unloading, its BindingRecord is removed from the store and immediately leaves new resolution.
- During Unloading, existing Consumers still retain the endpoint through their own CommittedViews.

## 5. Target

To compute Target:

1. Resolve the Realm of each Requirement.
2. Look up its Slot.
3. Check that the Binding is visible.
4. Collect the provider FiberId.
5. Return None if any Requirement is missing.

~~~rust
struct Target {
    providers: Vec<FiberId>,
}
~~~

Target equality compares only Provider FiberIds.

Examples:

| Change | Does Target change? |
|---|---|
| The same Provider updates an internal value in place | No |
| Provider A is replaced by Provider B with an equal value | Yes |
| BindingId changes but ProviderId stays the same | No |
| Interception metadata changes | No |
| A Realm change resolves to another Provider | Yes |

## 6. CommittedView

L-Begin captures:

~~~rust
struct CommittedEntry {
    binding: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    declared_intercept: Option<InterceptValue>,
}
~~~

CommittedView serves as:

- Dependency lookup during activation.
- A stable service view while Active.
- Access to the old Provider during cleanup.
- The source of truth for relied relationships.

Runtime does not replace an entry with a new Binding midway through an episode.

## 7. Watcher

Each Requirement has a BindingSlot watcher:

    watchers[slot] contains consumer FiberId

The following events trigger reconciliation of the relevant watchers:

- Insertion of a BindingRecord.
- Provider L-Finish.
- Manual disposal of a provide effect removing its BindingRecord.
- A Provider entering Unloading and removing its BindingRecord.

A watcher means only that changes to this Slot may affect the Fiber. CommittedView represents
actual committed dependencies.

## 8. Withdrawal

To withdraw a Binding manually:

1. EffectHandle requests disposal.
2. The cleanup scheduler selects that effect's seq.
3. The Actor removes the BindingRecord and refreshes watchers only if the Slot's current BindingId still matches. A replacement Binding is unaffected by the old inverse.
4. Consumers perform L-Divert, L-Leave, or retain the same Target according to their state.
5. The provide inverse waits only for Consumer Episodes committed to the exact withdrawn BindingId.
6. The inverse releases Core's endpoint reference and completes.

Notifications still reach every watcher of the Slot. The wait set captures only installed
Consumers retaining that BindingId, together with their EpisodeIds. Watchers without a committed
dependency do not join the wait, preventing added cleanup wait cycles outside the committed graph.

Each wait finishes when the captured Episode ends, including internally Inactive, a later
Episode starting, or Fiber removal. Later Episodes do not extend the wait. The same Episode
also qualifies if it becomes Active with episode.key equal to the latest ActivationKey.
A retired Consumer that completes its own L-Unload has ended the old Episode even if children
remain and its public state is still Unloading. This differs from the public settled condition
of `FiberHandle::wait_settled` and does not wait for child O-Remove.

The wait does not require unrelated independent Scope effects on an Active Fiber to complete.
If the same Provider returns, a Consumer may continue retaining the old Binding in its
CommittedView; the endpoint Arc remains valid. A Consumer's activation failure stays on that
Consumer and does not become a Provider cleanup failure.

When an entire Provider leaves, L-Leave removes all its BindingRecords, then waits for every
committed Consumer. The provide inverse retains an endpoint Arc until its LIFO turn runs.

## 9. relied

~~~text
relied(provider P) =
    exists installed consumer C:
        C.id != P
        and C.committed contains entry.provider == P
~~~

Installed includes Reloading, Active, and Unloading.

This guarantees:

- Once a Loading Consumer has committed a Provider, that Provider cannot clean up first.
- Consumer cleanup may use the old endpoint.
- The dependency edge disappears only after the Consumer finishes L-Unload and drops CommittedView.

## 10. Target churn during Loading

Suppose a Consumer starts Loading with Provider P:

1. P temporarily leaves, making Target None.
2. The same P becomes visible again before the current async step completes.
3. Target returns to P.
4. ActivationKey is still equal when the step completes.
5. The Consumer continues into Active.
6. Its CommittedView still contains the originally captured endpoint.

This recovery follows from the unchanged Provider identity and the ActivationKey check at the step boundary.

After the old Binding leaves the store, a new Binding may register in the same Slot.
Target still compares only Provider FiberIds.

## 11. Typed Client and ServiceRef

Context constructs a Core-owned ServiceRef for CommittedEntry, then ServiceContract wraps it
in a contract-local typed Client:

~~~rust
pub struct ServiceRef<C: ServiceContract> {
    inner: Arc<ServiceRefInner<C>>,
}

pub trait ServiceContract: Sized + Send + Sync + 'static {
    type Client: Clone + Send + Sync + 'static;
    fn client(reference: ServiceRef<Self>) -> Self::Client;
}
~~~

ServiceRefInner stores the committed endpoint, effective intercept, and caller/provider diagnostic
identities. All fields remain private.

Client/ServiceRef:

- It does not query the Binding store again.
- It does not participate in Target calculation.
- It does not expose the erased endpoint through Deref.
- Construction merges the current Context's interception metadata.
- It is used only in component logic belonging to its episode.

Call methods are defined on a Client newtype in the contract's own crate. ServiceRef's doc-hidden
adapter hook is reserved for generated macro code.


## 12. Service confinement

The Context passed to Scope::mount determines only the child's route, Realm, and intercept.
The child's service lookup still reads only its own CommittedView. An undeclared service returns
`ServiceAccessError::UndeclaredDependency`. The Fiber parent relationship controls only
mount/removal; it does not create hidden reactive dependencies.

## 13. Single-Provider model

Each BindingSlot has one Provider at a time. A service with multiple backends is represented by
an explicit Broker Component:

    Consumer -> Broker service -> backend set

Core observes the Broker Provider's identity; the Broker's service contract manages the backend
set and selection.
