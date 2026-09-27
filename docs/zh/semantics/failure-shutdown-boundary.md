[简体中文](failure-shutdown-boundary.md) | [English](../../en/semantics/failure-shutdown-boundary.md)

# 失败、关闭与系统边界

## 1. 失败分类

Core 区分：

- descriptor 或 API 调用错误；
- activation failure；
- inverse failure；
- user task panic；
- Runtime shutdown 中的聚合失败。

## 2. Descriptor/API 错误

以下调用级错误在产生状态变更前返回；它们不自动使 Fiber Failed：

- RuntimeShuttingDown；
- DuplicateBinding；
- UndeclaredProvide；
- UndeclaredDependency（Context::service）；
- InactiveContext（Scope 的 Context 归属校验）；
- InactiveScope；
- DescriptorMismatch；
- ServiceTypeMismatch；
- RealmMismatch（isolate 的独立返回类型）；
- FiberDisposed。

DescriptorMismatch 包括 mount 时 descriptor/config 类型不一致、update_config 的 Config
TypeId 不一致，以及 replace 未复用同一静态 descriptor 或 Config 类型不一致。
ServiceTypeMismatch 包含 ServiceId、Contract/Intercept channel 和 expected/actual TypeId；
调用级错误枚举保留该 payload。RealmMismatch 携带传入的跨 Runtime RealmId。

## 3. Activation failure

activation step 返回 Err：

1. L-Raise；
2. 进入 Unloading；
3. 撤销此前成功 step；
4. 删除本 episode 已注册的 BindingRecord；
5. 进入 `Inactive { failure: Some(error) }`；
6. 对外显示 Failed。

错误记录在 Fiber，而不是向 parent 或 sibling 扩散。

## 4. Failed retry

Failed Fiber 不因 service 抖动自动重试。

以下操作清除 failure：

- restart；
- config update；
- implementation replace。

dispose 直接 retire Fiber。

`Inactive { failure: Some(_) }` 不能直接 L-Begin。Failed 是 quiescent/settled 终态；
只有上述显式操作会清除 failure。

## 5. Effect forward failure

独立 Scope effect 的 driver 返回 Err：

1. EffectRecord 保存 forward error 并进入 Disposing；
2. 当前 EffectId 已产生的 inverse 按 seq 降序执行；
3. EffectHandle completion 返回 forward error 与 EffectDisposeReport；
4. Fiber 保持当前 lifecycle state。

activation 代码显式传播该错误时，activation step 才返回 ActivationFailure 并执行 L-Raise。

## 6. Inverse failure

inverse 返回 CleanupError 或 unwind panic：

- 包装为 CleanupFailure 并记录 owner、seq、label 和错误；
- 标记该 inverse 已尝试；
- 继续更早的 inverse；
- report 聚合全部 CleanupFailure。

Runtime 不重试 inverse。

原因是 Runtime 无法判断重复 inverse 是安全重试、重复释放还是第二次业务补偿。

EffectDisposeReport 只收集对应 EffectId；Fiber DisposeReport 从 O-Retire 到 O-Remove 收集
目标 subtree；ShutdownReport 从 ShuttingDown 到 Stopped 收集整个 Runtime。正常 reload 已
发布的历史错误不在后续报告中重复聚合。

## 7. Panic

panic=unwind：

- activation task 边界捕获后按 L-Raise；
- 独立 effect forward task 边界捕获后按 EffectError；
- inverse task 边界捕获后按 CleanupError，再包装为 CleanupFailure；
- Actor task 本身不运行用户 callback。

## 8. 永不结束的 Future

一个 activation step、effect forward step 或 inverse 永不完成时：

- activation step 使 Fiber 保持 Loading；
- Fiber-wide cleanup 的 step 或 inverse 使 Fiber 保持 Unloading；
- 独立 effect forward 或手动 effect cleanup 保持当前 lifecycle state，并使 Runtime 处于非 quiescent；
- Runtime snapshot 显示进行中的 step；
- 不伪造完成；
- 不丢弃 Future 作为 rollback。

因此 graceful `Runtime::shutdown` 在这些 Future 永不完成时也会一直 pending；Core 不伪造
完成结果。

## 9. Runtime shutdown

Runtime 具有三个 Host 状态：

~~~rust
pub enum RuntimeState {
    Running,
    ShuttingDown,
    Stopped,
}
~~~

shutdown：

1. Running -> ShuttingDown；
2. 拒绝新的 Root/Scope effect、mount、provide 和 Fiber restart/update/replace；
3. synthetic Root 进入 Unloading 并撤回 Root Binding；
4. 运行普通 dependency withdrawal；
5. Root accumulator 按 LIFO 撤销 effects；mount inverse 在其 seq 轮到时 retire child，
   不等待 child lifecycle；
6. 等 child registry 清空并完成 Root Episode；
7. 进入 Stopped；
8. 返回 ShutdownReport。

ShutdownReport：

~~~rust
pub struct ShutdownReport {
    pub cleanup_errors: Vec<CleanupFailure>,
}
~~~

cleanup error 不阻止 Runtime 到达 Stopped，只使 report 非空。

所有要求 RuntimeState == Running 的调用在 ShuttingDown 或 Stopped 时统一返回
RuntimeShuttingDown。只读 state/snapshot、wait/dispose、重复 shutdown 仍工作；
default_realm/create_realm/isolate/intercept 只派生 opaque identity/route，不修改 RuntimeCore，
也仍可调用。

`on_cleanup` 的专用 `RegisterCleanup` 命令允许已有 step 完成资源清理登记：登记必须属于
同一 Episode，该 Episode 尚未开始 unload cleanup，且 activation 或 effect 仍有 in-flight
step。满足这些条件时，即使 Runtime 已 ShuttingDown 或 owner 已 retired，登记仍可完成，
并在 Unloading 中立即加入待回收集合。这不允许新的 effect、provide 或 mount，也不恢复
过期 Scope；详见 [Effect API 约束](effects.md#12-effect-api-约束)。

## 10. 重复 shutdown

shutdown 是幂等 Host 操作：

- Running 时启动关闭；
- ShuttingDown 时等待同一关闭过程；
- Stopped 时返回保存的最终 report。

## 11. Root failure

Root 不执行普通 Component activation。Root effect inverse 失败按 cleanup failure 记录，shutdown 继续。

## 12. 语义边界

Core 能保证：

- 已记录 inverse 至多调用一次；
- cleanup 次序；
- dependency withdrawal 次序；
- committed dependency 稳定；
- lifecycle transition 串行化。

inverse 对外部状态的正确性属于组件 EffectContract；Realm 的语义仅为 service resolution。
Host 必须显式 await Runtime::shutdown；Rust Drop 不能替代异步回收。

## 13. 错误记录

诊断错误至少包含适用字段：

- FiberId；
- EpisodeId；
- EffectId；
- InverseOwner/seq；
- BindingId；
- ComponentId；
- ServiceId；
- phase；
- FailureKind 和 message。

错误记录用于定位，不改变 Target 或 lifecycle identity。
