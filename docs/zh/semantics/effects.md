[简体中文](effects.md) | [English](../../en/semantics/effects.md)

# Effect 与回收

## 1. 定义

Effect 是一次对运行环境的变化，以及能够恢复该变化的 inverse：

    effect : Context -> Context × Inverse

组件负责给出 inverse，Runtime 负责跟踪和调用。

典型 effect：

- 注册事件监听器；
- 打开监听端口；
- 启动后台任务；
- provide service；
- mount child component；
- 安装路由或定时器。

一次不可逆的网络发送、日志输出或支付操作不是可精确恢复的 effect。它只能被视为 emission，或由业务层提供补偿。

## 2. Rust 表示

概念接口：

~~~rust
type InverseFuture =
    Pin<Box<dyn Future<Output = Result<(), CleanupError>> + Send>>;

type InverseFn =
    Box<dyn FnOnce() -> InverseFuture + Send>;

struct AppliedEffect {
    inverse: InverseFn,
}
~~~

FnOnce 表达 inverse 只能被消费一次。运行时状态机仍必须保证并发 dispose 时最多调用一次。

## 3. EffectRecord

~~~rust
struct EffectRecord {
    id: EffectId,
    episode: EpisodeId,
    label: Arc<str>,
    state: EffectState,
    running_step: Option<StepId>,
    forward_error: Option<EffectError>,
    cleanup_errors: Vec<CleanupFailure>,
    completion: Arc<EffectCompletion>,
}

struct InverseEntry {
    seq: u64,
    owner: InverseOwner,
    label: Option<Arc<str>>,
    action: InverseAction,
}

enum InverseAction {
    User(InverseFn),
    RetireFiber(FiberId),
    WithdrawBinding {
        binding: BindingId,
        slot: BindingSlot,
        endpoint: Arc<dyn ErasedEndpoint>,
    },
}

enum InverseOwner {
    Activation,
    Effect(EffectId),
}
~~~

Episode accumulator 唯一持有 InverseEntry。Activation 产物使用 InverseOwner::Activation；
一次 Scope::effect 调用对应一个 EffectRecord，其产物使用 InverseOwner::Effect(id)。
EffectRecord 不复制 seq 列表，手动 dispose 和 snapshot 按 owner 过滤 accumulator。

普通组件 inverse 使用 User；mount/provide 分别使用 Core-owned RetireFiber 和
WithdrawBinding。Core action 仍参与同一 seq/LIFO 选择，但不经过用户 Future。

每个 inverse 在成功 yield 时获得 Fiber 内递增的 seq。seq 表示真实 application order，而不是 effect 创建顺序。
这里的 application order 精确定义为 Actor 接受 Yield 的顺序。组件必须在外部变化成功后
立即 yield inverse；Runtime 不猜测用户 Future 内部更早发生的不可观察变化。

若同一 Fiber 的两个 concurrent forward 会修改不满足交换律的外部状态，组件必须先等待
前一个 EffectHandle::wait_forward，再启动后一个；只有可交换的变化才可无序并发。否则 Actor
接受 Yield 的顺序可能与 Future 内部的实际写入时刻不同，Core 无法替组件恢复正确次序。

step task 一次只 poll driver 的一个 step，并在 completion 中把 Yield/Done/error 与 driver
一起移交 Actor。只有 `Yield { has_next: true }` 时该 driver 才是可继续的 continuation；
Actor 决定继续时把它移入下一 RuntimeTask，其他结果或 dispose/divert 则在提交状态后析构。
task 不自行越过 Actor boundary poll 下一步。

StepId 写入 record 与 driver 向 RuntimeTask 的转交一经 Actor 提交，该 step 就算已经开始；执行器
是否已做第一次物理 poll 不构成新的可取消状态。之后到达的 dispose/Target change 等该 step
落地，只阻止 Actor 再转交 continuation。

provide/mount 是 Core-owned 的原子 effect。它们在一个 Actor turn 中直接建立 EffectRecord、
inverse entry 与 topology，forward 随提交完成，不创建用户 driver 或 StepId；cleanup 仍进入
同一个 Episode accumulator。

## 4. 支持的执行形态

### 4.1 同步单步

    forward()
      -> inverse

forward 返回时 effect 已 Armed。

### 4.2 异步单步

    await forward()
      -> inverse

整个 Future 是一个 inertial step。dispose 或 Target 变化不能直接丢弃它；Future 完成后，Runtime 收集 inverse，再进入 cleanup。

### 4.3 同步 iterator

    yield inverse 1
    yield inverse 2
    yield inverse 3

每个 yield 形成 effect boundary。

### 4.4 异步 iterator

    await step 1
    yield inverse 1
    await step 2
    yield inverse 2

Target 变化或手动 dispose 发生时：

- 已经开始的 await step 必须完成；
- 它的 inverse 被收集；
- 后续 iteration 不再启动；
- 已收集 inverse 进入 LIFO cleanup。

Actor 在每次提交下一 iteration 前检查最新目标；已提交的 iteration 仍须完成并交回清理。

## 5. Accumulator

Episode accumulator 保存已产生的 inverse：

    apply e1 -> g1
    apply e2 -> g2
    apply e3 -> g3

恢复顺序：

    g3
    g2
    g1

形式上，组合 accumulator 是：

    g1 ∘ g2 ∘ g3

应用组合函数时，最晚产生的 inverse 先作用。

## 6. 串行 cleanup

同一 Fiber 同时最多运行一个 inverse。

选择算法：

1. 收集目标 dispose 请求覆盖的 accumulator entries；
2. 选择 seq 最大者；
3. take entry，把跨 turn 状态写入 `cleanup_running`；
4. 在 Actor 外 await；
5. completion 返回后清除 `cleanup_running`；
6. 重复。

同一 Fiber 的 cleanup scheduler 始终等待当前 inverse 完成，再选择下一个 inverse。

已请求回收的 effect 若已到 forward boundary，且在 accumulator/cleanup_running 中都没有 inverse，则直接完成
EffectCompletion 并移除 EffectRecord。空 effect、在第一次 Yield 前失败的 effect，以及所有
entry 已完成的 effect 都走这条规则；Runtime 不创建空操作 inverse。

## 7. 手动 dispose

EffectHandle::dispose：

- 幂等；
- 第一次调用关闭该 effect 的后续 iteration；
- 等待当前 step 落地；
- 撤销该 effect 已产生的 inverses；
- 后续调用等待同一终局；
- Fiber unload 不会再看到已经完成并移除的 inverse。

EffectHandle::wait_forward 在 driver 的终止 boundary 完成：自然结束为 Completed，被 manual
dispose/Fiber unload 截止为 Disposed，forward error 为 Err。它不等待 inverse cleanup。

同步 iterator 的每个 yield 都按 application order 收集；dispose 被 Actor 观察后不再启动下一 iteration。

异步 iterator 可能处于 step in flight；Runtime 不能在拿到 inverse 前宣称 forward 已撤销。

Fiber-wide unload 会关闭本 Episode 的所有 effect driver。每个 driver 只让当前 step 落地，不再启动 continuation；全部 driver 到达边界后，Runtime 才开始全 Fiber LIFO cleanup。

Fiber 进入 Unloading 后，尚未开始的手动 dispose 只增加 cleanup request；它必须与整个
Episode 一起等待 withdrawal guard，不能提前启动 inverse。

## 8. 嵌套 effect

嵌套 effect 仍按实际 yield/registration 顺序分配 seq。

例如：

    outer yields g1       seq 1
    inner yields g2       seq 2
    outer yields g3       seq 3

Fiber unload 顺序为 g3、g2、g1。

手动 dispose inner 只选择 inner 所拥有的 inverse。随后 Fiber unload 只处理剩余 accumulator inverse。

## 9. provide 和 mount

provide 与 mount 不是特殊生命周期系统，而是普通 effect：

### provide

Core-generated EffectRecord label 为 `linorun:provide:{ServiceId}`。

forward：

1. Scope provide 检查 Component descriptor 声明；RootContext provide 作为 Host-owned Root
   effect 不受普通 Component provides 集合限制；
2. 检查 BindingSlot 空闲；
3. ServiceContract adapter 产生 endpoint Arc 和对应 ContractVTable；
4. 在同一 Actor 提交中插入 BindingRecord，并让撤销注册的 inverse 进入 Episode accumulator、
   持有 endpoint Arc；
5. EffectRecord 直接进入 Armed，wait_forward 的结果为 Completed。

Fiber-wide topology withdrawal：

1. Provider 进入 Unloading 时，Core 立即从 store 删除 BindingRecord；
2. Actor 通知 Consumer；
3. withdrawal guard 等所有 committed Consumer 离开；
4. cleanup scheduler 到达该 seq 时，provide inverse 释放 Core 的 endpoint 引用。

手动 topology withdrawal：

1. cleanup scheduler 选择该 seq；
2. Actor 仅在 Slot 中仍是该 BindingId 时删除 BindingRecord 并通知 Consumer；若旧 Binding
   已撤回且 Slot 被新 Binding 占用，不删除新记录；
3. provide inverse 捕获精确 committed 到被撤回 BindingId 的 `(FiberId, EpisodeId)`；
   等待这些 Episode 结束，或仍在同一 Episode 且 Active、key 最新；
4. inverse 释放 Core 的 endpoint 引用。

这是软撤回边界：同一 Provider 恢复时，Consumer 可以在旧 Episode 内继续使用旧 endpoint
Arc。该等待不要求 Active Fiber 上无关的独立 Scope effect 完成；受影响 Fiber 自己的
activation failure 不改变 provide inverse 的结果。

Actor 通知全部 Slot watcher，但没有 committed 到该 Binding 的 watcher 不加入等待。
仅凭 watcher 建立等待边可能让两个从未互相使用服务的 Fiber 形成纯 Core 回收等待环。
捕获的 Episode 结束后，内部 Inactive、后续 Episode 或 Fiber 已移除都满足等待；后续
Episode 不延长旧 Binding 的等待。retired Consumer 完成自身 L-Unload 后，即使仍等待
children、公共状态仍为 Unloading，旧 Episode 也已结束。这不是
`FiberHandle::wait_settled` 的公共 settled 条件，也不等待 child O-Remove。

底层 service 资源由独立 effect 拥有。其 inverse 在 Fiber-wide cleanup 中按 seq 执行并完成异步关闭。

### mount

Core-generated EffectRecord label 为 `linorun:mount:{ComponentId}`。

forward：

1. 原子创建父级 mount EffectRecord/InverseEntry 和 child Fiber；
2. child 记录该 mount EffectId；
3. EffectRecord 直接进入 Armed；
4. 返回 FiberHandle。

inverse：

1. O-Retire child；
2. 清除 child.mount_effect；
3. 完成 mount Effect cleanup，不等待 child cleanup。

父级自然 cleanup 时按上述 inverse 执行。Host 调用 FiberHandle::dispose 时不安装第二个
inverse，而是直接执行同一 O-Retire reducer，并且只能把 action 精确为
`RetireFiber(child)` 的 Core-generated entry 被直接取出；它自己的 waiter 等
FiberCompletion 后才返回 DisposeReport。

## 10. Forward failure

forward 在尚未产生 inverse 前失败，表示该 step 没有留下已安装变化。

独立 Scope effect 在前几个 step 成功、后续 step 失败时：

- 当前失败 step 不产生 inverse；
- driver 进入 Disposing；
- 只选择 owner 为该 EffectId 的已收集 inverse；
- 这些 inverse 按 seq 降序执行；
- EffectHandle completion 保存 forward error 与 cleanup error；
- Fiber lifecycle 保持原状态。

Activation driver 在前几个 step 成功、后续 step 失败时：

- 已收集 inverse 保留在 Episode accumulator；
- 当前失败 step 不产生 inverse；
- activation 进入 L-Raise；
- accumulator 正常回收。

如果 forward 在外部产生了变化却无法返回 inverse，它违反 EffectContract。Core 无法自动修复。

## 11. Inverse failure

inverse 返回 CleanupError 或 unwind panic：

- 包装为带 owner/seq/label 的 CleanupFailure；
- inverse 视为已经尝试，不重试；
- 继续更早的 inverse；
- dispose report 聚合所有错误。

cleanup error 不阻止 lifecycle 到达其终局。

## 12. Effect API 约束

- Effect 只能通过当前 Fiber 的 Scope 创建；
- provide/mount 也必须归属 effect；
- Scope 只在 Reloading 或 Active 的当前 Episode 中接受新 effect；
- User inverse 不得调用 Scope/RootContext 启动 effect、provide 或 mount；这是一条可信组件的
  EffectContract，而不是 Core 额外维护的调用令牌。inverse 所需的临时异步工作必须在自身
  Future 返回前完成；
- inverse 不得 await 同一 Fiber 另一个普通 EffectHandle 的 dispose；同一 Fiber 的 scheduler
  正在等待当前 inverse，互等会构成组件级死锁。FiberHandle::dispose 走独立 O-Retire
  orchestration，但 inverse 也不得 await 自身或祖先 Fiber 的 dispose：自身移除等待当前
  inverse，祖先移除还等待当前 Fiber，都会形成组件自造的等待环；
- EffectHandle 不公开 InverseFn；
- Runtime snapshot 显示 label、state、seq 和 error；
- Drop EffectHandle 不等于 dispose，生命周期由 Runtime 持有。

`Scope::on_cleanup` 及 ComponentContext 的同名便捷方法为已取得的资源登记 inverse。
它们使用专用 `RegisterCleanup` 命令：shutdown 或 retire 已开始时，只要仍属于同一
Episode、尚未开始 unload cleanup，且 activation 或 effect step 仍在运行，登记仍可完成。
owner 已在 Unloading 时，该登记立即加入待回收集合。过期 Scope 仍被拒绝，新的 effect、
provide 和 mount 仍被拒绝；该例外只保证已有 step 落地时的资源清理不会丢失。

`Activation::run` 把整个 async body 作为一个 Once step。`on_cleanup` 登记参与相同的 seq
与 accumulator，但不会切分这个 activation step；Target 变化在整个 body 返回后才决策。
需要在 activation 各步骤之间检查 Target 时，使用 `Activation::steps`。

公共 `effect::sync`、`effect::once`、`effect::iter` 和 `effect::steps` 构造器分别适配同步
单步、Future、Iterator 和 Stream；`effect::inverse` 把 cleanup closure 装箱为 InverseFn。
这些构造器只建立 driver，forward closure 必须在 EffectStarted 已提交后由 RuntimeTask
调用。自定义执行形态实现 EffectDriver/IntoEffectDriver。
自定义 `into_driver` 也只能构造 driver，不得阻塞、panic 或提前执行外部变化。

## 13. 与 RAII 的关系

RAII 适合词法作用域资源；Linorun effect 适合动态、长生命周期、可手动卸载的组件资源。

内部实现用 RAII 管理锁、临时借用和 task-local 对象；组件外部 effect 的终止由 Fiber lifecycle 决定。
