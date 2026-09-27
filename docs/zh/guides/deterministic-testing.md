# 不用 sleep 测试组件生命周期

[简体中文](deterministic-testing.md) | [English](../../en/guides/deterministic-testing.md)

`linorun-testkit` 提供手动推进的执行器。测试决定一个异步步骤何时完成，再断言组件状态、
清理次数或服务撤回顺序，无需猜测后台任务需要等待多少毫秒。

应用依赖 `linorun-core`，测试依赖 `linorun-testkit`。使用本地源码时，将依赖路径指向
对应 crate。例如应用与 Linorun 检出目录并列时：

```toml
[dependencies]
linorun-core = { path = "../linorun/crates/linorun-core" }

[dev-dependencies]
linorun-testkit = { path = "../linorun/crates/linorun-testkit" }
```

## 先验证组件能启动和停止

`Scenario` 已经连接 Runtime、执行器和事件记录器。下面是可直接放进测试文件的例子：

```rust
use linorun_core::{component, ActivationFailure, ComponentContext, FiberState};
use linorun_testkit::Scenario;

#[component(id = "test/worker@1")]
async fn worker(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    ctx.on_cleanup("stop worker", || async { Ok(()) }).await?;
    Ok(())
}

#[test]
fn worker_starts_and_stops() {
    let scenario = Scenario::new();
    let fiber = scenario.run(scenario.runtime().mount(worker(), ()))
        .unwrap().unwrap();

    assert_eq!(scenario.run(fiber.wait_settled()).unwrap(), FiberState::Active);
    let report = scenario.run(fiber.dispose()).unwrap();
    assert!(report.cleanup_errors.is_empty());
    assert_eq!(scenario.run(fiber.state()).unwrap(), FiberState::Disposed);

    let report = scenario.run(scenario.runtime().shutdown()).unwrap();
    assert!(report.cleanup_errors.is_empty());
}
```

将 `worker()` 换成待测组件及其配置，检查应用关心的状态或资源行为即可。
`scenario.run(future)` 同时推进传入的 Future 和 Runtime 启动的任务，直到该 Future 完成。

挂载代码中的两层 `unwrap` 对应不同错误：外层是执行器的 `DriveError`，内层是 `mount`
自己的 `MountError`。当待测 Future 仍在等待、但没有任务可继续推进时，`run` 返回
`DriveError`。对故意暂停的步骤，这可以是预期结果。

## 在指定位置暂停异步工作

`controlled()` 返回一个 Future 和控制它完成的句柄。下面的测试先确认工作仍在等待，
再放行，并验证清理只执行一次：

```rust
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use linorun_core::{effect, EffectForwardOutcome};
use linorun_testkit::{controlled, Scenario};

#[test]
fn cleanup_runs_after_controlled_start() {
    let scenario = Scenario::new();
    let (release, gate) = controlled();
    let calls = Arc::new(AtomicUsize::new(0));
    let cleanup_calls = Arc::clone(&calls);

    let handle = scenario.run(scenario.runtime().effect(
        "controlled worker",
        effect::once(move || async move {
            gate.await;
            Ok(Some(effect::inverse(move || async move {
                cleanup_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })))
        }),
    )).unwrap().unwrap();

    scenario.executor().run_until_stalled();
    assert!(scenario.run(handle.wait_forward()).is_err());
    assert!(!scenario.snapshot().unwrap().quiescent);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    release.complete(()).unwrap();
    assert_eq!(
        scenario.run(handle.wait_forward()).unwrap().unwrap(),
        EffectForwardOutcome::Completed,
    );

    let report = scenario.run(handle.dispose()).unwrap();
    assert!(report.cleanup_errors.is_empty());
    scenario.run(handle.dispose()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    scenario.run(scenario.runtime().shutdown()).unwrap();
}
```

`Control::complete` 的多个克隆中只允许一个成功完成 Future。应保留控制句柄直到放行；全部
控制句柄被丢弃会使对应 Future 报 panic，提醒测试遗漏了一个边界。

`run` 接收的 Future 在返回 `DriveError` 时会被丢弃。需要保留本次等待自身的局部状态时，
先固定该 Future，再把可变引用交给执行器：

```rust
let mut waiting = Box::pin(handle.wait_forward());
assert!(scenario.run(waiting.as_mut()).is_err());
release.complete(()).unwrap();
let result = scenario.run(waiting.as_mut()).unwrap();
```

已经交给 Runtime 管理的启动任务和清理任务不会因为调用者丢弃等待 Future 而被取消。
测试停止与资源建立的交错时，也可以先让步骤等待、发起 `dispose` 或 `shutdown`，再放行
它取得资源并调用 `on_cleanup`。只要仍是同一 Episode 且卸载清理尚未开始，登记应成功，
清理应在关闭完成前执行；用已过期 Episode 登记则应被拒绝。

## 逐项推进多步启动

需要精确控制流的每一步时使用 `step_stream`：

```rust
use linorun_core::{effect, ActivationFailure};
use linorun_testkit::step_stream;

let (steps, stream) = step_stream::<Result<linorun_core::InverseFn, ActivationFailure>>();
let activation = linorun_core::Activation::steps(stream);
assert!(steps.push(Ok(effect::inverse(|| async { Ok(()) }))).is_ok());
steps.close();
```

测试组件可以在 `activate` 中返回该 `activation`。每次 `push` 提供一个启动步骤的清理
操作，`close` 表示不会再产生步骤。用于 effect stream 时，元素错误类型改为 `EffectError`，
再交给 `effect::steps`。需要更细的调度控制时可用 `run_one()` 推进一个可运行任务，或用
`run_until_stalled()` 运行到所有任务都在等待。

## 检查状态快照

`scenario.snapshot()` 返回同一个 Actor turn 内的一致 `RuntimeSnapshot`，并自动执行
`check_snapshot`，检查父子关系、资源归属和依赖索引等结构是否一致。

测试可直接检查与场景有关的字段，例如查找 `fiber.id()` 对应的记录，断言其 `state`。
`normalize_snapshot` 去除部分诊断细节，生成稳定排序的 Fiber、Binding 和 Effect 投影，
用于比较控制面结构。

深层一致性检查还包括：服务可见性与提供方资源对应、CommittedView 与反向依赖边对应、
Episode/Effect 身份与清理所有者对应、同一 Fiber 的清理序号唯一，以及最多一个正在执行
的清理操作。这些检查不能替代对业务结果的断言，应与资源日志或事件顺序一起使用。

## 检查服务撤回和清理顺序

`scenario.trace().drain()` 取得 Runtime 事件，`event_tag(&event.kind)` 将具体事件映射
为方便比较的 `EventTag`。需要检查组件或资源身份时，匹配原始 `RuntimeEventKind`。

提供方整体退出的场景通常应验证以下必要顺序：

```text
Binding removed from new resolution
    < Consumer finishes cleanup
    < Provider closes underlying resource
```

只比较当前场景要求的先后关系，不要求无关组件的事件形成一个固定全局顺序。
事件 `sequence` 在同一 Runtime 中递增，可以用于比较先后。

手动撤回一项 Binding 的场景应区分通知集合与等待集合：所有 slot watcher 都会收到通知，
但只等待撤回时 CommittedView 实际含该 BindingId 的使用方，并固定它们当时的 EpisodeId。
捕获的 Episode 已经离开就满足等待；使用方新一次启动即使还在 Loading，也不能延长旧
注册的撤回。同一 Episode 若保持 Active 且 key 匹配最新 ActivationKey，也达到边界。

自身清理完成但仍有子组件待移除的 retired 实例已经释放旧 Episode，即使公共状态仍是
`Unloading`。测试不要用公共 `wait_settled`、整棵子树 `Disposed` 或所有 watcher 都稳定
替代这项条件；它们等待的工作不同。应覆盖未实际使用该 Binding 的 watcher，以及使用方
旧 Episode 结束后新 Episode 仍在等待的场景。

已有完整场景可作为写测试的参考：

- [生命周期更新](../../../crates/linorun-testkit/tests/lifecycle_updates.rs)：重启、配置和实现替换。
- [依赖与上下文](../../../crates/linorun-testkit/tests/dependencies_context.rs)：服务声明、Realm 和调用元数据。
- [资源并发](../../../crates/linorun-testkit/tests/effects_concurrency.rs)：手动撤销、顺序与执行中的步骤。
- [拓扑链](../../../crates/linorun-testkit/tests/topology_chains.rs)：依赖链和提供方撤回。
- [关闭与观测](../../../crates/linorun-testkit/tests/shutdown_observability.rs)：关闭报告与快照。
- [规范边界](../../../crates/linorun-testkit/tests/conformance_edges.rs)：跨功能的边界场景。

## 使用独立参考模型

`ReferenceModel` 是单独实现的轻量状态机。测试通过 `ModelCommand` 显式提交挂载、开始、
步骤、失败、卸载，以及 Binding 和 Effect 操作，再读取排序后的 `ModelSnapshot`。
它能拒绝非法状态转换、重复身份、清理顺序冲突以及仍有资源时移除组件等操作。

模型命令目前由测试手动编排，不会自动把 Runtime 事件转换成命令，也不执行全量差分回放。
因此“模型通过”表示显式编排的模型操作满足约束，不能单独证明真实 Runtime 的所有执行
都正确。应结合真实场景、快照结构检查和事件顺序断言使用。

模型示例见[确定性测试](../../../crates/linorun-testkit/tests/deterministic.rs)，规范层面的
覆盖范围见[一致性测试策略](../testing/conformance-strategy.md)和
[场景目录](../testing/scenario-catalog.md)。
