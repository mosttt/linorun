# Concepts

[简体中文](../../zh/guides/concepts.md) | [English](concepts.md)

Linorun manages component startup, dependency changes, and shutdown. Your application implements the
business behavior; Linorun runs registered cleanup at the appropriate time. A first component only
needs `mount`, `on_cleanup`, and `shutdown`; see [Getting started](getting-started.md) for code. Read the
concepts below as they become relevant to your task.

## A component can start more than once

`worker()` is a component definition. `runtime.mount(worker(), config).await` mounts an instance and
returns a `FiberHandle` that controls it. Mounting the same definition multiple times creates separate
instances with their own configuration, state, and resources.

A configuration update, explicit restart, or dependency change can end an instance's current activation
and start a new one. Its handle stays the same. Each activation holds its own configuration and
dependency snapshots.

| Name | Meaning | When it matters |
| --- | --- | --- |
| `Runtime` | Runtime for a group of components | Create it, mount components, shut it down |
| `Component` | Definition that can be activated repeatedly | Write startup logic and declare required/provided services |
| Fiber | Instance created by one mount | Update, inspect, or stop it through `FiberHandle` |
| Episode | One activation, from startup through completed cleanup | Understand why configuration and dependencies do not switch midway |
| Effect | Acquisition and cleanup work managed by Runtime | Manage tasks, connections, and other external resources |
| Inverse | An effect's cleanup operation | Actually release the resource in its callback |

Everyday component code uses `ComponentContext`; it does not manually create Fibers or Episodes or
maintain a cleanup stack.

## Returning success means startup is complete

After the component's async function returns `Ok(())`, its services and registered resources remain
active. Linorun keeps its cleanup callbacks until a dependency change, restart, disposal, or Runtime
shutdown requires them.

`FiberState` exposes the observable states:

| State | Meaning |
| --- | --- |
| `Pending` | Waiting for all declared dependencies |
| `Loading` | Executing startup logic |
| `Active` | Startup completed |
| `Unloading` | Waiting for a dependency boundary or running cleanup |
| `Failed` | Startup failed and the associated recovery completed |
| `Disposed` | Instance permanently stopped |

Transitions may still be running after `mount`, `restart`, or `update_config` returns. Await
`fiber.wait_settled()` and inspect the returned state when the result matters. `Pending` can also be a
settled state: no further work is possible until a missing dependency appears.

## Cleanup reverses registration order

If startup opens a database, starts a task, and publishes a service, shutdown needs to withdraw the
service, stop the task, and close the database. Register cleanup immediately after each step; later
registrations are cleaned up first.

`ctx.on_cleanup(label, callback).await` waits for registration to complete. Within one component,
cleanup is serial and each operation runs at most once. Errors go into a report, and cleanup continues
with earlier entries. Independent components can progress concurrently; there is no single cleanup stack
covering the entire Runtime.

`ctx.effect(...)` starts managed acquisition work. Receiving its handle does not mean acquisition is
finished; use `wait_forward()` when needed. See [Writing components](../api/component-authoring.md).

## Services define calls and lifecycle dependencies

`service_contract!` defines methods. A provider implements them and calls `provide`. A consumer declares
the contract in `requires` and obtains a client with `ctx.service::<Service>()`.

Dependencies tell the runtime which component must stop first. When a provider stops as a whole, it
first withdraws its service so that new activations stop selecting it. Components already using it finish cleanup before the provider can
release underlying resources. Being a child does not grant access to undeclared services.

An activation fixes its selected providers in a dependency snapshot called the **CommittedView**. Calls
made during cleanup still reach the old provider, so an old resource is not handed to a replacement
provider for shutdown. Equal-looking service values do not imply equal provider identity; dependency
changes are determined by provider identity.

Existing clients do not follow the latest registration automatically. A restarted component obtains new
clients. Manual withdrawal waits only for consumers actually using that Binding at withdrawal time and
for their activations at that moment. Components merely watching the same slot are notified, not waited
on. Once a captured activation ends, subsequent activations do not extend the wait, nor does removal
of every descendant. This differs from public `wait_settled`. If the same provider returns, the original
activation may instead settle again and keep its old client. See [Defining and calling services](../api/service-contracts.md).

## Dependency changes take effect at step boundaries

A running startup step or cleanup future is not forcibly cancelled. When dependencies or configuration
change, Runtime remembers the latest target and decides whether to continue startup or clean up after
the current step completes. The specification calls this behavior **inertia**.

For example, while startup waits for a connection, a dependency may briefly disappear and return from
the same provider. If the target matches again at the next boundary, the temporary change need not
force a restart. Multi-step activation gives complex acquisitions additional check boundaries.

After a stop request, an already started activation or effect step can still register cleanup for newly
acquired resources through `on_cleanup`, while the same Episode remains installed and unload cleanup
has not started. Expired Episodes remain rejected, and this does not permit new external work.

For the same reason, `shutdown` waits for started steps and cleanup to complete. If they never finish,
shutdown keeps waiting. Resource-owning code defines stop signals and any timeouts its business logic
needs.

## Realms select service namespaces

One Runtime can host several implementations of the same service. A `Realm` separates service
namespaces: the service type plus Realm identifies a registration location, called a BindingSlot in the
specification. An actual registration is a Binding.

```rust
let root = runtime.root();
let realm = root.create_realm();
let local = root.isolate::<Database>(realm)?;

local.mount(database(), database_config).await?;
local.mount(worker(), worker_config).await?;
```

These components provide and resolve `Database` within the selected Realm. Other services keep their
original namespaces. Apply `isolate` separately for each service that needs isolation. The Realm must
belong to the same Runtime.

`isolate` derives a context without mutating the original or switching existing clients to another
provider. It isolates in-process service namespaces. The host environment supplies process, filesystem,
network, or tenant permission isolation.

## Intercept attaches information to calls

A contract can define call metadata such as log tags, trace identifiers, or application policies.
`intercept::<Service>` derives a context carrying additional data, and the provider reads the merged
value from `CallContext.intercept`.

The service defines merge rules. Linorun does not automatically interpret fields as timeouts or
permission enforcement. Contracts without metadata needs can use the default `()`. Intercept does not
change the selected service namespace.

## Observe state and shut down

`runtime.snapshot().await` returns a consistent view of components, dependencies, and resources. Optional
event recording traces startup, withdrawal, and cleanup order. Component tests can use
[deterministic testing](deterministic-testing.md) to advance async work without sleeps.

Stop one component with `fiber.dispose().await`; stop all components and host resources with
`runtime.shutdown().await`. Inspect `cleanup_errors` in the returned report before exiting the
application. Handles can be cloned and dropped, but dropping one does not perform async cleanup.

For custom adapters or state-machine review, continue with [Core API](../api/core-api.md),
[Data model](../architecture/data-model.md), and [Runtime specification](../reference/runtime-specification.md). These advanced
references retain the full internal terminology and transitions. Component authors can start with the
task-oriented entry points above.
