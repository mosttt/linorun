# Linorun

[English](README.md) | [简体中文](README.zh-CN.md)

Linorun is a Rust library for components that start when their services are available and clean up their resources when they stop. Changing a service provider automatically restarts its dependents in the right order.

Write an async function, declare the services it needs, and register how its resources are released. Linorun handles startup, dependency changes, rollback, and shutdown.

```rust
use linorun_core::{ActivationFailure, ComponentContext, component};

#[component(id = "app/worker@1")]
async fn worker(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    println!("Worker started");
    ctx.on_cleanup("stop worker", || async {
        println!("Worker stopped");
        Ok(())
    }).await?;
    Ok(())
}
```

Run the smallest complete example, then an async application with service updates and failure recovery:

```sh
cargo run -p linorun-core --example hello
cargo run -p linorun-core --example application
```

- [Getting started](docs/en/guides/getting-started.md): setup, a complete program, and cleanup.
- [Services](docs/en/tutorials/typed-service.md): define and call a typed service.
- [Resource lifecycle](docs/en/tutorials/reversible-component.md): partial setup, rollback, and explicit steps.
- [Documentation](docs/en/README.md): task guides and the complete advanced reference.

All macros are available from `linorun_core`. The runtime works with your async executor; Tokio is used by application examples, not required by the library.

The workspace also includes `linorun-macros` and `linorun-testkit`. Advanced APIs include explicit Component implementations, stepwise activation, custom Effect drivers, Context/Scope, service realms, call metadata, snapshots, events, and deterministic scheduling. Runtime semantics are specified in [Runtime specification](docs/en/reference/runtime-specification.md).
