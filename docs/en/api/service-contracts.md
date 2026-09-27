# Defining and calling services

[简体中文](../../zh/api/service-contracts.md) | [English](service-contracts.md)

Services give components explicit methods to call and tell Linorun which components must clean up first.
A complete service has a contract, an implementation, and a consumer. This logger does not need call
metadata:

```rust
use std::sync::Arc;
use linorun_core::{
    component, service_contract, service_impl, ActivationFailure, CallContext, ComponentContext,
};

service_contract! {
    pub contract Logger {
        id: "app/logger@1",
        methods: [
            async fn info(message: String) -> Result<(), String>;
        ],
    }
}

pub struct ConsoleLogger;

#[service_impl]
impl LoggerProvider for ConsoleLogger {
    async fn info(&self, _: CallContext<()>, message: String) -> Result<(), String> {
        println!("{message}");
        Ok(())
    }
}

#[component(id = "app/logging@1", provides = [Logger])]
async fn logging(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    ctx.provide::<Logger>(Arc::new(ConsoleLogger)).await?;
    Ok(())
}

#[component(id = "app/reporter@1", requires = [Logger])]
async fn reporter(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    let logger = ctx.service::<Logger>()?;
    logger.info("reporter ready".to_owned()).await
        .map_err(ActivationFailure::user)?;
    Ok(())
}
```

Mount `reporter()` and `logging()`. Either mount order works: if the logger is missing, `reporter` stays
`Pending` and starts automatically when the service appears. A returned mount handle does not mean
startup is complete; await `fiber.wait_settled()` when needed.
If a consumer was mounted before its dependency, wait for the provider to start before checking the
consumer. With a missing dependency, `wait_settled` can return `Pending`; it does not wait for a service
that may appear later.

A host can instead provide the logger with
`runtime.provide::<Logger>(Arc::new(ConsoleLogger)).await`, then mount consumers. The owner of the
implementation's lifecycle determines whether to register it from a component or from the host.

## Generated contract types

The `service_contract!` invocation above generates:

| Name | Purpose |
| --- | --- |
| `Logger` | Contract type used in `requires`, `provides`, `service`, and `provide` |
| `LoggerProvider` | Method interface implemented by a provider |
| `LoggerClient` | Cloneable client returned by `ctx.service::<Logger>()` |

The provider type for `Logger` is `Arc<dyn LoggerProvider>`. Clients call the implementation selected for
the current activation directly; ordinary service method calls do not go through the Runtime Actor
command queue.

Use `namespace/name@major` for the ID, such as `app/logger@1`. Within one Runtime, a service ID identifies
one Rust contract type. Independently defined contracts cannot substitute for one another merely by
using the same string.

Method parameters use identifier patterns and owned types such as `String`, `Vec<T>`, or `Arc<T>`.
`#[service_impl]` handles boxed futures while keeping implementations in `async fn` syntax. It supports
nongeneric trait impls whose methods take `&self`, then `CallContext<I>`, then business arguments. Use the
explicit Provider interface below when another implementation shape is required.

## Cleanup still calls the original implementation

A consumer can continue using its dependency in a cleanup callback:

```rust
let logger = ctx.service::<Logger>()?;
ctx.on_cleanup("reporter shutdown", move || async move {
    logger.info("reporter stopped".to_owned()).await
        .map_err(linorun_core::CleanupError::user)?;
    Ok(())
}).await?;
```

The provider selected for one activation stays fixed. Even if a replacement logger has been registered,
old clients still access the old implementation. Calling `service` again through the same activation's
context also retains the provider identity. The component obtains new dependencies on its next
activation. This fixed dependency snapshot is the **CommittedView**.

When a provider stops as a whole, Linorun first removes its service from new dependency resolution, then waits for existing
consumers to finish cleanup before allowing the provider to close earlier registered resources. This is
why consumers must declare the service in `requires`.

Use clients and finish their calls within their owning activation. Storing a client globally and using
it after the component has cleaned up does not create an independent lifecycle dependency, even though
the client still holds a reference.

## Service registration and resource shutdown

`ctx.provide::<Logger>(provider).await` returns an `EffectHandle`. Component shutdown automatically
withdraws the registration; the handle can also withdraw it early.

Manual withdrawal notifies every component watching the service slot, but waits only for committed
consumers actually holding that BindingId at withdrawal time, capturing their `(FiberId, EpisodeId)`
pairs. Merely declaring or watching the same slot without using this registration does not add a waiter.

Each captured consumer satisfies the wait once its captured Episode has left. A later Episode does not
extend the wait. If the original Episode remains, it must be Active with its key matching the latest
ActivationKey, a boundary reachable when the same provider returns.

A retired instance whose own cleanup is complete has released that old Episode even while children
remain and its public state is `Unloading`. This therefore differs from `FiberHandle::wait_settled()`:
it does not wait for the entire descendant tree, later consumer activations, or unrelated independent
effects on an Active component.

If the same provider returns, a consumer may retain its activation and old client while manual
withdrawal still completes. Withdrawal does not forcibly revoke all old clients. A consumer activation
failure also does not become a provider cleanup error.

Registration manages the Binding and references to the implementation. The provider acquires underlying
resources such as pools or tasks and registers their cleanup separately. Wait for resource cleanup
registration before publishing the service. Reverse cleanup then withdraws the service, waits for
consumers to exit, and closes the underlying resources.

Only one visible registration may occupy the same service and Realm. Conflicts return
`DuplicateBinding`. Use different Realms for independent implementations of one service; see
[Concepts](../guides/concepts.md).

## Attach metadata to calls

Define an `Intercept` type when a service needs log tags, tracing fields, or its own policy data. This
example merges tag lists by appending outer layers before inner layers:

```rust
use linorun_core::service_contract;

#[derive(Clone, Default)]
pub struct LogTags(pub Vec<String>);

fn merge_tags(base: &LogTags, overlay: &LogTags) -> LogTags {
    LogTags(base.0.iter().chain(&overlay.0).cloned().collect())
}

service_contract! {
    pub contract TaggedLogger {
        id: "app/tagged-logger@1",
        intercept: LogTags,
        identity: LogTags::default(),
        merge: merge_tags,
        methods: [
            async fn info(message: String) -> Result<(), String>;
        ],
    }
}
```

The provider reads metadata from the call context:

```rust
#[service_impl]
impl TaggedLoggerProvider for ConsoleLogger {
    async fn info(&self, call: CallContext<LogTags>, message: String) -> Result<(), String> {
        println!("[{}] {message}", call.intercept.0.join("/"));
        Ok(())
    }
}
```

A consumer declares the `TaggedLogger` dependency, then gets a client from a derived context:

```rust
let tagged = ctx.intercept::<TaggedLogger>(LogTags(vec!["worker".to_owned()]));
let logger = tagged.service::<TaggedLogger>()?;
logger.info("ready".to_owned()).await.map_err(ActivationFailure::user)?;
```

`intercept` does not mutate the original context or select another provider. Existing clients retain the
metadata captured when they were created. A new client from a derived context can carry new metadata
while still using the provider fixed for the current activation.

To attach metadata before a child starts, call `intercept` on the context used to mount it. A component
can also specify an initial dependency value with `requires = [TaggedLogger => LogTags(...)]`.
Merging starts with the declared value, then folds context overlays from outermost to innermost. Without
a declared value, merging starts from `identity`.

`identity` and `merge` must be nonblocking pure value operations that do not panic, obey left and right
identity and associativity, and leave their inputs unchanged. Test these laws in the contract's own
tests. Registered metadata must have immutable value semantics; external aliases must not silently
change merge results. Linorun carries the metadata; the service defines and enforces its meaning.

If metadata is unnecessary, omit `intercept`, `identity`, and `merge` together to use `()`.

## CallContext diagnostic fields

Every Provider method receives a `CallContext<I>`:

| Field | Meaning |
| --- | --- |
| `caller` | Calling component instance |
| `caller_episode` | Caller's current activation |
| `provider` | Provider instance fixed for this call |
| `binding` | Service registration used by the call |
| `intercept` | Merged call metadata |

These fields support logs, tracing, and service-specific authentication adapters. They do not change
dependency target equality. If unused, the Provider method can ignore the entire `CallContext<()>`
argument with `_`.

## Implement Provider explicitly

Provider traits use boxed futures for dynamic dispatch. The explicit implementation of the preceding
`LoggerProvider` method has this form:

```rust
use std::{future::Future, pin::Pin};

impl LoggerProvider for ConsoleLogger {
    fn info<'a>(
        &'a self,
        _: CallContext<()>,
        message: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            println!("{message}");
            Ok(())
        })
    }
}
```

This code and the `#[service_impl]` example are alternative forms of the same implementation; choose one.

Each call future retains the endpoint references it needs until completion. Await calls within consumer
work or cleanup. A contract returning a stream or starting other async work must define ownership and
completion conditions for that work. Whole-provider withdrawal waits for consumers to end the
activations holding committed dependencies; it does not independently count every in-flight RPC or
stream. Manual withdrawal of one registration uses the captured consumer Episode boundary described above.

## Custom ServiceContract and lower-level clients

For custom adapters, `ServiceContract` connects these pieces:

```rust
pub trait ServiceContract: Sized + Send + Sync + 'static {
    const ID: &'static str;
    type Provider: Send + Sync + 'static;
    type Intercept: Clone + Send + Sync + 'static;
    type Client: Clone + Send + Sync + 'static;

    fn identity_intercept() -> Self::Intercept;
    fn merge_intercept(base: &Self::Intercept, overlay: &Self::Intercept)
        -> Self::Intercept;
    fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint>;
    fn client(reference: ServiceRef<Self>) -> Self::Client;
}
```

This is the interface shape; `ErasedEndpoint` and `ServiceRef` come from `linorun_core`. Applications can
use the macro, and custom contracts remain free to implement the trait directly.

Core creates `ServiceRef<C>` with a fixed endpoint, call identity, and metadata. A contract-local Client
owns that reference and exposes concrete methods. Because the Client type belongs to the contract's
crate, downstream code does not need to add inherent methods to a foreign `linorun-core` type.

The generated endpoint wraps the provider in a private adapter and restores its type through
`dispatch_parts` and `as_any`. These are public adapter hooks hidden from routine documentation;
components use typed clients. `erase` and `client` only wrap ownership and adapt types. They must be
nonblocking, must not panic, and must not acquire external resources. A custom contract is responsible
for matching its endpoint and client.

Reusing a service ID with a different contract type, intercept type, or incompatible declared dependency
returns `ServiceTypeMismatch`. Runtime validates types before committing a mount or service
registration; failure does not commit partial topology. An already created client does not look up the
latest Binding on every call.

See [Core API](core-api.md), [Data model](../architecture/data-model.md), and
[Runtime specification](../reference/runtime-specification.md) for the complete error, routing, and ownership contracts.
