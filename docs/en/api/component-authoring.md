# Writing components

[简体中文](../../zh/api/component-authoring.md) | [English](component-authoring.md)

A component starts a feature and tells Linorun how to release its resources when it stops. An async
function is usually enough:

```rust
use std::sync::Arc;
use linorun_core::{component, ActivationFailure, ComponentContext};

struct WorkerConfig {
    name: String,
}

#[component(id = "app/worker@1")]
async fn worker(
    ctx: ComponentContext,
    config: Arc<WorkerConfig>,
) -> Result<(), ActivationFailure> {
    let name = config.name.clone();
    println!("start {name}");

    ctx.on_cleanup("stop worker", move || async move {
        println!("stop {name}");
        Ok(())
    }).await?;

    Ok(())
}
```

`worker()` creates a component definition; `runtime.mount(worker(), config).await` mounts an instance.
Returning `Ok(())` means startup has completed. The component remains active, and its registered cleanup
runs when it stops. Omit the second parameter when no configuration is needed, and pass `()` to mount.

See [Getting started](../guides/getting-started.md) for a complete host program. This page covers everyday
usage first, then the explicit `Component`, multi-step activation, and `Context`/`Scope` interfaces.

## Register cleanup before continuing startup

Once `ctx.on_cleanup(...).await?` returns, the cleanup has been recorded. Consider these registrations:

```rust
ctx.on_cleanup("close database", move || async move {
    database.close().await?;
    Ok(())
}).await?;

ctx.on_cleanup("stop worker", move || async move {
    worker.stop().await?;
    Ok(())
}).await?;
```

Shutdown first waits for `stop worker`, then runs `close database`. Here, `database` and `worker` are
application resources whose close methods return errors convertible to `CleanupError`.

Within one component instance, Linorun runs registered cleanup serially in reverse registration order,
with each operation invoked at most once. A failed cleanup is recorded, and earlier resources are still
cleaned up. There is no single global cleanup order across all components; declared service dependencies
and parent/child ownership establish the ordering they need.

Register cleanup immediately after acquiring each resource. If later startup code fails, Linorun releases
the resources already registered. If acquisition fails or panics before cleanup is registered, the
runtime cannot infer how to release that resource.

If `dispose` or `shutdown` is requested during an already started activation or effect step, that step
can still finish handing back its resources. `on_cleanup` can register their cleanup while the same
Episode remains installed and unload cleanup has not started. Registration then follows the normal
cleanup order. This permits completion of existing work, not new effects, services, or child components;
registrations from an expired Episode remain rejected.

Background tasks also need registered cleanup: signal them to stop, then wait for them to exit. `Drop`
releases ordinary Rust values; asynchronous shutdown belongs in the cleanup callback. Dropping a
component handle does not stop the component.

## Call a service provided by another component

Declare the dependency in the attribute, then obtain its client from the context:

```rust
#[component(id = "app/reporter@1", requires = [Logger])]
async fn reporter(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    let logger = ctx.service::<Logger>()?;
    logger.info("reporter started".to_owned()).await
        .map_err(ActivationFailure::user)?;

    ctx.on_cleanup("reporter stopped", move || async move {
        logger.info("reporter stopped".to_owned()).await
            .map_err(linorun_core::CleanupError::user)?;
        Ok(())
    }).await?;
    Ok(())
}
```

The `Logger` contract is defined in [Defining and calling services](service-contracts.md). If a required
service is missing, the component waits without entering its startup function. Startup begins when all
required services are available. Accessing an undeclared service returns `UndeclaredDependency`. Child
components must declare their own dependencies too.

When a provider stops as a whole, Linorun first stops its consumers. During cleanup, an existing
client or `ctx.service()` still accesses the provider selected for this activation; it does not switch
to a replacement. Once a new provider is available, the component can restart and obtain a new client.

## Provide a service to other components

Declare the contract in `provides`, then register an implementation:

```rust
#[component(id = "app/logging@1", provides = [Logger])]
async fn logging(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    ctx.provide::<Logger>(Arc::new(ConsoleLogger)).await?;
    Ok(())
}
```

If the implementation owns a connection pool, listening socket, or background task, establish it in this
order:

1. Acquire the underlying resource and wait for its cleanup registration to complete.
2. Publish the service with `ctx.provide::<Service>(provider).await?`.

Reverse cleanup then withdraws the service, waits for consumers to finish cleanup, and closes the
underlying resource. `provide` manages registration and references; the application defines how to close
its pool or stop its task asynchronously.

Providing a service absent from `provides` returns `UndeclaredProvide`. An occupied slot in the same
service namespace returns `DuplicateBinding`. Hosts can also register services directly with
`runtime.provide::<Service>(provider).await`.

Manually withdrawing one service through its provide handle waits only for consumers actually holding
that Binding at withdrawal time, and for the activations they held then. Once a captured activation
ends, a later activation does not extend the wait. Components merely watching the same service
namespace are notified but are not waited on. If the same provider returns, a consumer may also settle
within its original activation and retain its old client. See
[Service registration and resource shutdown](service-contracts.md#service-registration-and-resource-shutdown).

## Mount child components

```rust
ctx.mount(worker(), WorkerConfig {
    name: "child".to_owned(),
}).await?;
```

The parent owns the child's lifecycle, so the child also stops when the parent exits. The returned
`FiberHandle` can inspect state, update configuration, or stop the child early.

To select a separate service namespace for the child, derive a context first:

```rust
let local = ctx.isolate::<Database>(realm)?;
local.mount(tenant_worker(), tenant_config).await?;
```

Create `realm` through `root().create_realm()` on the same Runtime. This affects registrations and child
routes through the derived context; it does not replace dependencies the current component already
holds. See [Concepts](../guides/concepts.md).

## Update configuration, restart, and stop

The mount handle continues to identify the same component instance:

```rust
let fiber = runtime.mount(worker(), WorkerConfig {
    name: "first".to_owned(),
}).await?;

fiber.wait_settled().await;
fiber.update_config(WorkerConfig { name: "second".to_owned() }).await?;
fiber.wait_settled().await;

fiber.restart().await?;
fiber.wait_settled().await;

let report = fiber.dispose().await;
```

`update_config` and `restart` submit changes; use `wait_settled` to wait for startup or cleanup to finish.
A configuration update cleans up the previous activation before starting with the new configuration.
It does not mutate the `Arc<Config>` held by an executing async function. Configuration and component
definitions must have immutable snapshot semantics. Mutable business state belongs in managed resources
or service implementations.

`fiber.replace(component)` replaces an implementation while retaining the same static descriptor and
configuration type. Use the explicit `Component` interface below when different implementations need to
share a descriptor.

`dispose` permanently stops the instance and returns a cleanup report. Before the host exits, explicitly
await `runtime.shutdown()`. Started activation steps and cleanup futures run to completion without forced
cancellation, so their implementations need defined completion conditions.

## Independently dispose resources or acquire them in steps

Use `ctx.effect` when a resource needs an independent handle or acquisition produces separately
reversible steps:

```rust
use linorun_core::effect;

let handle = ctx.effect("worker", effect::once(|| async {
    println!("start worker");
    Ok(Some(effect::inverse(|| async {
        println!("stop worker");
        Ok(())
    })))
})).await?;

handle.wait_forward().await?;
```

`effect(...).await` submits the work to Runtime; acquisition may still be running. `wait_forward` waits
for acquisition to finish, returning `Completed` or `Disposed` if disposal ended it. Receiving a handle
does not imply the resource is ready. `on_cleanup(...).await` already waits for registration, so no
additional `wait_forward` is needed for that operation.

`effect::sync` accepts synchronous acquisition, `effect::once` accepts one async acquisition, and
`effect::iter` and `effect::steps` accept a synchronous iterator and an asynchronous stream, respectively,
with each item yielding a cleanup operation. Use `wait_forward` between effects whose order matters.
Concurrent effects are recorded in completion order.

`handle.dispose().await` disposes an effect early. Inside cleanup, do not await another ordinary effect's
`dispose()` within the same component; let the runtime schedule reverse cleanup. Cleanup also cannot
register new managed effects, services, or child components. Temporary work must finish before the
cleanup future returns.

## Implement Component explicitly

Implement `Component` directly when the definition needs fields, a shared descriptor, or an activation
stream. `Activation::run` handles an ordinary async startup function:

```rust
use std::sync::Arc;
use linorun_core::{
    component_descriptor, Activation, Component, ComponentContext, ComponentDescriptor,
};

component_descriptor! {
    static WORKER: () {
        id: "app/custom-worker@1",
        requires: [],
        provides: [],
    }
}

struct Worker;

impl Component for Worker {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &WORKER
    }

    fn activate(&self, ctx: ComponentContext, _: Arc<()>) -> Activation {
        Activation::run(async move {
            ctx.on_cleanup("stop worker", || async { Ok(()) }).await?;
            Ok(())
        })
    }
}
```

`Activation::once` accepts a future returning `Result<Option<InverseFn>, ActivationFailure>`.
`Activation::steps` accepts a stream yielding `Result<InverseFn, ActivationFailure>`. Each yielded cleanup
operation gives the runtime a boundary to check for dependency or configuration changes. This is useful
for complex acquisitions that need recovery after individual steps. A custom resource driver can
implement `EffectDriver` and use `IntoEffectDriver` to pass it to `ctx.effect`. The protocol and adapter
return types are documented in [Core API](core-api.md).

Both component declaration forms support `Service => value` to attach metadata to a dependency:

```rust
component_descriptor! {
    static REPORTER: () {
        id: "app/custom-reporter@1",
        requires: [Logger => LoggerIntercept::default()],
        provides: [],
    }
}
```

The attribute form is `#[component(id = "app/reporter@1", requires = [Logger => value])]`. Duplicate
declarations and the reserved Root ID are definition errors, reported when the static descriptor is
initialized.

## Use Context and Scope explicitly

`ctx.context()` provides dependency access and derived routes. `ctx.scope()` provides resource
registration and mounting. Use these interfaces directly when their responsibilities need to be passed
separately:

```rust
let context = ctx.context().isolate::<Database>(realm)?;
let scope = ctx.scope();
let child = scope.mount(&context, tenant_worker(), tenant_config).await?;
```

`scope.provide::<Service>(&context, provider)` registers a service on the specified route;
`scope.effect(label, adapter)` registers a resource in the current component. Context and Scope must
belong to the same Runtime, component instance, and activation. They use the same lifecycle rules as the
convenience methods on `ComponentContext`, with the same resource ownership.

## Handle errors

Returning `ActivationFailure` from startup cleans up already registered resources and eventually puts
the instance in `Failed`. Other independent components continue running. Linorun API errors can be
converted with `?`; use `ActivationFailure::user` to add context for business errors.

Returning `CleanupError` records an error in the `dispose` or `shutdown` report. The runtime continues
releasing other resources without automatic retries. Do not manually repeat cleanup already handed to
the runtime before returning an error. Irreversible actions such as sending messages need application
compensation or idempotency rules.

`Drop` for components, configuration, providers, and futures must be nonblocking and must not panic.
Service clients and managed resources should finish their work before the owning activation ends. See
[Runtime specification](../reference/runtime-specification.md) for the complete state machine and ownership constraints.
