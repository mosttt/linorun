# Crate and module boundaries

[简体中文](../../zh/architecture/crate-boundaries.md) | English

## 1. Workspace

The workspace contains three crates:

~~~text
linorun-core
linorun-macros
linorun-testkit
~~~

`linorun-core` has a normal dependency on `linorun-macros` and re-exports all four macros, so an application only needs Core. Generated macro code uses Core's public adapter plumbing. The macro crate itself depends on Core only through dev-dependencies for downstream tests and examples. `linorun-testkit` uses only Core's public handles, snapshots, events, and `TaskSpawner`; Core does not depend on testkit.

## 2. linorun-core

Current module layout:

~~~text
src/
  lib.rs
  component.rs
  context.rs
  effect.rs
  error.rs
  id.rs
  lifecycle.rs
  observability.rs
  route.rs
  runtime/
    mod.rs
    api.rs
    completion.rs
    task.rs
    tests.rs
  service.rs
  state.rs
  topology.rs
~~~

### component.rs

Defines:

- `Component` and its erased adapter;
- `ComponentDescriptor` and `Requirement`;
- `Activation::Once/Steps` and the `run/once/steps` constructors;
- the type erasure boundary for Config snapshots.

`Component::activate` is not called during a Runtime Actor turn. The Actor only builds the Init driver; the call runs inside a `RuntimeTask`.

### context.rs and route.rs

Define:

- `Context`, `Scope`, and `ComponentContext`;
- committed service lookup;
- the immutable route chain;
- `isolate` and the `intercept` fold.

Context holds no writable RuntimeCore reference. Scope mutations construct commands and await Actor replies. The direct `service/provide/mount/effect/on_cleanup` operations on `ComponentContext` delegate to the existing Context/Scope. Derived `isolate/intercept` contexts retain the same Scope and resource owner. The original Context/Scope APIs remain public.

### effect.rs

Defines:

- `EffectDriver`, `IntoEffectDriver`, and `DriverEvent`;
- `InverseFn`/`InverseFuture`;
- the `effect::sync/once/iter/steps/inverse` adapters.

Adapters only retain driver ownership. The forward closure starts executing when its task is first polled.

### lifecycle.rs

Implements the independent Fiber lifecycle reducer:

- `LifecycleState` and Episode;
- revision, retirement, and failure outcome;
- boundary decisions for Begin/Iter/Finish/Divert/Raise/Leave/Unload.

This module does not depend on an executor or call user code.

### service.rs and topology.rs

`service.rs` defines `ServiceContract`, `ServiceRef`, `CallContext`, erased endpoints, and the contract vtable.

`topology.rs` defines:

- `BindingSlot`/`BindingRecord`/`BindingTable`;
- `Target`, `ActivationKey`, and `CommittedView`;
- the Runtime-local contract registry;
- dependency resolution.

Target contains only Provider FiberIds. Endpoints and BindingIds remain fixed in CommittedView entries.

### runtime/

The production control plane is split by responsibility:

- `mod.rs`: synthetic Root, Actor commands, Fiber/Effect/Episode records, watcher reconciliation, withdrawal guard, serial cleanup scheduler, and shutdown/snapshot/event commits;
- `api.rs`: Runtime, Builder, Root/Handle APIs, RuntimeHandle, and TaskSpawner;
- `completion.rs`: terminal states and results shared by handles and the Actor;
- `task.rs`: user task execution, panic capture, and completion delivery;
- `tests.rs`: internal Core runtime tests.

One `ActorCore` owns all mutable control-plane state. Asynchronous tasks return completions and never modify records directly. `Runtime::new` accepts a TaskSpawner or a task submission closure. Direct `mount/provide/effect/on_cleanup` methods delegate to RootContext. `on_cleanup` registers an inverse through an existing Effect and waits for its forward boundary before returning. A dedicated RegisterCleanup command lets an already started step in the same Episode submit its remaining inverse during exit. It reuses the same Effect state machine and cleanup queue.

The watcher index represents Slot change notifications; actual CommittedView dependencies determine cleanup waits. Manual Binding withdrawal also captures each Consumer's EpisodeId so that later Episodes of the same Fiber cannot extend the wait.

### observability.rs, state.rs, error.rs, and id.rs

- `observability.rs`: public snapshot/event DTOs;
- `state.rs`: public coarse-grained state enums;
- `error.rs`: leaf errors, API errors, and reports;
- `id.rs`: opaque Runtime-local IDs and stable names.

## 3. linorun-macros

The macro crate provides two function-like procedural macros and two attribute macros, all re-exported by Core:

### #[component]

Generates a static descriptor and a Component implementation for an async function component, delegating to `Activation::run`.

### #[service_impl]

Converts ordinary async Provider methods into the boxed Future signatures required by the existing object-safe Provider trait.

### component_descriptor!

Generates a unique `LazyLock<ComponentDescriptor>` and fixes Config, Requirements, and provides through Core's doc-hidden constructors.

### service_contract!

Generates:

- a contract marker;
- an object-safe Provider trait;
- a contract-local Client newtype and asynchronous methods;
- a private erased endpoint adapter;
- a `ServiceContract` implementation.

Generated code uses public adapter boundaries such as `Requirement::new`, `ComponentDescriptor::new`, `ServiceRef::dispatch_parts`, and `ErasedEndpoint::as_any`. It does not access Core's private records.

## 4. linorun-testkit

Testkit modules:

~~~text
src/
  lib.rs       Scenario
  executor.rs  DeterministicExecutor
  control.rs   controlled Future and StepStream
  trace.rs     TraceRecorder/EventTag
  snapshot.rs  normalize/check snapshot
  model.rs     ReferenceModel
~~~

`DeterministicExecutor` implements the production `TaskSpawner`. It manually polls RuntimeTasks through a safe `ArcWake` queue; it introduces no alternate state mutation path into Core.

`Scenario` connects the Runtime, executor, and event sender. The snapshot checker and reference model only read DTOs or accept explicit ModelCommands. They receive no mutable reference to Core internals, so test and production builds share the same control-plane implementation.

## 5. Public and private boundaries

Public:

- Runtime, RootContext, ComponentContext, Context, and Scope;
- Component, ServiceContract, and macro adapter constructors;
- FiberHandle and EffectHandle;
- ServiceRef/CallContext/ErasedEndpoint adapter plumbing;
- opaque IDs and coarse-grained states;
- errors, reports, snapshots, and events;
- TaskSpawner/RuntimeTask.

Private:

- Actor commands and completion tokens;
- FiberRecord, EffectRecord, and Episode payload;
- LifecycleState details;
- Binding store, contract vtables, and watcher/consumer indexes;
- accumulator entries/actions and running inverses;
- RuntimeHandle.

This private layout prevents components, macros, and testkit from bypassing the Actor reducer to mutate state.

## 6. Dependency direction

~~~text
component ─┐
service ───┼──> topology/lifecycle ──> runtime
effect ────┘             │               │
context/route ───────────┘               ├──> observability
error/id/state ──────────────────────────┘

linorun-core    --normal dependency + re-export--> linorun-macros
linorun-macros  --generated adapter API / dev dependency--> linorun-core
linorun-testkit --public test API--> linorun-core
~~~

The Core runtime library does not bind to Tokio. Tokio is only a dev-dependency used by Core examples. The host supplies a TaskSpawner or a task submission closure satisfying the same contract. That interface can connect a thread pool or testkit's deterministic executor.

Implementation entry points: [Core manifest](../../../crates/linorun-core/Cargo.toml), [macro manifest](../../../crates/linorun-macros/Cargo.toml), and [Runtime Actor](../../../crates/linorun-core/src/runtime/mod.rs).
