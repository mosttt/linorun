# Architecture overview

[简体中文](../../zh/architecture/overview.md) | English

Component authors normally start with `#[component]`, `ComponentContext`, and `on_cleanup`. This page explains the coordination behind those entry points for readers who need iterative Effects, dependency switching, or runtime maintenance. The convenience APIs do not change these mechanisms.

## 1. Core idea

Linorun separates dynamic component systems into two orthogonal questions:

- Time: can changes installed by a component be completely reversed when it exits?
- Space: when a component's service dependencies change, can it automatically activate, leave, and reconnect?

Effects address time; coeffects/requirements address space. Context brings both into the component's execution environment.

## 2. Main objects

    Runtime
      ├── Fiber registry
      ├── Binding store
      └── Dependency watchers

    Fiber
      ├── Component descriptor
      ├── Lifecycle state
      ├── Episode / CommittedView
      ├── Episode accumulator
      └── Child fibers

    Context
      ├── Service lookup
      ├── Isolation route
      └── Interception metadata

A Component is a definition; a Fiber is an instance. One Component may create multiple Fibers. Each independently goes through Pending, Loading, Active, Unloading, Failed, and Disposed.

## 3. Control plane and user code

In Rust's multithreaded environment, one logical Runtime Actor is the linearization point:

- The Actor modifies Fibers, Bindings, and watchers.
- Activation steps and inverses execute outside the Actor.
- Results return to the Actor with an EpisodeId.
- The Actor does not call or await user Futures while holding state borrows or locks. Removed erased values are destroyed through a deferred-drop list after releasing the state borrow.

The Actor is not the service data plane. Ordinary service calls use the Fiber's CommittedView directly rather than passing through the Actor each time.

## 4. Dependency data flow

    Provider effect
        |
        v
    BindingSlot -> BindingRecord
        |
        v
    watcher refresh
        |
        v
    Consumer Target
        |
        v
    L-Begin -> CommittedView

A registered Binding is visible to new Consumers if and only if its Provider is Active and not retired. Bindings from Loading or Unloading Providers do not participate in new Targets.

Target contains only Provider FiberIds in Requirement order. A service value change does not trigger a reload; a Provider identity change does.

## 5. Episode

A complete episode is:

    Reloading -> Active -> Unloading

It retains:

- its EpisodeId;
- its ActivationKey;
- its CommittedView;
- an implementation/config snapshot;
- its Episode accumulator.

A dependency change during an episode changes the latest Target, not the CommittedView.

## 6. Provider/Consumer ordering

When a Provider leaves:

    Provider L-Leave
      -> remove Provider BindingRecord from store
      -> Consumer Target changes
      -> Consumer L-Leave / L-Divert
      -> Consumer cleanup
      -> Consumer leaves installed state
      -> Provider withdrawal guard opens
      -> Provider cleanup

Consumer cleanup can still read the old CommittedView and use the old Provider to return resources. Committed dependency relationships establish this order.

Manual withdrawal of one provide captures only Consumer Episodes whose CommittedView holds that BindingId. The wait ends when each captured Episode is no longer installed or returns to Active with a key matching the latest value. Later Episodes of the same Fiber and watchers merely subscribing to the Slot do not extend the wait. Republishing the same Slot from the same Provider preserves Target identity, so the original Episode may continue holding the old endpoint.

## 7. Effect ordering

Each successful effect step that produces an inverse receives an increasing Fiber-local sequence number. A step that produces no inverse creates no placeholder entry:

    forward 1 -> inverse seq 1
    forward 2 -> inverse seq 2
    forward 3 -> inverse seq 3

Cleanup:

    inverse 3
    inverse 2
    inverse 1

Inverses in the same Fiber execute serially. An inverse failure records an error without skipping the remaining inverses.

## 8. Inertia

Once an asynchronous step starts, the state machine cannot pretend it was cancelled:

    Loading step in flight
      + Target changes
      -> step still lands
      -> collect inverse
      -> compare latest ActivationKey
      -> continue or divert

If Target returns to its original value before the step lands, the episode continues. No new Episode is created in that case.

A step starts at the Actor commit that records its StepId and transfers its driver to a RuntimeTask, not at the executor's first poll. A scheduled but unpolled step therefore also has inertia. Only the next Actor boundary can decide whether to submit the following iteration.

## 9. Context derivation

Context is an immutable value:

    root context
      ├── isolate(service, realm)
      └── intercept(service, metadata)

`isolate` changes the BindingSlot used by mount/provide operations on the derived Context.

`intercept` only changes call metadata. It does not change BindingSlot, Target, or the Fiber lifecycle.

`Context::service` reads only the CommittedEntry for a Requirement declared by the current Fiber and returns a contract-local typed Client. A Fiber's parent represents mount/removal ownership; it does not supply implicit service capabilities.

Deriving isolate from an existing Fiber Context does not reselect an already committed endpoint. That route only affects later mount/provide operations. An intercept overlay affects new Clients constructed from the derived Context.

## 10. Encapsulation boundaries

The following boundaries are part of the final design and prevent components from bypassing Core semantics:

- Context, ServiceRef, and BindingRecord fields are private; components use contract-local Clients.
- Provider cleanup decisions are centralized in internal functions.
- Service endpoints are accessed through a consistent call boundary.
- Detailed lifecycle state is not exposed as a public mutable structure.

## 11. Convenience entry points and the complete API

`#[component]` and `Activation::run` remove trait and boxing boilerplate from ordinary async setup. ComponentContext directly provides service access, resource registration, and child operations. `on_cleanup` returns a handle after registration, allowing early disposal. Runtime accepts a task submission closure and exposes Root operations directly.

For more precise boundaries, `Component`, `Activation::Once/Steps`, `EffectDriver`, `effect::sync/once/iter/steps`, separate Context/Scope, RootContext, RuntimeBuilder, and all snapshots/events remain available. They share the same Actor, Episode accumulator, and error/cleanup protocol as the convenience entry points.
