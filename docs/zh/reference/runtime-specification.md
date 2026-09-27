[简体中文](runtime-specification.md) | [English](../../en/reference/runtime-specification.md)

# 运行时规范

- 适用版本：Core 0.1
- 规范级别：Normative
- 范围：单进程、可信组件、动态组合运行时

## 摘要

Linorun Core 是一个 Rust 组件运行库，管理组件依赖、异步生命周期和资源清理：

1. 组件产生的副作用必须带有 inverse，并由运行时跟踪；
2. 组件只在声明的依赖得到满足时激活；
3. 依赖变化按照 Provider Fiber 身份判断，而不是比较服务值；
4. 一次 activation 使用固定的 CommittedView；
5. Provider 退出解析后，先让 Consumer 撤销，再回收 Provider；
6. 异步步骤具有 inertia：已经开始的步骤必须落地，新目标在边界处理；
7. 每个 Fiber 同时最多执行一个生命周期转换。

本规范固定 Core 0.1 的数据结构、状态转移和七条不变量。

## 1. 设计约束

本规范定义 Core 0.1 的运行时契约。API 文档描述如何使用这些规则，架构文档说明实现方式，
一致性测试验证可观察行为。

- 组件必须显式声明服务依赖；父子关系不授予未声明的服务访问权限。
- Target 按 Requirement 顺序记录 Provider Fiber 身份，不比较服务值。
- 每个 Episode 固定保存依赖、配置和实现快照，直至该次清理完成。
- 已经开始的异步步骤必须落地；只在步骤边界处理新的 ActivationKey。
- Loading 期间目标短暂变化后恢复为相同 ActivationKey 时，继续当前 Episode。
- 同一 Fiber 的清理按登记逆序串行执行；Provider 整体清理必须等待已提交消费者离开。
- Context 派生保持不可变；服务隔离与调用元数据具有独立的规则。

## 2. 规范用语

- MUST：必须满足；
- MUST NOT：禁止；
- forward：产生一个可逆变化的操作；
- inverse：恢复该变化的操作；
- installed Fiber：处于 Reloading、Active 或 Unloading 的 Fiber；
- visible Binding：当前能被新 Consumer 解析的 Binding；
- committed view：某个 episode 开始时固定的依赖解析；
- settled Fiber：本 Fiber 已完成可立即执行的 lifecycle reconcile，公共状态为 Pending、
  Active、Failed 或 Disposed；它不要求独立 Scope effect 已完成；
- quiescent：所有 Fiber 都 settled；registry 中不存在 retired 但尚未移除的 Fiber，且没有
  lifecycle step、effect forward step、inverse、reconcile work 或待执行 O-Remove。

## 3. 目标

Core 0.1 MUST 支持：

- Component 定义与 Fiber 实例分离；
- Required dependency；
- 单 Provider Binding；
- 可逆 effect、手动 dispose 和 Fiber 级自动回收；
- 同步、异步和迭代式 effect；
- Provider/Consumer 有序卸载；
- immutable Context 派生；
- service isolation realm；
- interception metadata；
- 子 Fiber 挂载与退休；
- activation failure 后的局部恢复；
- Runtime shutdown；
- 可用于一致性测试的状态快照。

## 4. 语义内核

Core 0.1 由五个相互连接的机制组成：

- Episode accumulator 提供时间可组合性；
- reactive Target 驱动 Fiber lifecycle；
- CommittedView 固定一次 episode 的依赖；
- inertia 协调异步 step；
- withdrawal guard 排定 Consumer 与 Provider 的回收顺序。

## 5. 核心对象

### 5.1 Component

Component 是一个不可变定义，包含：

~~~rust
struct ComponentDescriptor {
    id: ComponentId,
    config_type: TypeId,
    requirements: Vec<Requirement>,
    provides: Set<ServiceId>,
}

struct Requirement {
    service: ServiceId,
    contract: ContractVTable,
    declared_intercept: Option<InterceptValue>,
}

struct InterceptValue {
    contract_type: TypeId,
    intercept_type: TypeId,
    value: Arc<dyn Any + Send + Sync>,
}

struct ContractVTable {
    contract_type: TypeId,
    intercept_type: TypeId,
    identity_intercept: fn() -> InterceptValue,
    merge_intercept: fn(
        &ServiceId,
        &InterceptValue,
        &InterceptValue,
    ) -> Result<InterceptValue, ServiceTypeMismatch>,
}
~~~

requirements 按 ServiceId 唯一，顺序稳定，并决定 Target 和 CommittedView 的顺序。
`None` 表示使用对应 ServiceContract 的 `identity_intercept()`。所有非空值必须由该
contract 的 Core 泛型 erasure 构造；descriptor 宏只调用 doc-hidden
`Requirement::new::<C>`。`Requirement.contract` 是 Core 生成的 erased vtable，携带
ServiceContract 与 Intercept 的 TypeId 以及 identity/merge 函数；InterceptValue 同时标记
来源 contract 和 Intercept 类型。Core 不把裸 `Arc<dyn Any>` 当作合法 intercept。

Descriptor 由宏建立为进程内唯一的 `'static` 实例；`config_type` 必须是
`TypeId::of::<C::Config>()`。Core 0.1 中所有 Requirement 都是 Required。Component 只能
provide descriptor 中声明的 ServiceId。

ComponentId 标识定义，不标识运行实例。FiberId 标识实例。

### 5.2 Fiber

Fiber 是 Component 的一次实例化：

~~~rust
struct FiberRecord {
    parent: Option<FiberId>,
    mount_effect: Option<EffectId>,
    children: Set<FiberId>,
    descriptor: &'static ComponentDescriptor,
    implementation: Arc<dyn DynComponent>,
    config: ErasedConfig,
    route: Arc<ContextRoute>,
    lifecycle: Lifecycle<EpisodePayload>,
    effects: Map<EffectId, EffectRecord>,
    next_inverse_seq: u64,
    cleanup_running: Option<RunningInverse>,
    unload_started: bool,
    cleanup_errors: Vec<CleanupFailure>,
    episode_cleanup_errors: Vec<CleanupFailure>,
    completion: Arc<FiberCompletion>,
}
~~~

FiberId 是 Runtime-local identity，在一个 Runtime 生命周期内单调分配且不复用。

`Lifecycle.revision` 用于配置更新、实现替换和显式 restart。它不是 Target 的组成部分，但参与
ActivationKey。FiberId 是 registry 的 map key，不在 record 内重复保存。

### 5.3 Runtime

Runtime 保存 Fiber registry、Binding store 和 dependency watcher：

~~~rust
struct RuntimeCore {
    fibers: Map<FiberId, FiberRecord>,
    bindings: Map<BindingSlot, BindingRecord>,
    contracts: Map<ServiceId, ContractVTable>,
    watchers: Map<BindingSlot, Set<FiberId>>,
    consumers_by_provider: Map<FiberId, Set<FiberId>>,
    default_realm: RealmId,
    identity_ids: IdentityCounters,
    sequence_ids: SequenceCounters,
    root: FiberId,
}
~~~

Runtime 使用一个逻辑 Actor 作为唯一控制面所有者，生命周期状态修改全部经过该 Actor 串行化。

`contracts` 固定一个 Runtime 内 `ServiceId -> ServiceContract TypeId/vtable` 的唯一映射。
mount 暂存 descriptor Requirement 及其相关 route intercept 校验；provide 暂存泛型
ServiceContract identity。两者再把缺少的映射与 Fiber/Effect/Binding 变更在同一
Actor 提交中写入；相同 ServiceId 对应不同 TypeId 时返回 ServiceTypeMismatch，且本次命令
不改变 RuntimeCore。provide 不读取与该 Binding 无关的 route intercept。映射在 Runtime
生命周期内单调增加，不随 Fiber/Binding 删除；L-Begin 因而只处理已验证的 endpoint、
Requirement metadata 和相关 route intercept。

Runtime 不得在持有控制面可变借用或锁时调用或等待组件代码、endpoint 或 inverse。
从控制面移出的 endpoint、Config、implementation、driver continuation 和用户 inverse
先进入 post-commit deferred-drop 列表，在释放 RuntimeCore 借用后析构。Drop 必须非阻塞；
且不得 panic；需要等待的资源关闭属于 inverse。

Runtime 创建时安装一个 synthetic Root Fiber：

- Root 使用保留的 FiberId；
- Root 使用保留 ComponentId `linorun/root@1`、unit Config 和空 requirements/provides 的
  synthetic descriptor；RootContext::provide 的 Host exemption 不读取该 provides 集合；
- 初始 state 为 Active；
- RuntimeState == Running 时，Root 的 latest ActivationKey 固定为
  `Target { providers: [] }, revision 0`；ShuttingDown/Stopped 时固定为 None；
- 初始 Episode 使用该空 key、空 Requirement 和空 CommittedView；
- RootContext 的 provide、mount 和 effect 归属 Root Episode accumulator；
- shutdown 使 Root 进入 Unloading，并通过相同的 withdrawal 与 cleanup 规则结束；完成后
  Root 保持 Inactive/Pending，不执行 L-Begin，也不 O-Remove。

保留值固定为 FiberId(0)、EpisodeId(0) 和 default Realm local 0；对应 identity counter 从 1
开始。snapshot/event sequence 也从 1 开始。

### 5.4 标识符

Core 语义只要求以下标识：

~~~rust
struct FiberId(u64);
struct EpisodeId(u64);
struct EffectId(u64);
struct BindingId(u64);
struct StepId(u64);
struct ComponentId(Arc<str>);
struct ServiceId(Arc<str>);
struct RealmId {
    runtime: Arc<RuntimeTag>,
    local: u64,
}

struct IdentityCounters {
    fiber: u64,
    episode: u64,
    effect: u64,
    binding: u64,
    step: u64,
}

struct SequenceCounters {
    snapshot_sequence: u64,
    event_sequence: u64,
}
~~~

EpisodeId、EffectId 和 BindingId 是 Runtime 内部诊断与异步 completion 关联身份。它们不参与 Target equality。RealmId 是绑定到所属 Runtime 的 opaque handle。

Fiber/Episode/Effect/Binding/Step 身份和序列号由 Actor 内的单调计数器分配且不复用。
default Realm 使用 local 0；RootContext::create_realm 通过 RuntimeHandle 的 AtomicU64 从 1
开始同步分配，不修改 RuntimeCore。RootContext/Context::default_realm 从同一 RuntimeTag
纯构造 local 0，供派生 route 显式重设回默认 Realm。ComponentId 与 ServiceId 是稳定的
字符串名称，不属于该 u64 空间。

## 6. LifecycleState

生命周期使用四个内部状态：

~~~rust
struct Lifecycle<P> {
    retired: bool,
    revision: u64,
    state: LifecycleState<P>,
}

enum LifecycleState<P> {
    Inactive {
        failure: Option<ActivationFailure>,
    },
    Reloading {
        episode: Episode<P>,
        step: StepId,
    },
    Active {
        episode: Episode<P>,
    },
    Unloading {
        episode: Episode<P>,
        outcome: UnloadOutcome,
    },
}

enum UnloadOutcome {
    Normal,
    ActivationFailed {
        failure: ActivationFailure,
        revision: u64,
    },
}
~~~

`FiberRecord.cleanup_running` 属于 FiberRecord，而不是 Unloading 变体。手动 effect dispose、
手动 provide dispose 和 Fiber-wide unload 都通过这一个串行 scheduler 运行 inverse。
EffectState::Disposing 表示单 effect 请求，LifecycleState::Unloading 表示整个 Episode 请求。

公共 API 按以下优先级投影；已移除和 retired 条件先于普通 Inactive 投影：

| 内部状态 | 公共状态 |
|---|---|
| 已从 registry 移除 | Disposed |
| Inactive、retired、仍在 registry | Unloading |
| Inactive，无 failure，未 retired | Pending |
| Reloading | Loading |
| Active | Active |
| Unloading | Unloading |
| Inactive，有 failure | Failed |

`Inactive { failure: Some(_) }` 即使已 retired 也先按 retired 规则显示为 Unloading，直到
它从 registry 移除。Failed 不会自动重试，但属于 settled 状态，也可以存在于 quiescent
Runtime。

Disposed 不是第五个运行状态，而是 Fiber 完成退休并从活动 registry 移除后的终局。

### 6.1 Episode

~~~rust
struct Episode<P> {
    id: EpisodeId,
    key: ActivationKey,
    committed: Arc<CommittedView>,
    payload: P,
}

struct EpisodePayload {
    implementation: Arc<dyn DynComponent>,
    config: ErasedConfig,
    inverses: Vec<InverseEntry>,
}
~~~

同一个 Episode 从 Reloading 延续到 Active，再延续到 Unloading。进入 Inactive 后 Episode 被丢弃；下一次 Reloading 创建新的 EpisodeId、CommittedView 和 accumulator。

Component implementation 与 Config 遵守不可变 snapshot 契约；外部别名不得通过 interior
mutability 绕过 replace/update 改变它们的 activation 语义。

## 7. Target、ActivationKey 与 CommittedView

### 7.1 BindingSlot

~~~rust
struct BindingSlot {
    service: ServiceId,
    realm: RealmId,
}
~~~

同一个 Slot 同时最多有一个已注册 BindingRecord。BindingRecord 从 store 删除后，Slot 立即可以再次注册。

### 7.2 BindingRecord

~~~rust
struct BindingRecord {
    id: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    owner_effect: EffectId,
}
~~~

Binding 对新 Consumer 可见，当且仅当记录仍在 store、Provider Fiber 为 Active 且未 retired。

Reloading Fiber 已创建的 Binding 对外不可见。Provider 进入 Unloading 时，Core 从 store 删除它注册的 BindingRecord；手动 dispose provide effect 时，Core 删除对应 BindingRecord。CommittedView 自己持有 endpoint Arc，因此旧 Consumer 仍能完成当前 episode，同时 Slot 已可注册新的 Binding。

### 7.3 Target

Target 计算规则：

1. retired Fiber 的 Target 为 None；
2. 按 requirements 顺序解析每个 BindingSlot；
3. 每个 Slot 必须存在 contract_type 相同的 visible Binding；
4. 任一 Requirement 不满足则 Target 为 None；
5. 否则 Target 为 Provider FiberId 序列。

~~~rust
struct Target {
    providers: Vec<FiberId>,
}
~~~

Target equality 只比较 providers。它不得比较：

- BindingId；
- EpisodeId；
- endpoint 地址；
- 服务业务值；
- Rust PartialEq 结果；
- interception metadata。

### 7.4 ActivationKey

~~~rust
struct ActivationKey {
    target: Target,
    revision: u64,
}
~~~

revision 表示配置更新、实现更新和显式 restart。依赖变化由 Target 表达，非依赖重启由 revision 表达。

### 7.5 CommittedView

~~~rust
struct CommittedView {
    key: ActivationKey,
    entries: Vec<CommittedEntry>,
}

struct CommittedEntry {
    binding: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    declared_intercept: Option<InterceptValue>,
}
~~~

CommittedView 在 L-Begin 时一次性构造，并在整个 episode 内保持不变。

Consumer 在 Reloading、Active 和 Unloading 中访问 declared service 时，只能读取该 CommittedView，不重新查询当前 Binding store。

## 8. Effect 模型

### 8.1 Effect

一个 effect 是 forward 和它产生的 inverse：

~~~text
forward(context) -> updated context + inverse
inverse(updated context) -> recovered context
~~~

运行时无法证明 inverse 正确，只负责跟踪、排序和调用。

### 8.2 EffectRecord

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

struct RunningMeta {
    episode: EpisodeId,
    seq: u64,
    owner: InverseOwner,
    label: Option<Arc<str>>,
}

enum InverseOwner {
    Activation,
    Effect(EffectId),
}

enum EffectState {
    Applying,
    Armed,
    Disposing,
}

enum RunningInverse {
    User(RunningMeta),
    Withdraw {
        meta: RunningMeta,
        endpoint: Arc<dyn ErasedEndpoint>,
        consumers: Vec<(FiberId, EpisodeId)>,
    },
}
~~~

Episode accumulator 是 InverseEntry 的唯一所有者。Activation step 产生的 entry 标记为
Activation；Scope effect 产生的 entry 标记为对应 EffectId。EffectRecord 只记录 driver
状态和共享 completion；手动 dispose 按 InverseOwner 从 accumulator 选择 entry。

用户 activation/effect 产生 User action。Mount 产生 RetireFiber；provide 产生
WithdrawBinding。accumulator 中的 entry 都尚未执行；Actor take action 后，跨 turn 的 User 或
Withdraw 状态移入 `cleanup_running`，完成后不放回 accumulator。
RetireFiber 是同步 reducer action；WithdrawBinding 由 Actor 删除 topology、通知相关 watcher，
并只等待精确 committed 到该 BindingId 的 Consumer Episode，再释放 endpoint；删除只在 Slot 当前 BindingId 与
action 匹配时发生，不能删除替代 Binding。两者不执行用户代码。

每当同步 iterator 或 async iterator yield 一个 inverse，Runtime 为其分配该 Fiber 内单调递增的 seq，并追加到 episode accumulator。
Actor 接受 Yield 的提交点就是 Core 可观察的 application linearization point；多个并发
forward completion 按 Actor 提交顺序获得 seq。组件必须在变化可观察后立即返回/yield
inverse，不能在变化与 Yield 之间插入无关 await，否则它自行破坏 application-order 契约。
同一 Fiber 上并发进行、且外部变化不满足交换律的 effects 必须由组件用 `wait_forward` 等
程序顺序串行化；Core 无法从 Future 内部观察更早的真实世界写入顺序。

Scope 在 Reloading 和 Active 中接受属于当前 Episode 的 effect。Fiber 进入 Unloading 时，
Scope 停止接受新 effect，并把全部 live Applying/Armed EffectRecord 切换为 Disposing；其中
Applying driver 只允许当前 step 落地，不再继续。

`on_cleanup` 通过专用 `RegisterCleanup` 命令为已取得的资源登记 inverse。关闭或 retire 已开始
时，只要登记仍属于同一 Episode、该 Episode 尚未开始 unload cleanup，且 activation 或
effect 仍有 in-flight step，登记仍可完成。这是已有 step 的收尾，不允许启动新的外部工作；
过期 Episode 的 Scope 仍被拒绝。登记沿用相同的 seq、accumulator 和 cleanup scheduler，
owner 已进入 Unloading 时立即加入待回收集合。

### 8.3 支持的 effect 形态

语义上支持：

- 同步返回一个 inverse；
- Future 返回一个 inverse；
- Iterator 依次产生多个 inverse；
- AsyncIterator 或 Rust 等价的 step stream 依次产生多个 inverse。

Activation 公开 `Once`/`Steps` 两种输入；Scope effect 通过 `IntoEffectDriver` 接受 once、
steps 和同步 adapter。Actor 内部都归一化为 Yield/Done step 事件，但二者保留各自的错误类型。

每个 step task 独占 driver，并且一次只 poll 一个 step。task 落地时把事件和仍存在的
driver 一起移交 Actor：

~~~rust
struct ActivationCompletion {
    result: Result<DriverEvent, ActivationFailure>,
    driver: ActivationDriver,
}

struct EffectStepCompletion {
    result: Result<DriverEvent, EffectError>,
    driver: ErasedEffectDriver,
}
~~~

`Yield { has_next: true }` 表示返回的 driver 是可继续的 continuation；final Yield、Done 和
Err 返回的 driver 只用于 post-commit drop，禁止再次 poll。Actor 接受 completion 后，或者
把可继续 driver 移入下一 RuntimeTask，或者在提交状态后析构；它不凭一个布尔值重新构造
driver，也不让 task 自行越过 Actor 决策点 poll 下一步。Actor 只在
`LifecycleState::Reloading.step` 或 `EffectRecord.running_step` 保存 completion identity，
不与 task 重复持有 driver。

Actor 在同一提交中写入 StepId 并把 driver 转交给 RuntimeTask 时，该 step 在语义上
已经开始；TaskSpawner 尚未对它执行第一次物理 poll，不形成可取消的中间态。之后才提交的
Target change 或 dispose 必须等待这个 step 落地。只有 completion 回到 Actor、且 Actor 尚未
转交下一 RuntimeTask 时，driver 才处于 boundary；Actor 在该 turn 内完成继续或停止决策，
不暴露可被其他 command 插入的“无 in-flight Reloading”状态。

provide 和 mount 是 Actor 内原子提交的 Core-owned effect：它们直接分配 inverse seq、建立
EffectRecord 与 topology，不创建用户 EffectDriver 或 StepId。提交成功时 forward 已自然
完成，EffectRecord 直接为 Armed；它们仍使用同一 accumulator、LIFO scheduler 和
EffectCompletion，而不是另一套 cleanup 机制。

普通 async Future 是一个粗粒度步骤。目标在 Future 进行中变化时，Future 仍然完成；Runtime 收集它最终产生的 inverse，然后在边界决定 commit 或 divert。

迭代式 effect 在每次 yield 后形成边界。目标变化后：

- 尚未开始的下一 iteration 不再执行；
- 已经开始的异步 iteration 必须完成；
- 它完成后产生的 inverse 必须先加入 accumulator；
- 随后进入 Unloading。

如果目标在当前 step 落地前恢复为原 ActivationKey，则当前 episode 按该 step 的 continuation 继续。这就是步骤边界上的 inertia 规则。

独立 Scope effect 的 forward step 失败时，Runtime 停止该 driver、按 seq 降序撤销它已经产生的 inverse，并把错误写入 EffectHandle 的 completion。Fiber lifecycle 保持不变；activation 代码显式传播该错误时，才由 activation failure 触发 L-Raise。

### 8.4 手动 dispose

EffectHandle::dispose 是幂等操作：

- 第一次调用请求停止后续 iteration 并撤销该 effect 已产生的 inverses；
- 当前正在执行的 iteration 仍然落地；
- inverse 按 seq 降序执行；
- 后续 dispose 等待同一个结果；
- Fiber unload 不得再次调用已完成 inverse。

Fiber 处于 Reloading 或 Active 时，手动请求可以在对应 effect driver 到达边界后运行；Fiber
一旦进入 Unloading，所有尚未开始的手动 cleanup 请求只排队，必须等 L-Unload 的 forward
boundary 与 withdrawal guard 同时满足后，才可由同一 scheduler 选择。手动 API 不能绕过
Provider/Consumer 顺序。

当一个已请求回收的 EffectRecord 已到 forward boundary，且它在 accumulator 或
cleanup_running 中都没有 entry 时，Actor 发布共享 completion，并在同一 turn 从 live
EffectRecord 中移除；这也
覆盖从未产生 inverse、或 forward 在第一个 Yield 前失败的 effect，不会虚构一个 inverse。
已有 entry 全部完成并被取出后使用同一规则。
EffectHandle 通过共享 completion 返回同一最终结果。

### 8.5 Episode accumulator

Fiber unload 使用 episode accumulator。每次选择当前尚未执行的最大 seq，并等待该 inverse 完成后再选择下一个。

Fiber-wide cleanup 在所有 EffectRecord 的当前 step 落地、且它们都停止产生新 inverse 后开始。这样 cleanup 集合和 application order 在第一次 inverse 前已经固定。

inverse 返回错误或 panic 时：

- 记录错误；
- 从 `cleanup_running` 清除该 inverse；
- 继续更早的 inverse；
- 不自动重试 inverse。

## 9. Context、isolate 与 intercept

### 9.1 Context

Context 是不可变、廉价克隆的派生值：

~~~rust
struct Context {
    runtime: RuntimeHandle,
    fiber: FiberId,
    episode: EpisodeId,
    route: Arc<ContextRoute>,
    committed: Arc<CommittedView>,
}
~~~

字段保持私有。Context clone 不创建新 Fiber、Binding 或 effect。

Context 负责读取 service 和派生 route；Scope 负责 effect、provide 和 mount 等变更。
Context 的只读 lookup 使用它保存的 CommittedView，不同步查询 Actor。组件不得在所属
Episode 完成后继续使用 Context 或由它创建的 typed service client；Scope 的异步变更接口会校验
Runtime/Fiber/Episode 归属并返回 InactiveContext。

### 9.2 isolate

~~~rust
struct ContextRoute {
    parent: Option<Arc<ContextRoute>>,
    isolate_delta: Map<ServiceId, RealmId>,
    intercept_delta: Map<ServiceId, InterceptValue>,
}
~~~

Context::isolate 返回新的 route 节点。它不修改原 Context，也不立即改变现有 Fiber 的 Target。
当前 Fiber 的 service lookup 仍读取既有 CommittedView；新的 isolate 只影响使用该派生
Context 执行的后续 mount/provide。要从另一 Realm 读取 service，必须让对应 child Fiber 在
该 route 下 mount，并由它自己的 Requirement 提交该 Realm 的 Binding。
`default_realm()` 返回同 Runtime 的 local 0 RealmId；把它传给内层 isolate 会覆盖外层 route
中的同 ServiceId Realm。

在派生 Context 上 mount 的 Fiber 使用派生 route 解析 Requirement。Scope::provide 也显式接收 Context，并使用该 Context 的 route。

### 9.3 intercept

Context::intercept 返回带 metadata overlay 的新 Context。

intercept：

- 可以影响 service 调用上下文；
- 不改变 BindingSlot；
- 不改变 Target；
- 不触发已有 Fiber reload。

metadata 的合并规则由 ServiceContract 定义。Core 只保留不可变值并按 Context 层级传递。
`identity_intercept` 必须是 `merge_intercept` 的左右单位元，merge 必须满足结合律；有效值
按 identity、declared metadata、outer route 到 inner route 的顺序 fold。

### 9.4 Service confinement

`Context::service` 只解析当前 Fiber Episode 的 CommittedView。未声明的 service 返回
`ServiceAccessError::UndeclaredDependency`；Core 不沿 parent Fiber 或 mount Context 隐式
查找 capability。每个 service read 都必须有对应的 Requirement，组件只能读取声明的服务依赖。

Fiber 的 `parent` 仍然只表示 mount/removal 所属关系，不是 service lookup 链。

## 10. Provider withdrawal

### 10.1 installed 与 relied

Reloading、Active 和 Unloading Fiber 都是 installed。

一个 Provider 被依赖，当存在 installed Consumer 的 CommittedView 指向它：

~~~text
relied(provider) =
    exists consumer:
        consumer.id != provider
        and consumer is installed
        and some consumer.committed entry.provider == provider
~~~

RuntimeCore 通过 consumers_by_provider 维护该关系；其真值由 installed CommittedView 定义。

### 10.2 Leave

Provider 从 Active 离开时，必须在开始 inverse 前：

1. 状态变为 Unloading；
2. 从 store 删除自己注册的 BindingRecord，使 Slot 可立即复用；
3. 通知相应 watcher 并重新计算 Consumer Target。

Consumer 发现 Target 不同后进入自己的 Unloading。Consumer cleanup 仍使用旧 CommittedView。

### 10.3 Withdrawal guard

Provider 只有在不再被任何 installed Consumer relied，且当前 episode 的所有 forward/effect driver 都已到达边界时，才能开始自己的 accumulator。

这里的“开始 accumulator”包括任何此前请求但尚未开始的手动 Effect cleanup；进入
Unloading 后不存在绕过该 guard 的第二条 inverse 启动路径。

手动 dispose provide effect 时，cleanup scheduler 选择该 provide inverse 后，Actor 仅当
该 Slot 当前记录的 BindingId 仍等于 inverse 携带的 BindingId 时删除它；已经撤回的旧
Binding 不得删除同 Slot 的替代 Binding。Actor 刷新该 Slot 的全部 watcher，但只捕获
CommittedView 中持有被撤回 BindingId 的 installed Consumer，记录其 `(FiberId, EpisodeId)`。
watcher 仅表示可能受拓扑变化影响，不能单独成为 withdrawal 等待边；否则从未使用该 Binding
的 Fiber 也可能与 Provider 形成纯 Core 回收等待环。

对每个捕获的 Consumer，以下任一条件结束等待：捕获的 Episode 已结束（包括内部 Inactive、
已进入后续 Episode 或 Fiber 已移除）；或仍在该 Episode、已 Active 且 episode.key 等于
最新 ActivationKey。后续 Episode 不延长旧 Binding 的等待。它不采用
`FiberHandle::wait_settled` 的公共 settled 条件，也不等待 child O-Remove 或 Active Fiber
上无关的独立 Scope effect；retired Consumer 完成自身 L-Unload 后，即使还在等待 children，
也已满足旧 Episode 结束的条件。所有捕获的 Consumer 都满足条件后释放 Core 的 endpoint 引用。
若同一 Provider 很快重新提供同一 Slot，
Consumer 的 ActivationKey 可以恢复并继续旧 Episode；此时旧 CommittedView 仍持有旧 endpoint。
Provider 的其他 Binding 和 effect 保持不变。Fiber-wide unload 已在进入 Unloading 时完成
Binding topology removal，随后由 withdrawal guard 等待所有 committed Consumer 真正离开。

## 11. 状态转移

以下规则是 Core 0.1 的规范状态机。

### 11.1 O-Insert

Host 或父 effect 创建 Fiber：

1. 校验 descriptor 是 `'static` 实例且 config_type 与 Component::Config 一致，并暂存
   Requirement contract mapping，以及这些 Requirement 对应的 route intercept 校验；任一失败则整个命令不改变
   RuntimeCore；
2. 分配 FiberId、mount EffectId 和父 Fiber 的新 inverse seq；
3. 在父 Episode 中写入 Armed mount EffectRecord 与 pending accumulator entry；
4. 写入 child 的 parent、mount_effect、descriptor、implementation、config 和 route；
5. retired 为 false，activation_revision 为 0，next_inverse_seq 为 1；
6. lifecycle 为 Inactive、无 failure，`cleanup_running` 为 None；
7. children/effects 为空并创建 FiberCompletion；
8. 加入 parent.children（Root child 也更新 Root 的 children）；
9. 注册 requirement watcher；
10. reconcile。

挂载 child 本身是父 Fiber 的 effect。未 retired 的普通 Fiber 保存父级 `mount_effect`；
synthetic Root 和已 retired Fiber 的该字段为 None。父级自然 cleanup 时，mount inverse
执行 O-Retire。

### 11.2 O-Retire

O-Retire 可由 mount inverse 或 Host 的 `FiberHandle::dispose` 触发：把 child 的 retired 设为
true、清除 child.mount_effect。Host 路径只能满足仍为 Armed、且 action 精确等于
`RetireFiber(child)` 的对应 Core-generated mount InverseEntry；Actor take 该 entry 并请求
移除 EffectRecord。因为同一个 O-Retire 结果已由 Host orchestration 建立，不再调用
该 InverseAction。它不能据此跳过其他 inverse。若父 scheduler 已先选择该 entry，则同步
RetireFiber 已在同一 Actor turn 完成，Host 只等待 FiberCompletion。两条入口共享同一个
reducer。O-Retire 不直接删除 Fiber，也不等待或跳过
child cleanup；FiberHandle waiter 和 parent 的 O-Remove 条件分别等待
FiberCompletion/children 清空。

retired 使 Target 立即变为 None。

### 11.3 O-Remove

当 Fiber：

- retired；
- Inactive；
- 没有 child；
- 没有未完成 effect 或 Binding；

Runtime 将其从 registry 移除并对外报告 Disposed。
移除前，Actor 向 FiberCompletion 发布 Disposed 和最终 DisposeReport；FiberHandle 因而不需要
Runtime 保留 tombstone record。

### 11.4 L-Begin

前提：

- Fiber 为 Inactive 且没有 failure；
- `cleanup_running` 为 None；
- 当前 ActivationKey 为 Some。

动作：

1. 捕获当前 visible Binding；
2. 构造 CommittedView，保留已验证的 declared interception metadata；
3. 创建 Episode；
4. accumulator 为空；
5. 状态进入 Reloading；
6. 构造不调用用户代码的 Init driver，并在提交后启动 activation 的第一个 step。

Init driver 持有 implementation、ComponentContext 和 config；第一次 RuntimeTask poll 才调用
`Component::activate` 并安装 Once/Steps driver。调用前先把 wrapper 置为 Exhausted，因此该
方法同步 panic 时仍能把有效 wrapper 随 ActivationFailure completion 归还 Actor。

### 11.5 L-Iter

前提：

- Fiber 为 Reloading；
- 当前 ActivationKey 等于 episode.key；
- completion 为 `Yield { inverse, has_next: true }`，并归还该 activation driver。

动作：

1. inverse 加入 accumulator；
2. completion 返回的 driver 作为 continuation 移入带新 StepId 的 RuntimeTask；
3. 保持 Reloading；
4. 调度下一 step。

### 11.6 L-Finish

前提：

- Fiber 为 Reloading；
- 当前 ActivationKey 等于 episode.key；
- completion 为 final `Yield { inverse, has_next: false }`，或 `Done`。

动作：

1. completion 携带 inverse 时将其加入 accumulator；
2. 状态进入 Active；
3. Provider 已注册 Binding 因 Provider Active 而变为 visible；
4. 通知 watcher。

### 11.7 L-Divert

前提：

- Fiber 为 Reloading；
- 一个成功 completion 已返回 Actor；
- 当前 ActivationKey 与 episode.key 不同。

Runtime 先收集 completion 携带的 inverse，再提交 Unloading，并在 Actor 外释放返回的
driver；即使 `has_next: true` 也不调度下一 RuntimeTask。step 从 StepId 与 task ownership
提交时已经开始，
因此 Target 在执行器首次 poll 前变化也不能撤销该 step；step error 始终按 L-Raise 处理。

Runtime 不得短暂进入 Active。

进入 Unloading 时删除本 Fiber 已注册的 BindingRecord 并刷新相应 watcher，同时把本 Episode
全部 live EffectRecord 标记为 Disposing；Applying driver 的当前 step 仍按 inertia 落地。

### 11.8 L-Raise

activation step 返回错误或可捕获 panic：

1. 在状态提交后释放 completion 返回的 activation driver；
2. 保留已经收集的 accumulator；
3. 进入 Unloading；
4. 删除本 Fiber 已注册的 BindingRecord 并刷新相应 watcher；
5. 本 Episode 的全部 live EffectRecord 标记为 Disposing；Applying driver 只完成当前 step；
6. outcome 记录 activation failure 和 `episode.key.revision`。

失败发生前已安装的 effect 必须先撤销。

### 11.9 L-Leave

前提：

- Fiber 为 Active；
- 当前 ActivationKey 与 episode.key 不同。

动作：

1. 进入 Unloading；
2. 删除本 Fiber 已注册的 BindingRecord；
3. 通知 watcher；
4. 全部 live EffectRecord 进入 Disposing，Applying driver 只完成当前 step；
5. 等待 in-flight step 落地和 withdrawal guard。

### 11.10 L-Unload

启动前提：

- Fiber 为 Unloading；
- 不再有 installed Consumer committed 到该 Provider；
- activation/effect forward driver 均已到达边界。

启动动作：

1. Unloading 状态本身请求整个 Episode cleanup；
2. cleanup scheduler 按 seq 降序从 accumulator take entry 并串行运行；
3. 回收期间 Fiber 保持 Unloading。

当 accumulator 为空、`cleanup_running` 为 None 且本 Episode 的 live EffectRecord 已清空时，
L-Unload 完成：

1. 清空已通过 InverseFinished 事件和适用 operation collector 暴露的 episode-local failures；
2. 从 consumers_by_provider 删除本 Consumer 的 committed dependency；
3. 清除 CommittedView 和 episode；
4. 进入 Inactive；
5. 仅当 outcome 中的 failure revision 仍等于 Fiber 当前 activation_revision 时保存 failure；
6. reconcile 最新 Target。

没有 failure 且最新 Target 存在时，在当前 reconcile 收敛过程中再次 L-Begin。

### 11.11 Restart 与 update

配置更新先替换 latest config snapshot；实现更新先替换 latest implementation snapshot；
restart 不改 snapshot。三者随后执行同一流程：

1. 增加 activation_revision，并清除已有 activation failure；
2. 若 Fiber 正处于 Unloading，令 outcome 中尚未提交的 activation failure 失效；
3. reconcile。

Active Fiber 因 ActivationKey 改变而 L-Leave；Inactive Fiber 在 ActivationKey 存在时 L-Begin。

descriptor 和 route 在一个 Fiber 的整个生命期内保持不变。`replace` 只接受返回同一
descriptor 静态实例、且 Config TypeId 匹配的实现。requirements、provides、Config 类型或
route 变化时，旧 Fiber retire，并创建新 Fiber。

## 12. Inertia 与异步协调

每个 Fiber 同时只能有一个 lifecycle transition。

逻辑 Actor 只在状态中记录当前 EpisodeId 和 step completion token。异步用户代码在控制面之外执行，完成后发送结果。

完成结果到达时：

- EpisodeId 匹配当前 Reloading/Unloading episode，才可推进；
- 已不存在的旧 episode completion 只做必要的局部资源释放，不得修改新 episode；
- Target 变化只更新最新意图，不并行启动第二个 transition。

Core 不通过丢弃 Future 假装回滚。一个永不结束的 step 会使 Fiber 保持 Loading 或 Unloading。

## 13. 七条核心不变量

### I1：Inverse 至多调用一次

每个成功收集的 inverse，在所有手动 dispose、Fiber unload、父级退休和 Runtime shutdown 的竞争轨迹中，调用次数不得超过一次。

Host 只可在自己已经提交同一 child 的 O-Retire 时，把精确匹配的 Armed
`RetireFiber(child)` entry 作为已满足而不调用；这不是跳过任意 inverse 的通道。

### I2：严格逆序、串行回收

同一 Fiber 同时最多运行一个 inverse。下一 inverse 必须是当前已请求回收且未完成集合中 seq 最大者。错误不会导致更早的 inverse 被跳过。

### I3：先 Leave，后 Provider cleanup

Fiber-wide unload 的第一个 inverse 开始前，Provider 必须已经不再 Active，其 BindingRecord
已从 store 删除，并已触发 watcher refresh。手动 provide dispose 选择 WithdrawBinding 时，
Actor 在发布 InverseStarted 前完成同样的 Binding topology removal。

### I4：先 Consumer，后 Provider

Provider 的 Fiber-wide accumulator cleanup 启动第一个 inverse 前，不得存在 CommittedView
仍指向该 Provider 的 installed Consumer。进入 Unloading 时尚未开始的手动 Effect cleanup
也并入该等待；若手动 inverse 已在 Active/Reloading 期间启动，则按 inertia 落地，L-Unload
不会并行启动第二个 inverse。

### I5：CommittedView 固定

一个 Episode 的 CommittedView 从 L-Begin 到 L-Unload 不得替换。Consumer cleanup 读取的仍是 activation 时提交的 endpoint。

### I6：Target 只比较 Provider 身份

Target 由 Requirement 顺序对应的 Provider FiberId 构成。BindingId、EpisodeId、服务值和 endpoint 地址不得参与 Target equality。

### I7：转换具有 inertia

每个 Fiber 同时最多一个 transition。已经开始的异步 step 必须落地；目标变化只在边界决定继续、divert 或串联下一 transition。

“已经开始”以 Actor 写入 StepId 并把 driver 转交 RuntimeTask 的提交为准，不以执行器首次
poll 为准；这个提交与上一 boundary 的决策不可被其他 command 拆开。

## 14. 派生约束

Core 0.1 还必须满足：

- Disposed Fiber 不重新进入 registry；
- 一个 BindingSlot 同时最多一个已注册 Binding；
- provide 和 mount 必须归属于某个 effect；
- Provider 只有在 Active 时对新 Consumer 可见；
- Failed Fiber 不自动重试，直到 restart、update 或 replace implementation；
- cleanup failure 被记录但不阻止其余 inverse；
- isolate 只改变派生 Context 的 Realm resolution；
- intercept 不改变 Target；
- Actor 或控制面不得 await 用户代码；
- User inverse 不得启动新的 effect、provide 或 mount；
- parent 只有在所有 child 被移除后才能 O-Remove；
- Fiber-wide cleanup 开始前，本 episode 的所有 effect driver 已停止产生新 inverse。

互需 Component 没有环外 visible Provider 时，环中的 Fiber 不能满足全部 Requirement，因此
保持 Pending，不会产生 committed dependency edge。Core 不在运行时打破或隐式改写依赖环；需要双向交互时，可用独立的 integration component 依赖双方并协调调用。已 committed 的依赖边不会形成环，
因此 withdrawal guard 不会因 committed dependency 环产生结构性死锁；它仍可能等待永不
结束的用户 Future。


## 15. Failure

### 15.1 Activation failure

Activation failure 属于单个 Fiber：

1. 进入 Unloading；
2. 撤销已完成步骤；
3. 到达 Inactive；
4. 保存 failure；
5. 对外显示 Failed。

Sibling 不受影响。

Failed Fiber 不因依赖抖动自动重试。Host 调用 restart、更新配置或替换实现时清除 failure。

### 15.2 错误数据结构

用户代码返回叶子错误，Runtime 为 cleanup 错误补充位置上下文：

~~~rust
enum FailureKind {
    User,
    Panic,
}

struct FailureCause {
    kind: FailureKind,
    message: Arc<str>,
}

struct ActivationFailure {
    cause: FailureCause,
}

struct EffectError {
    cause: FailureCause,
}

struct CleanupError {
    cause: FailureCause,
}

struct CleanupFailure {
    owner: CleanupOwner,
    fiber: FiberId,
    episode: EpisodeId,
    seq: u64,
    label: Option<Arc<str>>,
    cause: CleanupError,
}

enum CleanupOwner {
    Activation,
    Effect(EffectId),
}
~~~

`ActivationFailure`、`EffectError` 和 `CleanupError` 是叶子错误；`CleanupFailure` 是
带 CleanupOwner、Fiber、Episode、seq 和可选 label 的聚合记录。`RealmMismatch` 携带来自
另一 Runtime 的 RealmId。ServiceTypeMismatch 携带 ServiceId、Contract/Intercept channel
及 expected/actual TypeId，并由 MountError、ProvideError 或 ServiceAccessError 包装。

### 15.3 Cleanup failure

inverse 返回 `CleanupError` 或发生 panic：

- 包装为 `CleanupFailure`，记录 CleanupOwner、seq、可选 label 和错误；
- 继续其余 inverse；
- dispose/shutdown report 返回聚合结果；
- 不自动重试失败 inverse。

cleanup failure 不改变四态 lifecycle；外部状态是否真正恢复由组件契约负责。

EffectDisposeReport 只覆盖对应 EffectId。DisposeReport 覆盖目标 Fiber 从 O-Retire 到
O-Remove 的窗口内，目标及属于其 subtree 的 descendants 所产生的 cleanup failure。ShutdownReport 覆盖
Running -> ShuttingDown 到 Stopped 期间 Root 与全部非 Root Fiber。正常 reload 已发布的
历史 failure 不在后续报告中重复聚合。

### 15.4 panic

在 unwind 构建中，组件任务边界捕获 panic：activation panic 转为 ActivationFailure，独立 effect forward panic 转为 EffectError，inverse panic 转为 CleanupError，再包装为 CleanupFailure。

## 16. Runtime shutdown

shutdown 使用普通生命周期规则：

1. Runtime 拒绝新的 Root/Scope effect、mount、provide，以及 Fiber restart/update/replace；
2. synthetic Root 进入 Unloading，并撤回 Root Binding；
3. 依靠 withdrawal guard 和 Root Episode accumulator 的全局 LIFO 回收 Root effect；
4. 每个 mount inverse 按其 seq 执行 O-Retire；child cleanup 可以与 Root 更早 seq 的
   inverse 交错；
5. 等待所有非 Root Fiber Disposed，并完成 Root Episode；
6. 返回 cleanup failure 聚合。

拒绝新工作不丢弃正在落地的 step 所需的 cleanup 登记。第 8.2 节的 `RegisterCleanup`
收尾条件在 ShuttingDown 或 owner retired 后仍适用；旧 Episode 或已经开始 unload cleanup
的 Scope 不因此重新获得资源创建权限。

这是 graceful shutdown。Core 不丢弃仍在运行的用户 Future、inverse Future 或 continuation，
因此用户代码永不返回时，shutdown 可以永久 pending。

`RuntimeShuttingDown` 表示 RuntimeState 不是 Running，包括 Stopped。state/snapshot、
wait/dispose、重复 shutdown，以及 default_realm/create_realm/isolate/intercept 这类不修改 RuntimeCore 的
操作仍可调用。

## 17. Rust API 边界

公共面的完整签名与错误枚举见 [Core API 总览](../api/core-api.md)。以下代码固定主要类型
及其职责，不重复 snapshot DTO 的全部字段：

~~~rust
pub struct Runtime;
pub struct RuntimeBuilder;
pub struct RootContext;
pub struct Context;
pub struct Scope;
pub struct ComponentContext;
pub struct FiberHandle;
pub struct EffectHandle;
pub struct ComponentDescriptor;
#[doc(hidden)]
pub struct Requirement;
pub struct ServiceRef<C: ServiceContract>;
pub struct CallContext<I>;
pub trait ErasedEndpoint: Send + Sync + 'static { /* macro adapter */ }
pub struct RuntimeSnapshot;
pub struct RuntimeEvent;
pub enum RuntimeEventKind { /* see observability */ }
pub struct BindingEventRef;
pub type RuntimeTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
pub trait TaskSpawner: Send + Sync + 'static { /* non-blocking spawn */ }

pub enum FiberState {
    Pending,
    Loading,
    Active,
    Unloading,
    Failed,
    Disposed,
}

pub enum EffectForwardOutcome {
    Completed,
    Disposed,
}

pub enum Activation {
    Once(ActivationFuture),
    Steps(ActivationStream),
}

pub type ActivationFuture = Pin<Box<
    dyn Future<Output = Result<Option<InverseFn>, ActivationFailure>> + Send
>>;

pub type ActivationStream = Pin<Box<
    dyn Stream<Item = Result<InverseFn, ActivationFailure>> + Send
>>;

pub trait EffectDriver: Send + 'static { /* poll_step */ }
pub enum DriverEvent { /* Yield or Done */ }
pub trait IntoEffectDriver { /* adapter */ }

pub struct EffectDisposeReport;
pub struct DisposeReport;
pub struct ShutdownReport;
pub struct ActivationFailure;
pub struct EffectError;
pub struct CleanupError;
pub struct CleanupFailure;
pub enum CleanupOwner { /* Activation or Effect */ }
pub struct RealmMismatch { /* foreign RealmId */ }
pub enum ServiceTypeChannel { /* Contract or Intercept */ }
pub struct ServiceTypeMismatch { /* service/channel/expected/actual */ }
pub enum ServiceAccessError { /* see core-api */ }

pub trait Component: Send + Sync + 'static {
    type Config: Send + Sync + 'static;

    fn descriptor(&self) -> &'static ComponentDescriptor;
    fn activate(
        &self,
        ctx: ComponentContext,
        config: Arc<Self::Config>,
    ) -> Activation;
}
~~~

- Context：service lookup、isolate、intercept；`service::<C>()` 返回 `C::Client`；
- Scope：effect、provide、mount 和 cleanup 注册；
- ComponentContext：面向组件的统一入口，把 service 读取与资源操作委托给自身 Context/Scope；
- FiberHandle：state、wait_settled、restart、update_config、replace、dispose；
- EffectHandle：幂等 dispose；
- ServiceRef：只交给 contract adapter 的 committed endpoint 基础包装；
- Runtime：root operations、snapshot、shutdown。

`Activation::run`、`Activation::once` 和 `Activation::steps` 是既有 Once/Steps 执行形态的构造器。
ComponentContext 的便捷方法保留相同的声明依赖与 Episode 归属约束，不引入额外生命周期。
`Activation::run` 的整个 async body 是一个 Once step；body 内 await `on_cleanup` 只登记
inverse，不切分 activation boundary。Target 变化仍在整个 body 返回后决策；需要逐步检查
Target 时使用 `Activation::steps`。
这些编写入口的签名见 [Core API 总览](../api/core-api.md)。

每个 ServiceContract 声明一个由宏生成、定义在 contract 所属 crate 的 `Client` 类型。
Context 先构造字段私有的 ServiceRef，再由 contract adapter 包装为该 Client；调用方法定义在
Client 上，始终使用 committed endpoint 和构造它的 Context 所合并的 interception metadata。
这避免要求下游 crate 为 `linorun-core` 所拥有的泛型类型添加 inherent methods。

RuntimeSnapshot 及其 Fiber/Binding/Effect/Inverse DTO 是 public read-only 投影；内部
LifecycleState、EffectState、RunningInverse 与 accumulator entry 不因 snapshot 而公开。`Runtime::new(spawner)`
与 `Runtime::builder(spawner).build()` 都可构造 Runtime；`new` 委托同一 builder/build 路径。
`Runtime::builder` 必须接收 TaskSpawner，并可选接收一个标准库 `Sender<RuntimeEvent>`。`FiberHandle::wait_settled` 等待单个
Fiber 到达 settled 边界；`Runtime::wait_quiescent` 等待整个 Runtime 静止。详细内部 enum、
record 和 Actor command 是 crate-private 实现结构；`ErasedConfig`、`DynComponent`、
`ErasedEndpoint`、RuntimeHandle 和 completion cell 的最小定义见
[核心数据模型](../architecture/data-model.md#12-erased-implementation-types)。

## 18. 验收条件

Core 0.1 实现必须：

1. 通过七条不变量的自动化测试；
2. 通过 Effect、inertia、服务撤回、隔离和失败处理的场景测试；
3. 通过 L-Begin、L-Iter、L-Finish、L-Divert、L-Raise、L-Leave、L-Unload 的参考模型测试；
4. 证明 Target 不读取服务值；
5. 证明 cleanup 使用原 CommittedView；
6. 证明 Provider inverse 不早于 Consumer unload；
7. 证明异步目标抖动不会创建并行 transition。

## 19. 配套文档

- [Core API](../api/core-api.md)
- [架构总览](../architecture/overview.md)
- [七条不变量](invariants.md)
- [一致性测试策略](../testing/conformance-strategy.md)
- [场景目录](../testing/scenario-catalog.md)
