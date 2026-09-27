# Getting started

[English](getting-started.md) | [简体中文](../../zh/guides/getting-started.md)

Start and stop one component first, then add services and resources as needed. You do not need to learn the internal state machine before writing an application.

## Run it first

The repository pins Rust 1.98.0 in `rust-toolchain.toml`. From the repository root, run:

```sh
cargo run -p linorun-core --example hello
```

Expected output:

```text
Component started
Component cleaned up
```

This already covers mounting, startup, waiting, and shutdown. See [hello.rs](../../../crates/linorun-core/examples/hello.rs) for the complete source.

## Write a complete program

Applications get both the runtime and macros from `linorun-core`. If your application and this repository are neighboring directories:

```toml
[dependencies]
linorun-core = { path = "../linorun/crates/linorun-core" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

Adjust the path for your checkout. Tokio is the executor used here; other executors can be connected too.

```rust
use linorun_core::{ActivationFailure, ComponentContext, FiberState, Runtime, component};

#[component(id = "example/hello@1")]
async fn hello(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    println!("Component started");
    ctx.on_cleanup("say goodbye", || async {
        println!("Component cleaned up");
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::new(|task| {
        tokio::spawn(task);
    });
    let component = runtime.mount(hello(), ()).await?;
    assert_eq!(component.wait_settled().await, FiberState::Active);
    let report = runtime.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    Ok(())
}
```

There are four things to understand:

1. `Runtime` manages the application's components. The supplied function submits tasks to your executor.
2. `#[component]` turns an async function into a component factory. `hello()` creates a component ready to mount; `id` gives it a stable name.
3. `ctx.on_cleanup` records a shutdown action before returning. The action runs when the component stops.
4. `mount` installs a component and returns its control handle. `()` means there is no configuration. `shutdown().await` waits for cleanup to finish.

## Installed does not mean started

`mount(...).await` only means the component has been installed. It may still be starting or waiting for a missing service. `wait_settled()` waits for the current startup or cleanup process to reach a stable state and returns that state:

| State | Meaning |
| --- | --- |
| `Pending` | Required services are unavailable; startup has not begun |
| `Loading` | A startup step is running |
| `Active` | Startup succeeded |
| `Unloading` | Waiting for or executing cleanup |
| `Failed` | Startup failed and registered cleanup has been attempted |
| `Disposed` | The component and its descendants have been removed |

`wait_settled` returns Pending, Active, Failed, or Disposed. Pending is not an error, and this call does not wait for a service that might arrive later. To wait for all currently actionable runtime work, use `runtime.wait_quiescent().await`.

## Closing resources

As soon as you acquire a resource that needs explicit cleanup, register its shutdown action. For a background task, the cleanup function should signal it to stop and await its completion. Actions run in reverse registration order, including when setup fails partway through.

The handle returned by `on_cleanup` can release that resource early. Place this fragment inside a component function:

```rust
let cleanup = ctx.on_cleanup("release resource", || async {
    Ok(())
}).await?;
let report = cleanup.dispose().await;
```

Early and automatic cleanup share the same path; the callback is not invoked twice. See [resource lifecycle](../tutorials/reversible-component.md) for resources, failed setup, and explicit steps.

## Updates and failures

The control handle provides `restart()`, `update_config(config)`, `replace(implementation)`, and `dispose()`. The first three use normal cleanup ordering before restarting. `replace` must still refer to the same component descriptor.

Startup failure details appear in the component entry of `runtime.snapshot().await`. A failure does not stop unrelated components. Correct the configuration or explicitly restart to retry. Check the reports returned by `shutdown` and `dispose` for cleanup errors.

## Run a real application

```sh
cargo run -p linorun-core --example application
```

[application.rs](../../../crates/linorun-core/examples/application.rs) serves requests through a real Tokio worker task. It demonstrates waiting for a service, automatic startup, configuration updates, implementation replacement, rollback after failure, recovery, and ordered shutdown. It synchronizes through messages and runtime state, without sleeps.

Choose your next topic:

- [Services](../tutorials/typed-service.md): make components call each other.
- [Component authoring](../api/component-authoring.md): configuration, children, isolation, explicit steps, and custom effects.
- [Concepts](concepts.md): connect these operations to Fiber, Episode, and CommittedView.
- [Core API](../api/core-api.md): the complete interface, including every advanced capability.
- [Deterministic testing](deterministic-testing.md): control async steps precisely and check results.
