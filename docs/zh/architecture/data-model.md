# 核心数据模型

简体中文 | [English](../../en/architecture/data-model.md)

本文件展开 运行时规范 中的数据关系。字段表示语义，不固定 Rust 内存布局。

## 1. RuntimeCore

~~~rust
struct RuntimeCore {
    fibers: Map<FiberId, FiberRecord>,
    bindings: Map<BindingSlot, BindingRecord>,
    contracts: Map<ServiceId, ContractVTable>,
    watchers: Map<BindingSlot, Set<FiberId>>,
    consumers_by_provider: Map<FiberId, Set<FiberId>>,
    effect_owners: Map<EffectId, FiberId>,
    default_realm: RealmId,
    identity_ids: IdentityCounters,
    sequence_ids: SequenceCounters,
    root: FiberId,
}
~~~

RuntimeCore 只保存生命周期协调所需的主数据。

- fibers：Component 实例；
- bindings：当前已注册的 service；
- contracts：Runtime 内每个 ServiceId 唯一的 TypeId 与 erased vtable，运行期间只增不删；
- watchers：哪些 Fiber 的 Requirement 受某个 Slot 影响；
- consumers_by_provider：installed CommittedView 形成的 Provider/Consumer 边；
- effect_owners：EffectId 到所属 Fiber 的反向索引，与 Fiber.effects 在同一 Actor 提交中更新；
- default_realm：该 Runtime 中未被 isolate 覆盖的解析 Realm；
- identity_ids/sequence_ids：分别分配 Actor-owned Runtime-local identity 与 snapshot/event
  sequence；RealmId 使用 RuntimeHandle 的独立原子 allocator。

root 指向 synthetic Root Fiber。Root 初始为 Active，持有
`ActivationKey { target: Target { providers: [] }, revision: 0 }`、空 CommittedView 和
Root Episode accumulator；Host 的顶层 effect 都归属于该 Episode。
Root descriptor 使用保留 ComponentId `linorun/root@1`、unit Config 和空
requirements/provides；Host provide 是显式 exemption。
Root 使用 FiberId(0)、EpisodeId(0) 和 default Realm local 0；identity 与 sequence counter
从 1 开始。
Root 的 `mount_effect` 为 None；每个未 retired 的普通 Fiber 记录其父 Episode 中创建自己的
mount EffectId，O-Retire 时清为 None。

Root latest key 在 RuntimeState::Running 时固定为上述空 key，在 ShuttingDown/Stopped 时为
None。Root cleanup 完成后保持 Inactive，不重新 activation，也不从 registry 移除。

## 2. 身份

~~~rust
struct FiberId(u64);
struct EpisodeId(u64);
struct EffectId(u64);
struct BindingId(u64);
struct StepId(u64);
struct ComponentId(Arc<str>);
struct RealmId {
    runtime: Arc<RuntimeTag>,
    local: u64,
}
struct ServiceId(Arc<str>);

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

作用：

| 身份 | 作用 |
|---|---|
| FiberId | Provider identity 与实例索引 |
| EpisodeId | 一次 lifecycle episode 和异步 completion 关联 |
| EffectId | 手动 dispose、诊断 |
| BindingId | CommittedView 指向具体 Binding |
| StepId | 一次 activation 或 effect forward step completion |
| ComponentId | 稳定组件定义名称 |
| RealmId | Service isolation |
| ServiceId | 稳定 service contract 名称 |

只有 FiberId 进入 Target。其余 ID 不改变 dependency identity。

u64 identity 只在所属 Runtime 内解释。ComponentId/ServiceId 是跨 Runtime 稳定名称；
RealmId 是带 Runtime 归属的 opaque handle。

## 3. FiberRecord

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
    settled_waiters: Vec<Reply<FiberState>>,
    dispose_waiters: Vec<Reply<DisposeReport>>,
}
~~~

稳定字段：

- map key 对应的 FiberId、parent；
- descriptor；
- route；
- completion。

可变字段：

- implementation；
- config；
- mount_effect：O-Retire 时从父 Episode 的 EffectId 清为 None；
- lifecycle 中的 retired、activation revision 与 state；
- cleanup_running 与 cleanup reports；
- children；
- effects；
- next_inverse_seq。

descriptor 与 route 在 Fiber 生命周期内固定；改变它们时 retire 旧 Fiber 并 mount 新 Fiber。

## 4. LifecycleState

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

Reloading、Active 和 Unloading 统称为 installed，因为它们都持有 committed view。

Inactive 不持有 episode。Failed 是 Inactive 加 failure 的公共投影。

`cleanup_running` 常驻 FiberRecord，使 Active 状态下的手动 dispose 与 Unloading 状态下的整
Fiber cleanup 使用同一串行 inverse scheduler。EffectState::Disposing 表示单 effect 请求，
LifecycleState::Unloading 表示整个 Episode 请求，不保存第二份 request flag。

## 5. Episode

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

Episode 捕获 implementation 和 config，防止 update 影响进行中的 activation/cleanup。
这里固定的是不可变 value snapshot；Component/Config 的外部别名不得在 Core API 之外改变
其 activation 语义。

Episode 是否继续由 step boundary 的最新 ActivationKey 比较决定。

## 6. ComponentDescriptor

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
~~~

Requirement 顺序属于语义：

- ServiceId 在 requirements 中唯一；
- Target 第 i 项对应 Requirement 第 i 项；
- CommittedEntry 保持相同顺序；
- snapshot 与测试按该顺序输出。

ComponentDescriptor 是宏生成的进程内唯一 `'static` 实例；`config_type` 固定该逻辑组件
允许使用的 Config Rust 类型。实现替换必须复用同一实例。

descriptor 宏调用 public、doc-hidden 的 `Requirement::new::<C>`；该 Core 泛型构造器用
`ContractVTable::of::<C>()` 填入 contract，携带 ServiceContract 与 Intercept 的 TypeId 以及
erased identity/merge 函数；InterceptValue 的两个 TypeId
分别标识来源 contract 与值类型。这样 mount 不需要从 TypeId 反向恢复函数指针，也不会把
复用同一 metadata Rust 类型的不同 contract 混合。

## 7. Binding

~~~rust
struct BindingSlot {
    service: ServiceId,
    realm: RealmId,
}

struct BindingRecord {
    id: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    owner_effect: EffectId,
}
~~~

可见性是派生属性：

~~~text
visible(binding) =
    binding is registered
    and provider.state == Active
    and not provider.retired
~~~

- Reloading Provider 的 Binding 不可见；
- Active 且未 retired 的 Provider 的 Binding 可见；
- Provider 进入 Unloading 时，Core 从 store 删除记录；
- 手动 dispose provide effect 时，Core 从 store 删除对应记录；
- CommittedView 持有独立 endpoint Arc，旧 Consumer 不依赖 BindingRecord 存续。

## 8. Target 与 CommittedView

~~~rust
struct Target {
    providers: Vec<FiberId>,
}

struct ActivationKey {
    target: Target,
    revision: u64,
}

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

Target 表达“由谁提供”。CommittedView 还保存“具体读取什么”。

BindingId 出现在 CommittedView 是为了：

- 标识该 Episode 捕获的具体 provision；
- snapshot 和诊断；
- 区分同一 Provider 在同一 Slot 的先后 Binding。

它不参与 Target equality。

## 9. relied 关系

~~~text
relied(provider P) =
    exists installed fiber F:
        F.id != P
        and F.episode.committed contains entry.provider == P
~~~

依赖图不是 parent tree：

    Consumer Fiber
        -- CommittedEntry -->
    Binding
        -- provider -->
    Provider Fiber

实现维护 consumers_by_provider 反向索引；真值来源始终是 CommittedView。

## 10. EffectRecord

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
    forward_waiters: Vec<Reply<Result<EffectForwardOutcome, EffectError>>>,
    dispose_waiters: Vec<Reply<EffectDisposeReport>>,
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

enum EffectState {
    Applying,
    Armed,
    Disposing,
}

struct RunningMeta {
    episode: EpisodeId,
    seq: u64,
    owner: InverseOwner,
    label: Option<Arc<str>>,
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

Episode.accumulator 是 InverseEntry 的唯一所有者。InverseOwner::Activation 标识 activation
driver 的产物，InverseOwner::Effect(id) 标识 Scope effect 的产物。EffectRecord 不保存第二份
seq 索引；dispose 和 snapshot 直接按 owner 从 accumulator 过滤。

accumulator 中存在的 InverseEntry 都是 Armed。scheduler take entry 后，User/Withdraw 的
跨 turn 状态移入 FiberRecord.cleanup_running；完成后不再把 entry 放回 accumulator。
User action 在 Actor 外运行；RetireFiber 由 Actor 同步应用 O-Retire；WithdrawBinding 由
Actor 仅在 Slot 当前 BindingId 与 action 匹配时删除 Binding，并捕获 CommittedView 确实
持有该 BindingId 的 `(FiberId, EpisodeId)`。watcher 只用于定位可能受影响的 Fiber，不直接
产生清理等待。捕获的 Episode 离开 installed，或仍是同 Episode 且达到 Active、key 等于最新
ActivationKey 的边界后，释放 Core 的 endpoint 引用；后续 Episode 不延长此次等待。
同 Provider 重新发布同 Slot 可以恢复 Target，因此原 Episode 仍可持有旧 endpoint 并继续。
若旧 Binding 已撤回并被同 Slot 的新 Binding 替代，该 action 不得删除替代记录。
Core action 不调用组件代码。

seq 是 Fiber 局部 application order。EffectId 标识一次 effect 调用，seq 标识其中每个实际 inverse 的全 Fiber 顺序。

运行中的 inverse 由所属 Fiber map key 加 RunningMeta(episode, seq) 标识。

一个 effect iterator 可以产生多个 InverseEntry。EffectHandle dispose 选择 owner 为该 EffectId 的 entries；Fiber unload 选择整个 episode accumulator。

已请求回收的 EffectRecord 在 driver 到达 boundary、且 owner 对应的 entry 已不在
accumulator/cleanup_running 时，于同一 Actor turn 完成并移除。该条件对空 entry 集合成立，
因此一个没有产生 inverse 的 effect 也能完成 dispose 或 Fiber-wide cleanup；Runtime 不为它制造占位 entry。

## 11. ContextRoute

~~~rust
struct ContextRoute {
    parent: Option<Arc<ContextRoute>>,
    isolate_delta: Map<ServiceId, RealmId>,
    intercept_delta: Map<ServiceId, InterceptValue>,
}
~~~

route 是 immutable delta chain。解析从内向外查找最近 isolate；intercept metadata 按 ServiceContract 的合并规则组合。

Fiber route 在 mount 时固定。派生 isolate 只在用于后续 mount/provide 时产生 topology
效果；当前 Fiber 的 service lookup 仍读取既有 CommittedView。派生 intercept 会影响随后
从该 Context 构造的 typed client。

## 12. Erased implementation types

以下名称是 erased adapter 的最小类型；除明确标出的 adapter plumbing 外，均为
`linorun-core` crate-private，不是额外运行时机制：

~~~rust
type ErasedConfig = Arc<dyn Any + Send + Sync>;
trait DynComponent: Send + Sync + 'static { /* Component vtable */ }
pub trait ErasedEndpoint: Send + Sync + 'static {
    fn as_any(&self) -> &(dyn Any + Send + Sync); // doc-hidden adapter hook
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
struct RuntimeTag; // Runtime ownership tag for RealmId
struct RuntimeInner {
    runtime_tag: Arc<RuntimeTag>,
    default_realm: RealmId,
    next_realm: AtomicU64,
    sender: UnboundedSender<Command>,
}
struct RuntimeHandle(Arc<RuntimeInner>);
enum ActivationDriver {
    Init { /* implementation + ComponentContext + config */ },
    Once(ActivationFuture),
    Steps(ActivationStream),
    Exhausted,
}
type ErasedEffectDriver = Pin<Box<dyn EffectDriver>>;
struct ActivationCompletion {
    result: Result<DriverEvent, ActivationFailure>,
    driver: ActivationDriver,
}
struct EffectStepCompletion {
    result: Result<DriverEvent, EffectError>,
    driver: ErasedEffectDriver,
}
struct TurnOutput {
    tasks: Vec<RuntimeTask>,
    deferred_drops: Vec<Box<dyn Send + 'static>>,
    events: Vec<RuntimeEvent>,
}
struct EffectCompletion; // Completion cell shared by EffectHandle and Actor
struct FiberCompletion;  // Preserves Disposed/report for FiberHandle after O-Remove
struct ServiceRefInner<C: ServiceContract>; // Private state of the committed call base
~~~

ErasedEndpoint、ServiceRef、CallContext、Requirement 和 ComponentDescriptor constructor
是 public opaque 或 doc-hidden macro adapter plumbing；普通组件得到并使用的是
contract-local `C::Client`。InterceptValue 与 ContractVTable 留在 Core 内部，其余本节类型
也是 crate-private。

一个 step task 独占 ActivationDriver 或 ErasedEffectDriver；Actor 只在
LifecycleState::Reloading.step 或 EffectRecord.running_step 保存 completion identity。
task 每次只 poll 一个 step，并在 completion 中把 DriverEvent/error 与
driver 一起移交 Actor。只有 `Yield { has_next: true }` 的 driver 可作为 continuation 移入
下一 RuntimeTask；其余经 deferred-drop 析构。控制面与 task 不会同时持有同一个 driver。

L-Begin 构造 `ActivationDriver::Init`，不调用 Component 用户代码。第一次 task poll 先把
driver 暂置为 Exhausted，再调用 `Component::activate`，把返回值安装为 Once/Steps 并继续
poll 当前 step；因此同步 panic 时 wrapper 仍是有效的 Exhausted driver，可以随
ActivationFailure completion 归还 Actor 并 post-commit drop。

`ContractVTable::of::<C>()` 是 Core 自己在 `Requirement::new::<C>` 和 erased provide command
中调用的内部泛型构造器。Vtable 只含 TypeId 与函数指针，可复制；Runtime contract map 保存其副本。
Vtable 的 identity/merge adapter 校验每个 InterceptValue 的 contract_type 与 intercept_type，
输出值也写入同一对 TypeId。
FiberCompletion 保存从 O-Retire 到 O-Remove 的 subtree DisposeReport，并供所有
FiberHandle waiter 共享。Mount inverse 不等待该 completion；descendant failure 发生时，
Actor 直接追加到 parent chain 中所有已 retired collector。

default Realm 使用 local 0，next_realm 从 1 开始。Realm allocator 只生成绑定到 RuntimeTag
的 opaque identity，不读取或修改 RuntimeCore；因此 create_realm 是同步操作。
RootContext/Context/ComponentContext::default_realm 同样只克隆 RuntimeTag 并使用 local 0。

ComponentContext 把同一 Fiber/Episode 的 Context 与 Scope 组合为直接使用的组件接口；
派生 route 不改变 Scope 的 owner。Runtime 的便捷入口使用 Root owner。`on_cleanup` 只是
通过普通 Effect 保存 User inverse 并等待登记完成，这些入口均复用以上数据模型。

`UnloadOutcome` 是 `LifecycleState::Unloading` 的内部结果（Normal 或带 revision 的
ActivationFailed），`EpisodePhase` 仅是 snapshot DTO 的稳定投影，见
[可观测性](../operations/observability.md)。
