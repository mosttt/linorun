# Define and use services

[English](typed-service.md) | [简体中文](../../zh/tutorials/typed-service.md)

A service is an agreed calling interface between components. A business component can depend on
logging without knowing whether the implementation writes to a console or file. Once a dependency
is declared, Linorun waits for availability and coordinates restarts when its provider changes.

## 1. Define the methods

```rust
use linorun_core::{service_contract, service_impl, CallContext};

service_contract! {
    contract Logger {
        id: "app/logger@1",
        methods: [
            /// Writes one log entry.
            async fn info(message: String) -> Result<(), String>;
        ],
    }
}
```

Logger identifies the service, LoggerProvider is its implementation interface, and LoggerClient
is its calling type. Normally, use Logger in declarations and ctx.service::<Logger>(); there is no
need to construct a Client yourself. If no call metadata is needed, omit intercept, identity, and
merge. Their default metadata type is ().

## 2. Implement the service

```rust
struct ConsoleLogger;

#[service_impl]
impl LoggerProvider for ConsoleLogger {
    async fn info(&self, _call: CallContext<()>, message: String) -> Result<(), String> {
        println!("{message}");
        Ok(())
    }
}
```

The implementation receives one extra CallContext argument containing caller, provider, and call
metadata. This example does not use it, so it is named _call. #[service_impl] handles Future boxing;
you write ordinary async methods. Parameters use owned types such as String. See the
[service contract reference](../api/service-contracts.md) for the supported syntax.

## 3. Declare and use it in a component

```rust
use linorun_core::{component, ActivationFailure, ComponentContext};

#[component(id = "app/worker@1", requires = [Logger])]
async fn worker(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    let logger = ctx.service::<Logger>()?;
    logger.info("Worker started".to_owned())
        .await.map_err(ActivationFailure::user)?;
    Ok(())
}
```

The requires declaration makes the component wait for Logger and obtain the correct client.
Lookup without a declaration returns UndeclaredDependency; a parent providing that service does
not remove the need to declare it. The component maps the service's String error to ActivationFailure;
Core operation errors can use `?` directly.

## 4. Install and run

Combine the three snippets above with this main function, using the dependencies from the
[getting started guide](../guides/getting-started.md):

```rust
use std::sync::Arc;
use linorun_core::{Runtime, FiberState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::new(|task| { tokio::spawn(task); });
    let worker = runtime.mount(worker(), ()).await?;
    assert_eq!(worker.wait_settled().await, FiberState::Pending);
    runtime.provide::<Logger>(Arc::new(ConsoleLogger)).await?;
    runtime.wait_quiescent().await;
    assert_eq!(worker.state().await, FiberState::Active);
    assert!(runtime.shutdown().await.cleanup_errors.is_empty());
    Ok(())
}
```

The consumer starts Pending, then starts automatically after Logger is provided. provide returns
an EffectHandle for manual withdrawal and also withdraws the service automatically on shutdown.

A component can provide a service by declaring provides:

```rust
#[component(id = "app/logging@1", provides = [Logger])]
async fn logging(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    ctx.provide::<Logger>(Arc::new(ConsoleLogger)).await?;
    Ok(())
}
```

For workers, connections, or files that require asynchronous shutdown, register on_cleanup before
publishing the service. Service registration manages visibility and references; it cannot infer
how to shut down the underlying resource.

## 5. What happens when a service stops

A leaving provider first stops participating in new dependency resolution. Its consumers then
stop, but their cleanup functions can still use the original client for final calls. The provider
closes its underlying resources only after its consumers finish cleanup.

Each activation uses a fixed client; it does not silently switch implementations during a call.
A new provider is selected in a later activation. Service value changes alone do not restart
consumers; the runtime tracks the identity of the component instance providing the service.

Run the complete background-service example:

```sh
cargo run -p linorun-core --example application
```

## Advanced: call metadata and isolation

For example, attach log tags by defining the full interception algebra:

```rust
#[derive(Clone, Default)]
struct LogIntercept(Vec<&'static str>);

fn merge_log(base: &LogIntercept, overlay: &LogIntercept) -> LogIntercept {
    LogIntercept(base.0.iter().chain(&overlay.0).copied().collect())
}

service_contract! {
    contract TaggedLogger {
        id: "app/tagged-logger@1",
        intercept: LogIntercept,
        identity: LogIntercept::default(),
        merge: merge_log,
        methods: [
            async fn info(message: String) -> Result<(), String>;
        ],
    }
}
```

identity/merge must be nonblocking, non-panicking pure value operations with left/right identity
and associativity. Metadata merges in component-declared, outer Context, then inner Context order.
It neither changes providers nor triggers restarts. A declaration can use
`requires = [TaggedLogger => LogIntercept(vec!["worker"])]`.

Hosts can create separate service namespaces:

```rust
let root = runtime.root();
let tenant = root.create_realm();
let tenant_root = root
    .isolate::<TaggedLogger>(tenant)?
    .intercept::<TaggedLogger>(LogIntercept(vec!["tenant"]));
```

A service has at most one registered provider in each Realm. Realms partition service resolution.
Deriving isolate on the current component does not change dependencies already chosen for this
activation; it affects later mount/provide operations. default_realm explicitly restores default
routing. New clients created from a derived context receive its new intercept metadata.

The complete example with a hand-written Component, Provider trait, and call tags remains available:

```sh
cargo run -p linorun-macros --example typed_service
```

See [typed_service.rs](../../../crates/linorun-macros/examples/typed_service.rs).
Hand-written ServiceContract, ServiceRef, erased adapters, Scope/Context, and component_descriptor!
remain available for advanced control. See [service contracts](../api/service-contracts.md) and
[service semantics](../semantics/services-and-coeffects.md) for type boundaries and manual withdrawal
completion conditions.
