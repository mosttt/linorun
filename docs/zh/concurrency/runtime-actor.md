# Runtime Actor

简体中文 | [English](../../en/concurrency/runtime-actor.md)

实现分布在 `runtime/mod.rs`（控制面）、`api.rs`（命令入口）、`completion.rs`（共享结果）
和 `task.rs`（用户任务边界）；`runtime/tests.rs` 验证该生产实现。

## 1. 角色

Runtime Actor 是 Core 控制面的唯一可变状态所有者：

~~~rust
struct ActorCore {
    runtime_state: RuntimeState,
    contracts: ContractRegistry,
    bindings: BindingTable,
    watchers: Map<BindingSlot, Set<FiberId>>,
    consumers_by_provider: Map<FiberId, Set<FiberId>>,
    fibers: Map<FiberId, FiberRecord>,
    effect_owners: Map<EffectId, FiberId>,
    // identity/sequence counters, waiters and event sender
}

struct TurnOutput {
    events: Vec<RuntimeEvent>,
    tasks: Vec<RuntimeTask>,
    deferred_drops: Vec<Box<dyn Send + 'static>>,
}
~~~

activation 的唯一 StepId 保存在 `LifecycleState::Reloading.step`，effect 的唯一 StepId 保存在
`EffectRecord.running_step`。driver 同时只由对应的一个 RuntimeTask 持有。

FiberId、EpisodeId、EffectId、BindingId、StepId 由 Actor 的 `IdentityCounters` 分配；
RealmId 由 RuntimeHandle 的原子 allocator 分配；snapshot sequence 和 event sequence 由
`SequenceCounters` 分配。

Actor 串行修改：

- Fiber registry；
- lifecycle state；
- Binding store；
- watcher 与 committed consumer indexes；
- EffectRecord；
- snapshot sequence；
- event sequence。

Actor 初始化 RuntimeCore 时同时创建 Active Root Fiber、
`ActivationKey { target: Target { providers: [] }, revision: 0 }`、空 CommittedView 和 Root Episode。
Root 使用 FiberId(0)/EpisodeId(0)，default Realm local 为 0；其余 identity 和 event/snapshot
sequence 从 1 开始。若配置 event Sender，build 的初始提交首先发送 synthetic Root 的
FiberInserted；Root 是直接构造的 Active 基点，不发送 LBegin/LFinish。

## 2. Actor 不执行用户代码

以下工作在 Actor 外执行：

- Component activation step；
- effect forward step；
- inverse；
- service endpoint call；
- event Sender 的 Receiver 侧处理、日志和 metrics 聚合。

Actor turn 只做有限的内存状态变更和任务调度。Reducer 从状态中移出的 endpoint、Config、
implementation、driver/continuation 和 User InverseAction 必须进入 deferred_drops，不能在
持有 RuntimeCore 借用时析构。

## 3. Command

内部命令：

~~~rust
enum Command {
    Mount { /* owner, descriptor, implementation, config, route, reply */ },
    StartEffect { /* owner, label, driver, reply */ },
    RegisterCleanup { /* owner, label, inverse, reply */ },
    Provide { /* owner, route, contract, endpoint, reply */ },
    ActivationFinished { /* fiber, episode, step, completion */ },
    EffectStepFinished { /* fiber, episode, effect, step, completion */ },
    InverseFinished { /* fiber, episode, seq, result */ },
    FiberState { /* fiber, reply */ },
    WaitSettled { /* fiber, reply */ },
    WaitEffectForward { /* effect, completion, reply */ },
    DisposeEffect { /* effect, completion, reply */ },
    Restart { /* fiber, reply */ },
    UpdateConfig { /* fiber, config, type, reply */ },
    Replace { /* fiber, descriptor, implementation, reply */ },
    Dispose { /* fiber, reply */ },
    Snapshot { /* reply */ },
    WaitQuiescent { /* reply */ },
    Shutdown { /* reply */ },
}
~~~

命令名称是实现细节；每个命令必须映射到 运行时规范 状态转移。

## 4. Actor turn

一个 turn：

1. 接收一个 Command；
2. 验证当前 record 和 EpisodeId；
3. 应用一次有限 reducer；
4. 反复按 FiberId 顺序扫描 registry，直到没有可立即应用的状态规则；
5. 完成已满足条件的 settled/shutdown/quiescent waiter；
6. 结束 reducer 并释放 RuntimeCore 借用；
7. 依次析构 deferred_drops；
8. 发送事件并 spawn 用户任务。

直接命令的 reply 在对应 handler 内发送，等待条件的 reply 在 waiter flush 中发送；它们不是
post-commit 阶段的最后一步。当前 turn 仍会完成上述同步工作后才处理下一条命令。API 收到
reply 不表示用户任务已执行完毕；启动结果使用 wait_settled、wait_forward 或 wait_quiescent 观察。

Actor 不跨 await 保留 RuntimeCore 的可变借用。

`deferred_drops` 只是 post-commit ownership bundle，不是 lifecycle state。组件类型的 Drop 必须
非阻塞、不得 panic 且不承担资源回收；需要等待的关闭逻辑必须是 inverse。

所有用户 task 和 Actor task 都通过 Builder 提供的 TaskSpawner 提交；spawn 只调度，不得在
当前 Actor 调用栈内同步 poll task，也不得 panic。

WaitSettled、WaitQuiescent 和 Shutdown 在条件尚未满足时保存 Reply waiter；Actor turn 本身仍立即结束，后续 reconcile 达到条件时完成 waiter。

## 5. Reconcile fixed point

每个命令完成局部写入后，Actor 对 BTreeMap 中的 Fiber 按 FiberId 顺序执行 reconcile pass；
只要某条规则发生状态变化，就再执行一轮。循环在一整轮没有变化时结束。这个固定点过程让
Binding、revision、parent retire 和 cleanup completion 的同步连锁在处理下一条命令前完成，同时
不为互不依赖的 Fiber 声明额外的公共总顺序。

每次 `reconcile_one` 只处理一个 Fiber 的当前状态：

~~~text
Inactive + key          -> L-Begin
Reloading + boundary    -> L-Iter / L-Finish / L-Divert
Active + key changed    -> L-Leave
Unloading + guard open   -> request episode cleanup
Unloading + cleanup done -> finish L-Unload
retired Inactive        -> O-Remove when child set empty
~~~

无需 await 的连锁 Leave 在同一 turn 中通过后续 pass 传播。需要 activation step、inverse
或 withdrawal guard 的 Fiber 留在相应过渡状态，等待 completion 或后续命令。

## 6. Target 计算

~~~rust
fn activation_key(
    core: &RuntimeCore,
    fiber: &FiberRecord,
) -> Option<ActivationKey>
~~~

算法：

1. synthetic Root：Running 返回空 Target/revision 0，ShuttingDown/Stopped 返回 None；
2. 普通 Fiber retired 则 None；
3. 按 Requirement 顺序解析 BindingSlot；
4. Slot 必须存在已注册 BindingRecord；
5. Binding.contract_type 必须与 Requirement.contract/Runtime contract map 一致；
6. Binding 的 Provider 必须 Active 且未 retired；
7. 收集 Provider FiberId；
8. 附加 fiber.activation_revision。

函数只读 ActorCore，不调用组件代码。

## 7. L-Begin 调度

Actor 原子完成：

1. 再次计算 ActivationKey；
2. 捕获 CommittedView 及已验证的 declared interception metadata；
3. 分配 EpisodeId；
4. 写入 Reloading；
5. 用捕获的 implementation/config/ComponentContext 构造尚未调用用户代码的
   ActivationDriver::Init；
6. 把 StepId 写入 `LifecycleState::Reloading.step`。

提交后把 Init driver 转交 RuntimeTask。第一次 poll 才调用 `Component::activate` 并进入
Once/Steps；同步 panic 由 task 转成 ActivationFailure，Exhausted wrapper 仍随 completion
归还。

Consumer 从写入 Reloading 起成为 installed，因此 Provider withdrawal guard 会观察到它。

## 8. Step completion

~~~rust
struct ActivationCompletion {
    result: Result<DriverEvent, ActivationFailure>,
    driver: ActivationDriver,
}

struct EffectStepCompletion {
    result: Result<DriverEvent, EffectError>,
    driver: ErasedEffectDriver,
}

Command::ActivationFinished { fiber, episode, step, completion }
Command::EffectStepFinished { fiber, episode, effect, step, completion }
~~~

Activation completion 的有效条件是 Fiber 仍处于匹配 Episode 的 Reloading，且
`LifecycleState::Reloading.step` 等于返回的 StepId。Effect completion 的有效
条件则是匹配 Episode 仍 installed、对应 EffectRecord 为 Applying 或 Disposing，且
`EffectRecord.running_step` 等于返回的 StepId；正常 effect completion 可以发生
在 Reloading、Active 或 Unloading。不能通过这些 identity 校验的 completion 不得修改状态，
其返回 driver 只进入 post-commit deferred drop。

随后：

- Yield：以 InverseOwner::Activation 分配 seq、加入 accumulator；
- failure：L-Raise；
- `Yield { has_next: true }` 使返回的 driver 成为 continuation；Actor 比较最新 ActivationKey，决定把它
  移入下一 RuntimeTask，或在 L-Divert 提交后析构；
- final Yield、Done 和 Err 的返回 driver 禁止再 poll，并在对应状态提交后析构；done/final
  Yield 比较最新 ActivationKey，决定 L-Finish 或 L-Divert。

step task 每次只 poll 一步，并把仍存在的 driver 所有权随 completion 交回。Actor 只保存
StepId；Actor 和 task 不会同时持有 driver，task 也不能自行越过 Actor boundary。
StepId 写入 record 与 driver 向 RuntimeTask 的转交在同一次 Actor 提交中发生，并定义 step 已开始；
即使 TaskSpawner 尚未首次 poll，之后处理的 Target/dispose command 也只记录停止意图并等待
该 step completion。

## 9. Effect completion

EffectRecord 在 Applying step 完成前不会被删除。

dispose_requested 只改变 EffectState：

    Applying | Armed -> Disposing

当前 step 完成后先清空 `EffectRecord.running_step`；Yield 以 InverseOwner::Effect(id) 收集 inverse。随后：

- step error：保存 forward_error，切换为 Disposing，只请求回收该 EffectId 的 entries；
- Effect 已被 manual/Fiber-wide disposal 标记为 Disposing：不启动下一 iteration，请求该
  effect cleanup；
- 仍为 Applying 且 `Yield { has_next: true }`：把返回 driver 移入下一 effect RuntimeTask；
- 仍为 Applying 且 final Yield/Done：EffectRecord 进入 Armed，forward outcome 为 Completed。

不继续的返回 driver 都在对应状态提交后析构。独立 effect error/manual cleanup 不改变 Fiber
lifecycle；Fiber-wide 路径继续等待全部 driver boundary 与 withdrawal guard。

## 10. 进入 Unloading

L-Divert、L-Raise 和 L-Leave 共享同一个 `enter_unloading` reducer。Actor 在一个 turn 中：

1. Reloading/Active -> Unloading，并保留对应 outcome；
2. 从 store 删除 Provider-owned BindingRecord；
3. 当前 Episode 的 Applying/Armed EffectRecord -> Disposing；
4. 后续 fixed-point pass 让 watcher Consumer 观察变化；
5. Provider 保持 Unloading，等待 step boundary 和 guard。

状态提交后，新 Target 不再选择该 Provider；具体 lifecycle event 仍按入口分别为 LDivert、
LRaise 或 LLeave。每个首次转为 Disposing 的 effect 发送一次 EffectDisposeRequested。

## 11. Withdrawal guard

~~~rust
fn is_relied_on(
    core: &RuntimeCore,
    provider: FiberId,
) -> bool
~~~

真值条件：

    any installed Fiber
        other than provider
        whose CommittedView contains provider

RuntimeCore 维护：

~~~rust
consumers_by_provider: Map<FiberId, Set<FiberId>>
~~~

该索引在 L-Begin 加边，在 L-Unload 删除边。

## 12. Cleanup scheduler

每个 Fiber 只有一个跨 turn 的执行记录：

~~~rust
enum RunningInverse {
    User(RunningMeta),
    Withdraw {
        meta: RunningMeta,
        consumers: Vec<(FiberId, EpisodeId)>,
        endpoint: Arc<dyn ErasedEndpoint>,
    },
}

struct FiberRecord {
    cleanup_running: Option<RunningInverse>,
    cleanup_errors: Vec<CleanupFailure>,
    episode_cleanup_errors: Vec<CleanupFailure>,
    // lifecycle, effects and accumulator
}
~~~

`EffectState::Disposing` 就是该 effect 的 cleanup request；
`LifecycleState::Unloading` 就是整个 Episode 的 cleanup request，不另设重复的 requested set
或 episode flag。

RetireFiber 在一个 turn 内完成，不写入 `cleanup_running`。

当 `cleanup_running` 为 None：

1. 若 Fiber 为 Unloading 且 L-Unload 的 withdrawal guard/forward boundary 尚未同时满足，则
   不选择任何 entry；
2. 确认请求覆盖的 effect driver 都已到达边界；
3. Fiber 为 Unloading 时，请求范围是全部 live EffectRecord 和整个 accumulator；否则
   请求范围是状态为 Disposing 的 effects 及 owner 位于其中的 entries；
4. 对请求范围中已经到达 forward boundary、且在 accumulator/cleanup_running 中已无 entry 的 EffectRecord，
   写入共享 completion，并在同一 turn 移除 record；空 effect
   不创建 inverse；
5. 若该步移除了 record，则重新计算选择集合；若不存在 candidate，则完成相应手动
   request，或在 Unloading 的终局条件满足时完成 L-Unload；
6. 存在 candidate 时，选择最大 seq 并 take InverseAction；
7. User：写入 `cleanup_running`，提交后通过 TaskSpawner 运行 Future；
8. RetireFiber：在本 turn 应用 O-Retire，连续发布 InverseStarted/InverseFinished；
9. WithdrawBinding：仅当 `bindings[slot].id == binding` 时删除仍注册的旧 Binding；若 Slot
   已空或已由另一 BindingId 占用则不修改 topology。删除旧 Binding 时先发布 BindingRemoved，
   捕获 CommittedView 持有该 BindingId 的 `(FiberId, EpisodeId)`，再发布 InverseStarted，
   把 endpoint 存入 RunningInverse。每个捕获的 Episode 均满足以下条件之一后，释放
   endpoint、发布 InverseFinished：已离开 installed；或仍是同 Episode 且达到 Active、
   key 与最新 ActivationKey 一致。

InverseFinished 到达后：

1. 验证并清除匹配的 `cleanup_running`；
2. 记录错误；若失败，则按本次 Actor 顺序追加到 Episode、对应
   Effect collector、self 及 parent chain 中所有已 retired Fiber collector，并在
   ShuttingDown 时追加到 shutdown collector；
3. 下一次 fixed-point pass 继续 Fiber。

WithdrawBinding 在发布 InverseStarted 前按 BindingId 删除仍匹配的 Binding 并捕获实际
Consumer Episode；后续 fixed-point pass 继续推进全部受影响 watcher，但仅 watch 同 Slot
不会产生清理等待。捕获的 Episode 已离开后，不再等待同 Fiber 的后续 Episode，也不等待
retired Fiber 的 child O-Remove。同 Provider 很快重新发布同 Slot 时，原 Episode 的
ActivationKey 可恢复并继续使用旧 endpoint。Slot 中的替代 Binding 不受影响。
RetireFiber 和 WithdrawBinding 都不 spawn 用户 Future。

## 13. Binding 生命周期

provide command 在一个 turn 中创建：

- ServiceId/Contract TypeId mapping（若尚未存在）；
- EffectRecord；
- BindingRecord；
- owner_effect 关系；
- watcher refresh。

这些写入是一个 reducer 提交：Actor 先完成 Context、descriptor、Slot 和 contract 校验；
任一校验失败时，不插入 contract mapping、EffectRecord 或 BindingRecord。

Binding topology removal 与 watcher reaction 的顺序：

    registered
      -> manual provide dispose or Provider enters Unloading
      -> remove BindingRecord
      -> watcher reconcile
      -> captured consumer episodes reach their withdrawal boundary

Provider Fiber-wide unload 在进入 Unloading 时移除全部 BindingRecord，并在 relied(provider) 为 false 后运行 accumulator。

改变 Binding visibility 的命令在当前 turn 内处理完直接 watcher 以及无需 await 的同步 Leave 闭包。需要 step、inverse 或 withdrawal guard 的 Fiber 保持在相应过渡状态。

## 14. Parent/child

O-Insert 同时更新：

- fibers[child]；
- fibers[parent].children；
- 父 Episode 的 mount EffectRecord/InverseEntry；
- child.mount_effect。

Dispose command 立即执行 O-Retire：写入 retired、清除 child.mount_effect，并且只能 take 父
Episode 中 action 精确等于 `RetireFiber(child)` 的 mount InverseEntry，把它
取出、完成并移除该 EffectRecord；随后等待 child
FiberCompletion。若父 scheduler 已先选择 mount entry，则它执行同一 reducer，后来的
Dispose command 只注册 completion waiter。Actor 串行化二者，因此不会重复调用 inverse。

O-Remove child 在同一 turn 中先从 registry、watcher/consumer index 与 parent.children 删除，
再向 FiberCompletion 发布 Disposed/final report；下一 fixed-point pass 重新检查 parent。
Descendant failure 已在发生时追加到 retired ancestor collector，O-Remove 不再整批复制 report。
Parent retired、children 为空且其余终局条件满足时可以 O-Remove。

## 15. Snapshot

Snapshot command 在 Actor turn 中读取一致状态：

~~~rust
pub struct RuntimeSnapshot {
    pub sequence: u64,
    pub runtime_state: RuntimeState,
    pub quiescent: bool,
    pub fibers: Vec<FiberSnapshot>,
    pub bindings: Vec<BindingSnapshot>,
    pub effects: Vec<EffectSnapshot>,
}
~~~

snapshot 可以显示非 quiescent 状态，但内部字段必须来自同一 Actor turn。

## 16. Shutdown

第一次 Shutdown：

1. runtime_state 设为 ShuttingDown；
2. Root Active -> Unloading，并删除 Root BindingRecord；
3. Root 的全部 live EffectRecord -> Disposing；Applying driver 只完成当前 step；
4. Root Episode cleanup 按 accumulator seq 回收 effect；mount inverse 在轮到时 retire
   顶层 Fiber，但不等待其 lifecycle；
5. 保存 shutdown waiter；
6. 通过普通 reconcile 推进。

所有非 Root Fiber 已移除且 Root Episode 完成后：

1. runtime_state 设为 Stopped；
2. 保存 ShutdownReport；
3. 完成全部 waiter。

## 17. 便捷 API 的提交路径

Runtime 的直接操作委托 RootContext；ComponentContext 的操作委托其原 Context/Scope。
闭包 TaskSpawner 与 trait 实现遵循相同的非阻塞提交契约。便捷入口不创建另一条状态修改路径。

`on_cleanup` 通过 RegisterCleanup 提交 User inverse，复用 Effect 路径登记同步 driver，再等待 wait_forward。
登记阶段不调用 cleanup callback。连续 await 登记确定 accumulator 顺序；与 shutdown 竞争时，
已经提交的登记 step 仍遵守 inertia，可能在登记调用恢复前完成清理。返回的 EffectHandle
保留终态，重复 dispose 不再调用 inverse。普通关闭后登记返回 RuntimeShuttingDown。

收尾例外：当同一 Episode 仍 installed、尚未开始 unload accumulator，且 activation 或 effect
step 仍在运行时，允许 on_cleanup 补交该 step 已获得资源的 inverse，包括 shutdown/retire 后。
若 Fiber 已 Unloading，新登记 Effect 立即标为 Disposing；全部 forward step 落地后一起回收。
这一例外不开放新的普通 effect、provide 或 mount，也不接受旧 Episode 的 Scope。

`effect_owners` 与 Fiber.effects 的插入、移除必须在同一次状态提交中执行，Debug 和 Release
构建都执行状态更新；debug assertion 仅检查结果，不承载更新操作。
