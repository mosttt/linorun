# Crate 与模块边界

简体中文 | [English](../../en/architecture/crate-boundaries.md)

## 1. Workspace

Workspace 包含三个 crate：

~~~text
linorun-core
linorun-macros
linorun-testkit
~~~

`linorun-core` 正常依赖 `linorun-macros` 并重导出全部四个宏，应用只需依赖 Core。宏 crate 的生成代码引用 Core 公共 adapter plumbing；宏 crate 自身仅在 dev-dependencies 中依赖 Core，用于下游测试与示例。`linorun-testkit` 只使用 Core 公共 handle、snapshot、event 和 `TaskSpawner` 接口，Core 不依赖 testkit。

## 2. linorun-core

当前模块布局：

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

定义：

- `Component` 与 erased adapter；
- `ComponentDescriptor`、`Requirement`；
- `Activation::Once/Steps`，以及 `run/once/steps` 构造方法；
- Config snapshot 的类型擦除边界。

`Component::activate` 不在 Runtime Actor turn 中调用；Actor 只建立 Init driver，实际调用发生在 `RuntimeTask` 内。

### context.rs 与 route.rs

定义：

- `Context`、`Scope`、`ComponentContext`；
- committed service lookup；
- immutable route chain；
- `isolate` 与 `intercept` fold。

Context 不持有可写 RuntimeCore 引用。Scope mutation 只构造命令并等待 Actor reply。
`ComponentContext` 的直接 `service/provide/mount/effect/on_cleanup` 入口委托已有 Context/Scope；
派生 `isolate/intercept` 保留同一 Scope，资源归属不变。原始 Context/Scope API 继续公开。

### effect.rs

定义：

- `EffectDriver`、`IntoEffectDriver`、`DriverEvent`；
- `InverseFn`/`InverseFuture`；
- `effect::sync/once/iter/steps/inverse` adapters。

adapter 只保存 driver ownership；forward closure 从第一次 task poll 开始执行。

### lifecycle.rs

实现独立的 Fiber lifecycle reducer：

- `LifecycleState` 与 Episode；
- revision、retire、failure outcome；
- Begin/Iter/Finish/Divert/Raise/Leave/Unload 的边界决策。

该模块不依赖 executor，也不调用用户代码。

### service.rs 与 topology.rs

`service.rs` 定义 `ServiceContract`、`ServiceRef`、`CallContext`、erased endpoint 和 contract vtable。

`topology.rs` 定义：

- `BindingSlot`/`BindingRecord`/`BindingTable`；
- `Target`、`ActivationKey`、`CommittedView`；
- Runtime-local contract registry；
- dependency resolution。

Target 只含 Provider FiberId；endpoint 与 BindingId 固定保存在 CommittedView entry 中。

### runtime/

生产控制面按职责拆分：

- `mod.rs`：synthetic Root、Actor command、Fiber/Effect/Episode records、watcher reconcile、
  withdrawal guard、serial cleanup scheduler，以及 shutdown/snapshot/event 提交；
- `api.rs`：Runtime、Builder、Root/Handle API、RuntimeHandle 和 TaskSpawner；
- `completion.rs`：句柄与 Actor 共享的终态与结果；
- `task.rs`：用户任务执行、panic 捕获与 completion 回传；
- `tests.rs`：Core 内部运行时测试。

所有可写控制面状态只由一个 `ActorCore` 拥有。异步 task 只归还 completion，不直接修改 record。
`Runtime::new` 接受 TaskSpawner 或任务提交闭包；直接 `mount/provide/effect/on_cleanup`
委托 RootContext。`on_cleanup` 使用现有 Effect 登记 inverse，并等待 forward boundary 后返回，
专用 RegisterCleanup command 允许已开始的同 Episode step 在退出期间补交收尾 inverse；
它复用同一 Effect 状态机与清理队列。

watcher index 表达 Slot 变化通知；清理等待由实际 CommittedView 依赖决定。手动 Binding
撤回还捕获 Consumer EpisodeId，避免把等待延长到同一 Fiber 的后续 Episode。

### observability.rs、state.rs、error.rs、id.rs

- `observability.rs`：public snapshot/event DTO；
- `state.rs`：public 粗粒度状态 enum；
- `error.rs`：叶子错误、API 错误和 report；
- `id.rs`：opaque Runtime-local ID 与稳定名称。

## 3. linorun-macros

宏 crate 提供两个 function-like procedural macro 和两个 attribute macro，均由 Core 重导出：

### #[component]

为 async 函数组件生成静态 descriptor 和 Component 实现，委托 `Activation::run`。

### #[service_impl]

将普通 async Provider 方法转换成既有 object-safe Provider trait 所需的 boxed Future 签名。

### component_descriptor!

生成唯一的 `LazyLock<ComponentDescriptor>`，并通过 Core 的 doc-hidden constructor 固定 Config、Requirement 和 provides。

### service_contract!

生成：

- contract marker；
- object-safe Provider trait；
- contract-local Client newtype 和异步方法；
- 私有 erased endpoint adapter；
- `ServiceContract` implementation。

生成代码只使用 `Requirement::new`、`ComponentDescriptor::new`、`ServiceRef::dispatch_parts` 和 `ErasedEndpoint::as_any` 等公开 adapter 边界，不访问 Core 私有 record。

## 4. linorun-testkit

testkit 模块：

~~~text
src/
  lib.rs       Scenario
  executor.rs  DeterministicExecutor
  control.rs   controlled Future and StepStream
  trace.rs     TraceRecorder/EventTag
  snapshot.rs  normalize/check snapshot
  model.rs     ReferenceModel
~~~

`DeterministicExecutor` 实现生产 `TaskSpawner`。它用 safe `ArcWake` queue 手动 poll RuntimeTask；不会向 Core 注入另一条状态变更路径。

`Scenario` 把 Runtime、executor 和 event sender 连接起来。snapshot checker 与 reference model 只读取 DTO 或接受显式 ModelCommand，不获得 Core 内部可变引用，因此测试构建和生产构建共享同一控制面实现。

## 5. 公共与私有边界

公开：

- Runtime、RootContext、ComponentContext、Context、Scope；
- Component、ServiceContract 与宏 adapter constructors；
- FiberHandle、EffectHandle；
- ServiceRef/CallContext/ErasedEndpoint adapter plumbing；
- opaque IDs 和粗粒度状态；
- errors、reports、snapshot、events；
- TaskSpawner/RuntimeTask。

私有：

- Actor command 与 completion token；
- FiberRecord、EffectRecord、Episode payload；
- LifecycleState 细节；
- Binding store、contract vtable、watcher/consumer indexes；
- accumulator entry/action 与 running inverse；
- RuntimeHandle。

私有布局保证组件、宏和 testkit 都不能绕过 Actor reducer 修改状态。

## 6. 依赖方向

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

Core 运行库不绑定 Tokio；Tokio 仅是 Core 示例使用的 dev-dependency。Host 提供 TaskSpawner
或满足其契约的任务提交闭包；相同接口既可连接线程池，也可连接 testkit 的确定性 executor。

实现入口：[Core manifest](../../../crates/linorun-core/Cargo.toml)、
[宏 manifest](../../../crates/linorun-macros/Cargo.toml)、
[Runtime Actor](../../../crates/linorun-core/src/runtime/mod.rs)。
