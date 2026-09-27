# Linorun documentation

[English](README.md) | [简体中文](../zh/README.md)

Start with a task, then consult the API, architecture, and semantics references when you need control over startup boundaries or maintain the runtime. Every document is available in English and Chinese.

## Choose a task

| Document | What you will do |
| --- | --- |
| [Getting started](guides/getting-started.md) | Set up, mount a component, wait, and shut down |
| [Resource lifecycle](tutorials/reversible-component.md) | Register cleanup, roll back failed setup, and use explicit steps |
| [Services](tutorials/typed-service.md) | Define, implement, declare, and call services |
| [Component authoring](api/component-authoring.md) | Configuration, children, Context/Scope, and advanced operations |
| [Service contracts](api/service-contracts.md) | Macro syntax, call metadata, and hand-written contracts |
| [Deterministic testing](guides/deterministic-testing.md) | Control async boundaries and inspect state and events |
| [Concepts](guides/concepts.md) | Connect practical tasks to internal terminology |

## Runnable examples

```sh
cargo run -p linorun-core --example hello
cargo run -p linorun-core --example application
cargo run -p linorun-core --example reversible_component
cargo run -p linorun-macros --example typed_service
```

The first two show ordinary async authoring and a complete application. The latter two retain explicit stepwise activation and hand-written service adapters.

## Complete API and diagnostics

- [Core API](api/core-api.md)
- [Observability: snapshots, events, and metrics](operations/observability.md)
- [Glossary](reference/glossary.md)

## Architecture and concurrency

- [Architecture overview](architecture/overview.md)
- [Data model](architecture/data-model.md)
- [Crate and module boundaries](architecture/crate-boundaries.md)
- [Runtime Actor](concurrency/runtime-actor.md)
- [Transition protocol](concurrency/transition-protocol.md)

## Runtime semantics

Runtime specification defines Core 0.1. The topic guides explain individual rules, invariants, and implementation constraints.

- [Runtime specification](reference/runtime-specification.md)
- [Fiber lifecycle](semantics/fiber-lifecycle.md)
- [Effects and cleanup](semantics/effects.md)
- [Services and coeffects](semantics/services-and-coeffects.md)
- [Context, isolation, and interception](semantics/context-isolation-interception.md)
- [Failure and shutdown boundaries](semantics/failure-shutdown-boundary.md)
- [Seven invariants](reference/invariants.md)

## Maintenance and testing

- [Conformance strategy](testing/conformance-strategy.md)
- [Scenario catalog](testing/scenario-catalog.md)
