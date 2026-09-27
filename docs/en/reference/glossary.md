[简体中文](../../zh/reference/glossary.md) | [English](glossary.md)

# Glossary

## Activation

Forward computation performed by a Component during Reloading. It may be a single Future or a
stream that produces inverses step by step.

## ActivationKey

The combination of Target and activation revision. A Fiber compares its committed key with the
latest key to decide whether to continue, Leave, or Divert.

## Active

The state in which Fiber activation is complete and its committed key still equals the latest
key. Only registered Bindings of Active Providers are visible to new Consumers.

## Actor

The control-plane task that exclusively owns RuntimeCore. It applies reducers, maintains
watchers, and schedules user Futures after committing state.

## Binding

One concrete service provision. A Binding connects BindingSlot, Provider Fiber, endpoint,
and owner Effect.

## BindingId

The identity of a particular BindingRecord within a Runtime. CommittedView retains it to
identify the provision captured by this Episode.

## BindingSlot

The pair of ServiceId and RealmId, identifying the unique location for service registration and resolution.

## CallContext

Committed diagnostic data received by a generated service adapter for a call: caller Fiber/Episode,
Provider, Binding, and effective intercept. It does not participate in Target equality.

## boundary

A decision point where an activation/effect step completes and returns its result to the Actor.
Runtime compares the latest ActivationKey, stops continuations, or starts cleanup only at boundaries.

## Component

An instantiable definition containing a descriptor, Config type, and activation implementation.

## ComponentDescriptor

A Component's unique, process-wide static interface: ComponentId, Config TypeId, Requirements,
and provides. Implementation replacement must reuse the same descriptor instance.

## ComponentId

A stable component definition name, not the identity of a running Fiber instance. Public `as_str`
returns the canonical name.

## CommittedEntry

The fixed resolution of one Requirement in CommittedView, containing BindingId, slot,
Provider FiberId, endpoint, and metadata.

## CommittedView

The dependency view captured at L-Begin and fixed throughout the Episode.

## Consumer

A Fiber whose CommittedView references a particular Binding.

## Context

A Component's immutable view of declared committed services, Realm routing, and interception
metadata. Undeclared services are not implicitly looked up along parent Fibers.

## ContextRoute

An immutable parent chain of isolate_delta and intercept_delta.

## ContractVTable

The erased ServiceContract description carried by Requirement: contract/intercept TypeIds and
metadata identity/merge adapters. Runtime fixes one vtable mapping for each ServiceId.

## Disposed

The public terminal state after a Fiber is retired, finishes cleanup, clears its children,
and leaves the registry.

## Effect

A forward change and the one or more inverses it produces.

EffectRecord tracks a Scope effect's driver and completion. The Episode accumulator exclusively
owns InverseAction; a User action exclusively owns its InverseFn.

## EffectHandle

A handle that requests manual disposal of an EffectRecord and waits for its result.

## EffectId

The identity of an effect invocation within a Runtime.

## StepId

A Runtime-local identity for an activation or effect forward step completion. Inverse completion
is identified by the combination `(FiberId, EpisodeId, seq)`.

## Effect sequence

The application order of inverses within a Fiber. Cleanup selects the largest seq among accumulator
entries covered by current requests, implementing LIFO.

## Episode

One Reloading–Active–Unloading lifecycle instance, owning EpisodeId, ActivationKey,
CommittedView, and accumulator.

## EpisodeId

The identity of an Episode within a Runtime, also used to correlate asynchronous completions.

## Episode accumulator

Stores all InverseEntries produced by an Episode in Fiber application seq order. It exclusively
owns inverses; cleanup selects the largest seq in the requested set and executes serially.

## Failed

The public projection of `Inactive { failure: Some(ActivationFailure) }`. Failed is a
settled/quiescent terminal state and does not retry automatically.

## Fiber

A running Component instance with its own lifecycle, effects, Bindings, and child set.

## FiberId

The identity of a Fiber within a Runtime, also used as Provider identity in Target.

## Inactive

The internal resting state. When not retired, it corresponds to Pending without failure or
Failed with failure. A retired Fiber still waiting for child removal has public state Unloading.

## Inertia

An asynchronous step that has started must complete. Target changes are handled only at step boundaries.

A step task exclusively owns the driver and transfers both the event/error and driver to the
Actor at every completion. Only a driver returned with `Yield { has_next: true }` may be placed
into another RuntimeTask by the Actor. The step starts when the Actor commits StepId and this
ownership transfer; the first executor poll is not an additional cancellation boundary.

The entire async body of `Activation::run` is one step; `on_cleanup` does not split its boundary.
After shutdown or retirement and before unload cleanup starts, an in-flight step of the same
Episode may still register an acquired resource's inverse through the dedicated `RegisterCleanup`
command. New work and expired Scopes remain rejected.

## installed

A Fiber in Reloading, Active, or Unloading. An installed Fiber retains a CommittedView.

## intercept

Attaches service-specific metadata to a derived Context. It affects calls without changing Target.

## InterceptValue

Core's erased representation of ServiceContract metadata, carrying contract TypeId, intercept
TypeId, and value together. A bare `Arc<dyn Any>` is not a valid metadata channel.

## Inverse

A recovery operation provided after an effect forward succeeds, invoked by Runtime at most once.

## InverseAction

An undo action exclusively owned by the Episode accumulator: a user FnOnce, RetireFiber, or
WithdrawBinding. Core-owned actions share seq/LIFO selection with user inverses, but execute
through the Actor reducer.

## isolate

Selects a RealmId for a ServiceId on a derived Context, changing BindingSlot resolution.
It affects subsequent mount/provide calls; it does not reselect endpoints already committed in
the current Fiber's CommittedView.

## L-Begin

An Inactive Fiber captures CommittedView and enters Reloading.

## L-Divert

A Reloading Fiber observes a different latest key at a step boundary and enters Unloading.

## L-Finish

A Reloading Fiber completes activation and enters Active.

## L-Iter

A Reloading Fiber collects an inverse and continues to the next activation step.

## L-Leave

An Active Fiber stops providing Bindings and enters Unloading.

## L-Raise

Enter Unloading after an activation step fails.

## L-Unload

After the withdrawal guard opens, run the accumulator, drop the Episode, and return to Inactive.

## O-Insert / O-Retire / O-Remove

The three Fiber-tree orchestration operations: insert a Fiber; unconditionally mark it retired;
and remove it from the registry once it is Inactive with no children/effects/Bindings.
A mount inverse performs only O-Retire.

## Pending

The public state of an Inactive, non-retired Fiber without failure or ActivationKey.
A leaf with no Requirements can start activation directly from Pending.

## Provider

A Fiber that owns a Binding.

## quiescent

A Runtime state with no lifecycle step, running effect step, running inverse, reconcile work,
or pending O-Remove; no retired-but-unremoved Fiber in the registry; and every Fiber having
reached its latest ActivationKey or a settled Failed/Disposed terminal state.

## Realm

A service resolution domain identified by RealmId. Each Runtime's default Realm is its
RuntimeTag/local 0, available through RootContext/Context::default_realm.

## reconcile

Select the next lifecycle rule from a Fiber's current state and latest ActivationKey.

## Reloading

A Fiber executing activation, with a fixed CommittedView and an incrementally growing accumulator.

## Requirement

A Required service coeffect declared by a Component.

## relied

A Provider appears in an installed Consumer's CommittedView.

## Runtime

A Core instance holding the Fiber registry, Binding store, watchers, and Root effects.

## RuntimeEvent

A structured event sent by the Actor after state commits, carrying a monotonically increasing
Runtime-local sequence. It supports traces, logs, and Host metrics without feeding back into
Runtime state.

## Scope

A Component's mutation capability for creating effects, providing services, and mounting children.

## settled

A Fiber has completed immediately runnable lifecycle reconciliation and reached a Pending,
Active, Failed, or Disposed boundary. Settled does not require independent Scope effects on an
Active Fiber to have completed; those effects still prevent Runtime quiescence.

## ServiceContract

The contract linking ServiceId, Provider type, contract-local Client, endpoint adapter, and
interception metadata. The macro generates a Client newtype in the contract's own crate.

## Service client

The `C::Client` returned by `Context::service::<C>()`. Call methods live on the contract-local
newtype, which internally holds a Core-constructed ServiceRef.

## ServiceId

A stable service contract name. Public `as_str` returns the canonical name.

## ServiceRef

An opaque call base constructed by Core for CommittedEntry, fixing endpoint and effective
intercept, then wrapped by ServiceContract into `C::Client`. The doc-hidden dispatch hook is
reserved for generated adapters. Components must not continue using Client/ServiceRef after
their owning Episode finishes.

## TaskSpawner

The nonblocking RuntimeTask submission interface injected by the Host. Spawn may only schedule;
it must not synchronously poll the task on the Actor's call stack.

## Target

A sequence of Provider FiberIds in Requirement order.

## Unloading

The internal state in which a Fiber has stopped providing Bindings to new Consumers and is
waiting for Consumers or executing its Episode accumulator. The public API also uses this state
for a retired Fiber that is already Inactive but has not yet left the registry.

## watcher

A reactive index from BindingSlot to potentially affected Fibers.

## withdrawal

Provider-wide withdrawal first removes BindingRecords, waits for committed Consumers to leave,
and then runs the Episode accumulator. Manual provide withdrawal removes one BindingRecord and
notifies every relevant watcher, but waits only for Consumer Episodes committed to that exact
BindingId. The wait ends when the captured Episode ends or that same Episode is Active with a
current key. Later Episodes do not extend the wait. This uses neither the public settled
condition of `FiberHandle::wait_settled` nor child O-Remove, and creates no cleanup wait edges
from watchers without committed dependencies. Old CommittedViews may continue retaining old endpoints.

## coeffect

An environmental input that a Component must declare before reading. Linorun represents it
through Requirement and CommittedView.

## emission

An external operation that an inverse cannot undo exactly, such as sending a message or making
a payment. A business compensation protocol is responsible for it.

## RootContext

The Host interface to the synthetic Root Fiber. It creates Realms, effects, provisions, and
top-level mounts; all changes still enter the Root Episode accumulator. Root uses the reserved
ComponentId `linorun/root@1`.

## withdrawal guard

The ordering condition that makes a Provider wait, before starting Episode cleanup, until every
installed Consumer has dropped its CommittedView pointing to that Provider.

## ActivationFailure / EffectError / CleanupError / CleanupFailure

Respectively: activation forward, independent effect forward, and inverse leaf errors; and a
cleanup aggregation record carrying owner, Fiber, Episode, seq, and an optional label.
