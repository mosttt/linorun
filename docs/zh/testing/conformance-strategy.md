# 一致性测试策略

简体中文 | [English](../../en/testing/conformance-strategy.md)

## 1. 验收目标

测试证明的是状态机语义，而不是代码行覆盖率。Core 0.1 的验收对象为：

- 运行时规范 的 O-Insert/O-Retire/O-Remove 与 L-Begin 至 L-Unload；
- 七条核心不变量；
- 资源清理、迭代、inertia、服务解析与隔离行为；
- CommittedView、withdrawal guard 和失败状态转换；
- Rust 公共 API 的类型边界、Send/Sync、panic 转换与宏展开。

所有竞态场景使用可控 Future/Stream 或 reducer 输入，不使用 sleep。

## 2. 测试层次

### 2.1 纯状态单元测试

`lifecycle.rs`、`topology.rs`、`route.rs` 和 effect adapter 的测试直接验证：

- Begin/Iter/Finish/Divert/Raise/Leave/Unload；
- revision 与 pending failure outcome；
- Target 只比较 ProviderId；
- CommittedView 固定 endpoint/binding；
- Slot replacement 的 BindingId 匹配删除；
- isolate 最近层优先与 intercept fold 顺序；
- Once/Iter/Steps 的 `has_next` boundary。

这些测试不启动 Runtime Actor。

### 2.2 Actor/Core 单元测试

`runtime/tests.rs` 内部测试可以观察 crate-private record，覆盖：

- EffectRecord 与 Episode accumulator 的共同 seq；
- Core-owned mount/provide action；
- cleanup scheduler 的 Running ownership；
- report collector；
- snapshot projection；
- completion token 的 stale 检查；
- shutdown Root episode。

用户 forward、activation 和 inverse 始终在 task 中 poll；测试不会从 Actor 内直接调用它们。

### 2.3 确定性公共 API 测试

`linorun-testkit::DeterministicExecutor` 实现同一个生产 `TaskSpawner` 接口。它以 safe `ArcWake` queue 手动 poll task，因此测试可以精确选择：

- Actor 先处理 dispose，还是先接受 completion；
- activation/effect step 何时落地；
- inverse 何时完成；
- Binding replacement 在哪个 boundary 发生；
- snapshot 位于 transition 前还是后。

`Scenario::run` 同时推进外层 API Future 和 Runtime task。没有 runnable task时返回 `DriveError`，表示测试已到达一个真实 pending boundary。

### 2.4 下游宏 fixture

`linorun-macros/tests/generated_api.rs` 与 `authoring.rs` 作为独立下游 crate 编译并运行生成代码，验证：

- `component_descriptor!` 产生唯一静态 descriptor；
- `service_contract!` 产生 contract-local Client；
- Provider trait object、erased adapter 和 typed method 连通；
- `Context::service::<C>()` 的返回类型为 `C::Client`；
- declared/route intercept 实际传入 endpoint；
- identity 与 merge 结合律的代表值；
- `#[component]` 与 `#[service_impl]` 生成代码沿同一公共 API 完成 setup、调用和 cleanup；
- 从 Core 重导出使用宏时的下游类型边界。

生成代码没有为外部 `ServiceRef<C>` 添加 inherent impl。

### 2.5 可运行示例与 rustdoc

全目标测试会编译：

- `linorun-core/examples/reversible_component.rs`；
- `linorun-core/examples/hello.rs`；
- `linorun-core/examples/application.rs`；
- `linorun-macros/examples/typed_service.rs`；
- 所有 integration tests。

rustdoc 使用 `-D warnings` 构建，保证文档内 Rust 路径和 intra-doc link 有效。

### 2.6 便捷组件入口

`authoring_context.rs` 通过公共 API 验证：连续 cleanup 登记的串行 LIFO、提前 dispose 至多一次、
派生 Context 的服务路由与资源归属、Consumer cleanup 使用旧 endpoint，以及登记与 shutdown
竞争时的惯性边界。它使用确定性 executor，不为便捷入口引入另一套运行时模拟。

## 3. linorun-testkit

### DeterministicExecutor

每个 spawned task 由一个 Arc task cell 独占 Future。waker 只把该 cell 重新排入 runnable queue；`run_one` 一次只 poll 一个 task。Future Ready 后从 cell 移除且 active count 减一。

这直接检查 driver continuation ownership：Actor 和 task 不会同时持有同一个 driver，下一 iteration 只能在上一个 completion 被 Actor 接受后 spawn。

### controlled 与 step_stream

- `controlled<T>()` 返回单次 `Control<T>` 与 `Controlled<T>` Future；
- `step_stream<T>()` 返回可 push/close 的 `StepSender<T>` 与 Stream。

Control 可以 Clone，但只有一次 `complete` 成功，适合模拟 completion race。

### TraceRecorder

RuntimeBuilder 的标准库 event sender 接到 `TraceRecorder`。`drain` 保留 Actor sequence；`event_tag` 在不丢失原始 payload 的前提下提供粗粒度标签。

### Snapshot checker

`check_snapshot` 在每个观测点检查：

- parent/children 双向一致；
- synthetic Root 与 live child mount effect ownership；
- BindingId 唯一性、visibility、owner effect 与 committed consumer index；
- `relied_by` 与 CommittedView Provider edge 双向一致；
- Episode/Effect identity，以及 Effect inverse 投影与 Episode accumulator 的完整一致性；
- Fiber 内 inverse seq 唯一；
- Fiber 内 Running inverse 至多一个。

`snapshot_validation.rs` 使用真实 Runtime 快照，篡改重复 BindingId、缺失/重复/错序的 inverse
投影，并检查 Armed、Running 和完成后的快照；它验证结构检查能力，不证明整个执行历史。

`normalize_snapshot` 提供粗粒度结构比较：保留 Fiber 状态、Provider/parent 关系、Binding身份和
Effect inverse seq，但省略 RuntimeState、EpisodeId、revision、step identity、Effect阶段、Realm
和错误文本等字段。需要检查这些语义时，必须直接断言完整 RuntimeSnapshot；normalized 相等
不代表完整运行时状态相等。

### ReferenceModel

`ReferenceModel` 独立存储 ModelFiber、ModelBinding 和 ModelEffect。`ModelCommand` 显式表示 lifecycle、binding、consumer edge、effect、yield 和 inverse start/finish。

模型拒绝非法 phase、重复 Fiber-local seq/identity、并行 inverse、非最大 requested seq，以及
仍 installed 或仍有 child/Binding/Effect 的 O-Remove。FinishUnload 要求 running inverse、
live Effect、Binding 和该 Fiber 的 consumer 关系已显式清除；失败命令保持原 record。它不持有 Runtime 容器或
executor，也不复用生产 record。

边界：当前测试由场景代码显式向模型提交 `ModelCommand`；它不是从 Runtime event 自动回放的
全量差分模型。它证明独立 transition oracle 的规则和非法转换拒绝，Runtime 与规范的一致性还
必须结合下列 snapshot checker、事件顺序断言和场景目录判断，不能仅由该模型测试推出。
模型不保存 Target/revision/StepId，也不区分已撤销 Binding 的 CommittedView 存续：Begin 的
显式 retry 与 Finish 的 key 相等由场景保证，StartInverse 不能独立证明 withdrawal guard。
`model_boundaries.rs` 验证模型能表达的终局限制及失败命令原子性；不可据此宣称全量形式化证明。

## 4. 每步结构检查

### Descriptor 与 contract

- descriptor 是唯一 `'static` 实例；
- config TypeId 等于 `Component::Config`；
- Requirement ServiceId 唯一且顺序稳定；
- provides 无重复；
- Runtime-local ServiceId 映射的 Contract/Intercept TypeId 一致；
- contract identity/merge 为纯值运算并满足代数定律。

### Fiber 与 Episode

- parent 存在，child.parent 与 parent.children 对称；
- Root 无 parent、无 mount effect，且不移除；
- 普通未 retired child 的 mount_effect 对应 parent Episode entry；
- 一个 Fiber 最多一个 installed Episode；
- Inactive 不持有 CommittedView；
- removed Fiber 不在 Binding、watcher 或 children 中。

### Binding 与 CommittedView

- 每个 ServiceId+RealmId Slot 最多一个 Binding；
- Binding provider/owner effect 存在；
- visible Binding 的 Provider 为 Active 且未 retired；
- replacement 删除按 BindingId 匹配；
- CommittedEntry 顺序与 Requirement 一致；
- Episode 内 key、binding、endpoint 与 declared intercept 不变。

### Effect 与 inverse

- seq 在 Fiber 内唯一，并按 Actor 接受 Yield completion 的顺序分配；
- InverseFn 只有 accumulator entry 持有；
- Running inverse 每 Fiber 至多一个；
- scheduler 选择当前 requested Armed 集合最大 seq；
- Running 先落地，再选择下一条；
- Effect 完成时在 accumulator/cleanup_running 中已无 entry，record 移除与 completion 在同一 turn 发布；
- 空 effect 不产生占位 inverse。

## 5. 七条核心不变量

### I1 inverse 至多一次

测试给 inverse 绑定 counter，并在 manual dispose、Fiber unload、shutdown、panic/error 路径后断言：

~~~text
calls[inverse] <= 1
~~~

### I2 Fiber-local serial LIFO

事件断言相邻 cleanup：

~~~text
InverseStarted(max requested seq)
< InverseFinished(same seq)
< InverseStarted(next max seq)
~~~

若较低 seq 已由 manual dispose 开始，则它按 inertia 落地，随后才从剩余集合重新取最大值。

### I3 Binding 先退出解析

Provider 的 `LLeave/BindingRemoved` 发生在它的第一个 Fiber-wide `InverseStarted` 前；snapshot 不会出现非 Active Provider 的 visible Binding。

### I4 Consumer 先于 Provider

依赖链与分支测试断言所有 installed Consumer 已 L-Unload，Provider 才能开始 accumulator。链式结果为 `C cleanup < B cleanup < A cleanup`。

手动 Binding 撤回另由 `manual_withdrawal_cycles.rs` 验证：仅订阅同 Slot、没有提交该
BindingId 的 Loading Fiber 不得形成 Core inverse 等待环。`withdrawal_episodes.rs` 用两个
实际 Consumer 验证捕获对象为 `(FiberId, EpisodeId)`：一个仍在旧 Episode 时，另一个已在
替代 Provider 上进入新 Loading；前者退出后，旧撤回不等待后者的新 Episode。该文件也验证
同 Provider 重新发布时，原 Episode 和旧 CommittedView 可以保留并结束撤回等待。

这些回归依赖 CommittedView 固定、只有 Active Provider 可被新 Episode 解析、Provider
不能越过仍 installed 的 committed Consumer 开始整级清理等前提。捕获的真实依赖与
Episode 身份防止本次撤回引入无关等待边；它们不是对任意用户 Future 终止性或任意外部
等待关系的完整证明。

### I5 CommittedView 固定

same-provider rebind、provider switch 和 cleanup 调用都比较 BindingId/endpoint，证明旧 episode 不读取新 Binding store。

### I6 Target 只使用 ProviderId

- 同 Provider 内部值变化不 reload；
- 值相同但 ProviderId 不同会 divert/reload；
- same-provider BindingId replacement 在 boundary 恢复时保留旧 Episode。

### I7 单 Fiber 单 transition

受控 Loading 期间提交 Target churn、restart、dispose；snapshot 始终只有一个 Episode 和一个 running activation step。completion 只根据落地时 latest key 决策。

## 6. Trace 偏序

核心偏序为：

~~~text
Provider LLeave
< BindingRemoved
< Consumer LLeave/LDivert
< Consumer LUnloadFinished
< Provider InverseStarted
~~~

Core-owned effect：

~~~text
provide: EffectStarted < BindingInstalled < [BindingVisible] < EffectForwardCompleted
mount:   EffectStarted < FiberInserted < EffectForwardCompleted < [child LBegin]
~~~

它们没有 StepId，不发送 EffectYielded。每个 EffectId 的 `EffectDisposeRequested` 至多一次。

## 7. 场景与测试位置

| 场景组 | 主要测试位置 |
|---|---|
| E01–E16 | `linorun-core::runtime::tests`、`effects_concurrency.rs`、`conformance_edges.rs` |
| F01–F18 | `lifecycle.rs`、`dependencies_context.rs`、`lifecycle_updates.rs`、`conformance_edges.rs` |
| D01–D13 | `topology.rs`、`dependencies_context.rs`、`topology_chains.rs` |
| C01–C14 | `route.rs`、`service.rs`、`dependencies_context.rs`、macro downstream fixture |
| P01–P03 | Core runtime parent tests、`topology_chains.rs` |
| R01–R11 | Core runtime inertia tests、`effects_concurrency.rs`、`dependencies_context.rs`、snapshot tests |
| S01–S07 | `shutdown_observability.rs`、`conformance_edges.rs`、`topology_chains.rs` |
| A01–A03 | `authoring_context.rs` |

精确输入/预期见[规范场景目录](scenario-catalog.md)。

## 8. 工作区验收命令

~~~bash
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --all-targets --all-features --release --locked
cargo test --workspace --doc --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
~~~

以上命令用于完整版本验收；日常改动按影响范围选择相关测试。Debug 与 Release 都必须覆盖
运行时行为，避免 debug assertion 中的副作用掩盖发布构建回归。doctest 独立执行；示例需要
实际运行才能证明交互流程，`--all-targets` 只保证它们能编译。

## 9. GitHub Actions

[CI 工作流](../../../.github/workflows/ci.yml)在推送到 `main`、创建或更新 Pull Request 时运行，
也支持从 GitHub Actions 页面手动触发。Rust 版本及组件由 `rust-toolchain.toml` 统一指定，
Cargo 命令使用 `--locked` 固定依赖解析。

Ubuntu 检查格式、Clippy 和 API 文档；Clippy 与 rustdoc 警告视为失败。Linux、Windows 和
macOS 分别运行 Debug/Release 全工作区测试、文档测试和四个可运行示例。各平台独立报告结果，
一个平台失败不会取消其他平台。

工作流缓存 Cargo 下载与构建产物，并按任务、系统、架构、工具链及锁文件区分缓存。
同一分支的新提交会取消仍在执行的旧工作流。Actions 使用固定提交版本，工作流只需仓库读取权限。
