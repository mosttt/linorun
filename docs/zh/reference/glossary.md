[简体中文](glossary.md) | [English](../../en/reference/glossary.md)

# 术语表

## Activation

Component 在 Reloading 中执行的 forward computation。它可以是单个 Future，也可以是逐步产生 inverse 的 stream。

## ActivationKey

Target 与 activation revision 的组合。Fiber 用 committed key 和 latest key 的比较决定继续、Leave 或 Divert。

## Active

Fiber activation 完成且 committed key 仍等于 latest key 的状态。只有 Active Provider 的已注册 Binding 对新 Consumer 可见。

## Actor

串行拥有 RuntimeCore 的控制面任务。它应用 reducer、维护 watcher，并在状态提交后调度用户 Future。

## Binding

一次具体 service provision。Binding 连接 BindingSlot、Provider Fiber、endpoint 和 owner Effect。

## BindingId

Runtime 内标识具体 BindingRecord 的身份。CommittedView 保存它以标识本 Episode 捕获的 provision。

## BindingSlot

ServiceId 与 RealmId 的二元组，是 service 注册和解析的唯一位置。

## CallContext

宏生成的 service adapter 在一次调用中收到的 committed 诊断数据：caller Fiber/Episode、
Provider、Binding 和 effective intercept。它不参与 Target equality。

## boundary

一个 activation/effect step 完成并把结果交回 Actor 的决策点。Runtime 只在 boundary 比较
最新 ActivationKey、停止 continuation 或开始 cleanup。

## Component

可实例化定义，包含 descriptor、Config 类型和 activation 实现。

## ComponentDescriptor

Component 的进程内唯一静态接口：ComponentId、Config TypeId、Requirements 和 provides。
实现替换必须复用同一 descriptor 实例。

## ComponentId

稳定的组件定义名称；不标识运行中的 Fiber 实例。公共 `as_str` 返回规范名称。

## CommittedEntry

CommittedView 中一个 Requirement 的固定解析，包含 BindingId、slot、
Provider FiberId、endpoint 和 metadata。

## CommittedView

L-Begin 时捕获，并在整个 Episode 中固定的依赖视图。

## Consumer

CommittedView 引用某个 Binding 的 Fiber。

## Context

Component 对已声明 committed services、Realm route 和 interception metadata 的不可变视图。
未声明 service 不沿 parent Fiber 隐式查找。

## ContextRoute

由 isolate_delta 和 intercept_delta 构成的 immutable parent chain。

## ContractVTable

Requirement 携带的 erased ServiceContract 描述：contract/intercept TypeId，以及 metadata 的
identity/merge adapter。Runtime 为每个 ServiceId 固定一个 vtable 映射。

## Disposed

Fiber retire、完成 cleanup、children 清空并从 registry 移除后的公共终局。

## Effect

一次 forward 变化以及它产生的一个或多个 inverse。

EffectRecord 跟踪 Scope effect 的 driver 和 completion；InverseAction 由 Episode
accumulator 唯一持有，User action 内部唯一持有 InverseFn。

## EffectHandle

请求手动 dispose 某个 EffectRecord 并等待结果的 Handle。

## EffectId

Runtime 内标识一次 effect 调用的身份。

## StepId

一次 activation 或 effect forward step completion 的 Runtime-local identity。Inverse completion
由 `(FiberId, EpisodeId, seq)` 组合身份标识。

## Effect sequence

Fiber 内 inverse 的 application order。cleanup 在当前请求覆盖的 accumulator entries 中选择最大 seq，实现 LIFO。

## Episode

一次 Reloading—Active—Unloading 生命周期实例，持有 EpisodeId、ActivationKey、CommittedView 和 accumulator。

## EpisodeId

Runtime 内标识 Episode 的身份，也用于关联异步 completion。

## Episode accumulator

按 Fiber application seq 保存一个 Episode 已产生的全部 InverseEntry；它是 inverse 的唯一
所有者，cleanup 选择请求集合中的最大 seq 并串行执行。

## Failed

`Inactive { failure: Some(ActivationFailure) }` 时的公共投影。Failed 是 settled/quiescent
终态，不会自动重试。

## Fiber

Component 的运行实例，拥有独立生命周期、effects、Binding 和 child set。

## FiberId

Runtime 内标识 Fiber 的身份，也是 Target 中的 Provider identity。

## Inactive

内部静止状态。未 retired 时，无 failure 对应 Pending，有 failure 对应 Failed；retired
但仍等待 children 移除时，公共状态仍为 Unloading。

## Inertia

已经开始的异步 step 必须落地；Target 变化只在 step boundary 处理。

step task 独占 driver；每次落地把事件/error 和 driver 一起移交 Actor。只有
`Yield { has_next: true }` 的返回 driver 可由 Actor 放入下一 RuntimeTask。
Actor 提交 StepId 和这次 ownership transfer 时，step 即已开始；首次 executor poll
不是额外的取消边界。

`Activation::run` 的整个 async body 是一个 step，`on_cleanup` 不切分它的 boundary。
同一 Episode 的 in-flight step 在 shutdown 或 retire 后、unload cleanup 开始前仍可通过
专用 `RegisterCleanup` 登记已取得资源的 inverse；新工作与过期 Scope 仍被拒绝。

## installed

处于 Reloading、Active 或 Unloading 的 Fiber。Installed Fiber 持有 CommittedView。

## intercept

在派生 Context 上附加 service-specific metadata。它影响调用，不改变 Target。

## InterceptValue

Core 内部的 ServiceContract metadata erased 表示，同时携带 contract TypeId、intercept
TypeId 和值。裸 `Arc<dyn Any>` 不是合法 metadata 通道。

## Inverse

Effect forward 成功后提供的恢复操作，由 Runtime 至多调用一次。

## InverseAction

Episode accumulator 唯一持有的撤销动作：用户 FnOnce、RetireFiber 或 WithdrawBinding。
Core-owned action 与用户 inverse 共享 seq/LIFO 选择，但由 Actor reducer 执行。

## isolate

在派生 Context 上为 ServiceId 选择 RealmId，从而改变 BindingSlot resolution。
它影响后续 mount/provide；不会重选当前 Fiber 已经提交的 CommittedView endpoint。

## L-Begin

Inactive Fiber 捕获 CommittedView 并进入 Reloading。

## L-Divert

Reloading Fiber 在 step boundary 发现 latest key 不同，转入 Unloading。

## L-Finish

Reloading Fiber 完成 activation 并进入 Active。

## L-Iter

Reloading Fiber 收集一个 inverse，并继续下一个 activation step。

## L-Leave

Active Fiber 停止提供 Binding 并进入 Unloading。

## L-Raise

Activation step 失败后进入 Unloading。

## L-Unload

Withdrawal guard 释放后运行 accumulator、丢弃 Episode 并回到 Inactive。

## O-Insert / O-Retire / O-Remove

Fiber tree 的三个 orchestration 操作：插入 Fiber；无条件标记退休；在 Fiber 已 Inactive 且
没有 child/effect/Binding 后从 registry 移除。Mount inverse 只执行 O-Retire。

## Pending

Inactive、无 failure、未 retired 且没有 ActivationKey 的公共状态。无 Requirement 的 leaf
可以直接从 Pending 开始激活。

## Provider

拥有 Binding 的 Fiber。

## quiescent

没有 lifecycle step、running effect step、running inverse、reconcile work 或待执行
O-Remove，registry 中没有 retired 但尚未移除的 Fiber，且每个 Fiber 已达到 latest
ActivationKey，或已处于 Failed/Disposed settled 终态的 Runtime 状态。

## Realm

Service resolution domain，由 RealmId 标识。每个 Runtime 的 default Realm 是同一
RuntimeTag/local 0，可由 RootContext/Context::default_realm 取得。

## reconcile

根据 Fiber 当前状态和 latest ActivationKey 选择下一条生命周期规则。

## Reloading

Fiber 正在执行 activation，持有固定 CommittedView 和逐步增长的 accumulator。

## Requirement

Component 声明的 Required service coeffect。

## relied

某个 Provider 出现在 installed Consumer 的 CommittedView 中。

## Runtime

保存 Fiber registry、Binding store、watcher 和 Root effects 的 Core 实例。

## RuntimeEvent

Actor 在状态提交后发送的结构化事件，带 Runtime-local 单调 sequence。它用于 trace、日志和
Host metrics，不反向改变 Runtime 状态。

## Scope

Component 创建 effect、provide 和 mount 的状态变更 capability。

## settled

单个 Fiber 已完成可立即执行的 lifecycle reconcile，并处于 Pending、Active、Failed 或
Disposed 边界。settled 不要求 Active Fiber 上的独立 Scope effect 已完成；后者仍会阻止
Runtime quiescent。

## ServiceContract

ServiceId、Provider 类型、contract-local Client、endpoint adapter 和 interception metadata
的契约。宏在 contract 所属 crate 中生成 Client newtype。

## Service client

`Context::service::<C>()` 返回的 `C::Client`。调用方法定义在 contract-local newtype 上，
内部持有 Core 构造的 ServiceRef。

## ServiceId

稳定的 service contract 名称。公共 `as_str` 返回规范名称。

## ServiceRef

Core 为 CommittedEntry 构造的 opaque call base，固定 endpoint 和 effective intercept，再
由 ServiceContract 包装成 `C::Client`。doc-hidden dispatch hook 只供宏生成 adapter 使用；
组件不得在所属 Episode 完成后继续使用 Client/ServiceRef。

## TaskSpawner

Host 注入的非阻塞 RuntimeTask 提交接口。spawn 只能调度，不能在 Actor 调用栈内同步 poll
task。

## Target

按 Requirement 顺序排列的 Provider FiberId 序列。

## Unloading

Fiber 已停止向新 Consumer 提供 Binding，正在等待 Consumer 或执行 Episode accumulator
的内部状态；公共 API 也用它表示已 retired、已回到 Inactive、但尚未从 registry 移除的
Fiber。

## watcher

BindingSlot 到可能受影响 Fiber 的响应式索引。

## withdrawal

Provider Fiber-wide withdrawal 先删除 BindingRecord、等待 committed Consumer 离开，再执行
Episode accumulator。手动 provide withdrawal 删除单个 BindingRecord、通知全部相关 watcher，
但只等待精确 committed 到该 BindingId 的 Consumer Episode。捕获的 Episode 结束，或仍是
同一 Episode 且 Active、key 最新时，该等待完成；后续 Episode 不延长等待。它不采用
`FiberHandle::wait_settled` 的公共 settled 条件，不等待 child O-Remove，也不让无 committed
依赖的 watcher 建立回收等待边。旧 CommittedView 可以继续持有旧 endpoint。

## coeffect

Component 必须声明才能读取的环境输入；Linorun 用 Requirement 和 CommittedView 表示。

## emission

不可由 inverse 精确撤销的外部操作，例如发送消息或支付；由业务补偿协议负责。

## RootContext

Synthetic Root Fiber 的 Host 入口。它创建 Realm、effect、provide 和顶层 mount，所有变化
仍进入 Root Episode accumulator。Root 使用保留 ComponentId `linorun/root@1`。

## withdrawal guard

Provider 开始 Episode cleanup 前，等待所有 installed Consumer 丢弃指向该 Provider 的
CommittedView 的顺序条件。

## ActivationFailure / EffectError / CleanupError / CleanupFailure

分别表示 activation forward、独立 effect forward、inverse 叶子错误，以及带 owner、Fiber、
Episode、seq 和可选 label 的 cleanup 聚合记录。
