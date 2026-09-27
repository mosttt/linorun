# Core API 总览

[简体中文](core-api.md) | [English](../../en/api/core-api.md)

本文件描述 Linorun Core 0.1 的公共 API。运行时规范规定生命周期语义；本文件规定 Rust
类型和调用边界。实现可以隐藏字段，但不能改变这些返回值和完成条件。

## 1. 顶层类型

~~~rust
pub struct Runtime;
pub struct RuntimeBuilder;
pub struct RootContext;
pub struct Context;
pub struct Scope;
pub struct ComponentContext;
pub struct FiberHandle;
pub struct EffectHandle;
pub struct ServiceRef<C: ServiceContract>;
pub struct CallContext<I>;
pub struct ComponentDescriptor;
#[doc(hidden)]
pub struct Requirement; // descriptor macro plumbing; private fields
pub trait ErasedEndpoint: Send + Sync + 'static {
    #[doc(hidden)]
    fn as_any(&self) -> &(dyn Any + Send + Sync);
}
pub struct RuntimeSnapshot;
pub struct FiberSnapshot;
pub struct EpisodeSnapshot;
pub struct ActivationKeySnapshot;
pub struct BindingSnapshot;
pub struct EffectSnapshot;
pub struct InverseSnapshot;
pub struct RuntimeEvent;
pub enum RuntimeEventKind;
pub struct BindingEventRef;

pub struct FiberId(/* private u64 */);
pub struct EpisodeId(/* private u64 */);
pub struct EffectId(/* private u64 */);
pub struct BindingId(/* private u64 */);
pub struct StepId(/* private u64 */);
pub struct ComponentId(/* private stable name */);
pub struct ServiceId(/* private stable name */);
pub struct RealmId(/* private Runtime identity + local u64 */);

pub type RuntimeTask = Pin<Box<
    dyn Future<Output = ()> + Send + 'static
>>;

pub trait TaskSpawner: Send + Sync + 'static {
    fn spawn(&self, task: RuntimeTask);
}

pub enum RuntimeState {
    Running,
    ShuttingDown,
    Stopped,
}

pub enum EpisodePhase {
    Loading,
    Active,
    WaitingWithdrawal,
    Cleaning,
}

pub enum EffectSnapshotState {
    Applying,
    Armed,
    Disposing,
}

pub enum InverseSnapshotState {
    Armed,
    Running,
}

pub enum EffectForwardOutcome {
    Completed,
    Disposed,
}

impl ComponentId {
    pub fn as_str(&self) -> &str;
}

impl ServiceId {
    pub fn as_str(&self) -> &str;
}

impl Requirement {
    #[doc(hidden)]
    pub fn new<C: ServiceContract>(declared: Option<C::Intercept>) -> Self;
}

impl ComponentDescriptor {
    #[doc(hidden)]
    pub fn new<Config>(
        id: &'static str,
        requirements: Vec<Requirement>,
        provides: Vec<&'static str>,
    ) -> Self
    where
        Config: Send + Sync + 'static;
}
~~~

`ComponentDescriptor::new` 只供宏/static 定义使用。重复 Requirement ServiceId、重复 provides
ServiceId 或普通组件使用保留 ID `linorun/root@1` 是 descriptor definition error；构造在发布
descriptor 前 panic，不产生可被 mount 的部分对象。`config_type` 始终由泛型 Config 写入。

所有 handle 和 Context 都是轻量 Clone。Drop 不会隐式 dispose；Runtime 持有真正的
生命周期。

Runtime 也不在 Drop 中伪造异步 shutdown。Host 必须显式 await shutdown 后再释放最后一个
Runtime/RootContext handle；否则 Core 不承诺 effect 已回收。

RuntimeSnapshot 及 Fiber/Binding/Effect/Inverse snapshot DTO 的字段见
[可观测性](../operations/observability.md)。这些 DTO 和上列 snapshot enum 是 public read-only 类型；
内部 LifecycleState、EffectState、RunningInverse 和 accumulator entry 仍是 crate-private。
RuntimeEvent/RuntimeEventKind 的精确 payload 同样见可观测性文档。

## 2. Runtime

~~~rust
impl Runtime {
    pub fn new<S: TaskSpawner>(spawner: S) -> Self;
    pub fn builder<S: TaskSpawner>(spawner: S) -> RuntimeBuilder;
    pub fn root(&self) -> RootContext;
    pub async fn snapshot(&self) -> RuntimeSnapshot;
    pub async fn wait_quiescent(&self) -> RuntimeSnapshot;
    pub async fn shutdown(&self) -> ShutdownReport;
}

impl RuntimeBuilder {
    pub fn event_sender(
        self,
        sender: std::sync::mpsc::Sender<RuntimeEvent>,
    ) -> Self;
    pub fn build(self) -> Runtime;
}
~~~

`Runtime::new` 委托 `Runtime::builder(spawner).build()`。闭包 `Fn(RuntimeTask) + Send + Sync + 'static`
可以直接作为 TaskSpawner。Runtime 还提供与默认 `root()` 相同签名的 mount、provide、effect
和 on_cleanup，均保持 Root 所有权；隔离和调用元数据派生使用 `root()`。

TaskSpawner::spawn 必须只提交任务并立即返回；它不能在调用栈内 poll task，且不得 panic。
RuntimeBuilder 把 spawner 类型擦除后保存。可选 event_sender 是唯一其他配置；它使用标准库无界 Sender，
Actor 在提交状态后按 sequence 发送，不调用用户 callback。Receiver 断开只停止诊断输出，
不改变 Runtime 状态。确定性调度器属于 `linorun-testkit`，不进入生产 Builder。

`wait_quiescent` 等待整个 Runtime 的 quiescent 定义成立。`shutdown` 是 graceful 操作：
它不丢弃用户 Future、inverse Future 或 continuation；因此永不结束的用户 step 会让
shutdown 一直 pending。

RuntimeState 不是 Running 时，新的 Root/Scope effect、mount、provide 以及 Fiber
restart/update/replace 返回 RuntimeShuttingDown。只读查询、wait/dispose、重复 shutdown 和
不修改 RuntimeCore 的 default_realm/create_realm/isolate/intercept 仍可调用。
已开始的 activation 或 Effect step 在交回资源时，仍可通过原 Episode 的 `on_cleanup`
完成清理登记；进入用户清理阶段后不再接受登记，过期 Episode 也不能借此恢复操作权限。

## 3. RootContext

~~~rust
impl RootContext {
    pub fn default_realm(&self) -> RealmId;
    pub fn create_realm(&self) -> RealmId;

    pub async fn effect<E>(
        &self,
        label: impl Into<Arc<str>>,
        effect: E,
    ) -> Result<EffectHandle, EffectStartError>
    where
        E: IntoEffectDriver;

    pub async fn mount<C: Component>(
        &self,
        component: C,
        config: C::Config,
    ) -> Result<FiberHandle, MountError>;

    pub async fn provide<C: ServiceContract>(
        &self,
        service: C::Provider,
    ) -> Result<EffectHandle, ProvideError>;

    pub fn isolate<C: ServiceContract>(
        &self,
        realm: RealmId,
    ) -> Result<RootContext, RealmMismatch>;

    pub fn intercept<C: ServiceContract>(
        &self,
        value: C::Intercept,
    ) -> RootContext;
}
~~~

Root 的 effect、mount 和 provide 都是 Root-owned tracked effects，并由 shutdown 回收。
RealmId 由 RuntimeHandle 的原子 identity allocator 分配，不修改 RuntimeCore；RootContext
的 default_realm 纯构造同一 RuntimeTag/local 0。RootContext 不能绕过 effect tracking 或
直接修改 Fiber registry。RootContext::provide 不受普通
Component descriptor 的 provides 集合限制；
`UndeclaredProvide` 只适用于 Scope::provide。

## 4. Component 与 activation

~~~rust
pub trait Component: Send + Sync + 'static {
    type Config: Send + Sync + 'static;

    fn descriptor(&self) -> &'static ComponentDescriptor;

    fn activate(
        &self,
        ctx: ComponentContext,
        config: Arc<Self::Config>,
    ) -> Activation;
}

pub enum Activation {
    Once(ActivationFuture),
    Steps(ActivationStream),
}

impl Activation {
    pub fn run<F>(setup: F) -> Self
    where F: Future<Output = Result<(), ActivationFailure>> + Send + 'static;
    pub fn once<F>(setup: F) -> Self
    where F: Future<Output = Result<Option<InverseFn>, ActivationFailure>> + Send + 'static;
    pub fn steps<S>(setup: S) -> Self
    where S: Stream<Item = Result<InverseFn, ActivationFailure>> + Send + 'static;
}

pub type ActivationFuture = Pin<Box<
    dyn Future<Output = Result<Option<InverseFn>, ActivationFailure>> + Send + 'static
>>;

pub type ActivationStream = Pin<Box<
    dyn Stream<Item = Result<InverseFn, ActivationFailure>> + Send + 'static
>>;
~~~

`#[component]` 可把异步函数生成同名无参组件工厂，依赖和提供声明写在属性上；
`#[service_impl]` 把普通 async 方法生成服务实现。四个宏均由 `linorun_core` 重导出。
详见[组件编写](component-authoring.md)和[服务契约](service-contracts.md)。
`Activation::run` 接受返回 `Result<(), ActivationFailure>` 的启动 Future，转换为不返回额外
inverse 的 Once；`once` 与 `steps` 为原有变体自动装箱。原有 trait/enum/driver 能力全部保留。

`Once` 是一个粗粒度 inertial step；`Some(inverse)` 产生一次 Yield，`None` 完成。
`Steps` 每个 item 产生一个 Yield，stream 结束产生 Done。两者都在 step boundary 触发
最新 `ActivationKey` 检查；错误进入 L-Raise。

L-Begin 不在 Actor 内调用 `Component::activate`。Core 先建立一个持有 implementation、
ComponentContext 与 config 的 Init driver，第一 RuntimeTask 再调用该方法并 poll 第一 step；
方法同步 panic 与 Future/Stream poll panic 都由同一 activation task boundary 转成
ActivationFailure。

descriptor 宏用 public、doc-hidden 的 `Requirement::new::<C>` 和
`ComponentDescriptor::new::<Config>` 生成一个 `LazyLock<ComponentDescriptor>`；解析后的
descriptor 地址是进程内唯一静态 identity。构造器在 Core 内写入 Config/contract/intercept
TypeId，宏不直接接触 ContractVTable 或 InterceptValue。

## 5. ComponentContext、Context 与 Scope

~~~rust
pub struct ComponentContext {
    context: Context,
    scope: Scope,
}

impl ComponentContext {
    pub fn context(&self) -> &Context;
    pub fn scope(&self) -> &Scope;
    pub fn default_realm(&self) -> RealmId;
    pub fn service<C: ServiceContract>(&self) -> Result<C::Client, ServiceAccessError>;
    pub async fn provide<C: ServiceContract>(&self, provider: C::Provider)
        -> Result<EffectHandle, ProvideError>;
    pub async fn mount<C: Component>(&self, component: C, config: C::Config)
        -> Result<FiberHandle, MountError>;
    pub async fn effect<E: IntoEffectDriver>(&self, label: impl Into<Arc<str>>, effect: E)
        -> Result<EffectHandle, EffectStartError>;
    pub fn isolate<C: ServiceContract>(&self, realm: RealmId) -> Result<Self, RealmMismatch>;
    pub fn intercept<C: ServiceContract>(&self, value: C::Intercept) -> Self;
    pub async fn on_cleanup<F, Fut>(&self, label: impl Into<Arc<str>>, cleanup: F)
        -> Result<EffectHandle, EffectStartError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CleanupError>> + Send + 'static;
}

impl Context {
    pub fn default_realm(&self) -> RealmId;

    pub fn service<C: ServiceContract>(
        &self,
    ) -> Result<C::Client, ServiceAccessError>;

    pub fn isolate<C: ServiceContract>(
        &self,
        realm: RealmId,
    ) -> Result<Context, RealmMismatch>;

    pub fn intercept<C: ServiceContract>(
        &self,
        value: C::Intercept,
    ) -> Context;
}

impl Scope {
    pub async fn effect<E>(
        &self,
        label: impl Into<Arc<str>>,
        effect: E,
    ) -> Result<EffectHandle, EffectStartError>
    where
        E: IntoEffectDriver;

    pub async fn provide<C: ServiceContract>(
        &self,
        context: &Context,
        service: C::Provider,
    ) -> Result<EffectHandle, ProvideError>;

    pub async fn mount<C: Component>(
        &self,
        context: &Context,
        component: C,
        config: C::Config,
    ) -> Result<FiberHandle, MountError>;
}
~~~

`ComponentContext` 支持 Clone；便捷方法使用它保存的 Context 与 Scope，派生 isolate/intercept
保留同一 Scope 归属。RootContext、Scope、Runtime 也提供上述相同签名的 `on_cleanup`。
它通过已有 Effect 路径登记 inverse，等待登记步骤落地后返回，保证连续登记时的逆序；不会在
登记时调用用户清理代码。返回的 EffectHandle 支持提前 dispose。
若 dispose/shutdown 发生在资源获取途中，已开始的 activation 或 Effect step 仍可登记
刚取得资源的清理。应在这个受运行时跟踪的步骤中完成登记；脱离步骤的后台任务不能依靠
保留的旧 Context 无限期登记资源。

`Context` 只读取创建它的 Fiber Episode 的 CommittedView；它不沿 parent Fiber 隐式查找
service。未声明 service 返回 `ServiceAccessError::UndeclaredDependency`。`Scope` 的
provide/mount 必须使用同一 Runtime、当前 Fiber 和 Episode 派生的 Context；这些操作都会
转成普通 tracked effect。

在当前 Fiber 上新派生的 isolate 不会重选该 Fiber 已提交的 endpoint；它只影响随后用该
Context 执行的 mount/provide。intercept overlay 则会进入此后新构造的 `C::Client`。
`default_realm()` 从 RuntimeTag 纯构造 local 0 RealmId，不访问 Actor；在外层 isolate 后，
可把该值再次传给 `isolate::<C>`，显式把 C 的后续路由设回 Runtime default。

Scope/RootContext::provide 与 mount 的 label 由 Core 固定为
`linorun:provide:{ServiceId}`、`linorun:mount:{ComponentId}`；普通 effect 使用调用方传入
的 label。

## 6. Effect driver

~~~rust
pub trait EffectDriver: Send + 'static {
    fn poll_step(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<DriverEvent, EffectError>>;
}

pub enum DriverEvent {
    Yield {
        inverse: InverseFn,
        has_next: bool,
    },
    Done,
}

pub trait IntoEffectDriver {
    type Driver: EffectDriver;
    fn into_driver(self) -> Self::Driver;
}

pub type InverseFuture = Pin<Box<
    dyn Future<Output = Result<(), CleanupError>> + Send + 'static
>>;

pub type InverseFn = Box<dyn FnOnce() -> InverseFuture + Send + 'static>;

pub mod effect {
    pub fn inverse<F, Fut>(cleanup: F) -> InverseFn
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CleanupError>> + Send + 'static;

    pub fn sync<F>(forward: F) -> impl IntoEffectDriver
    where
        F: FnOnce() -> Result<Option<InverseFn>, EffectError> + Send + 'static;

    pub fn once<F, Fut>(forward: F) -> impl IntoEffectDriver
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<InverseFn>, EffectError>> + Send + 'static;

    pub fn iter<F, I>(forward: F) -> impl IntoEffectDriver
    where
        F: FnOnce() -> I + Send + 'static,
        I: IntoIterator<Item = Result<InverseFn, EffectError>> + Send + 'static,
        I::IntoIter: Send + 'static;

    pub fn steps<F, S>(forward: F) -> impl IntoEffectDriver
    where
        F: FnOnce() -> S + Send + 'static,
        S: Stream<Item = Result<InverseFn, EffectError>> + Send + 'static;
}
~~~

Activation 使用同一事件形状但独立的 `ActivationFailure` 类型。Effect forward 错误和
inverse cleanup 错误不会混用。`effect` 模块的四个构造器只建立 driver；forward closure
在 Actor 提交 EffectStarted 后由 TaskSpawner 调度的 task 调用，不在 API 调用栈或 Actor
turn 内提前执行。自定义类型也可以直接实现 EffectDriver/IntoEffectDriver。
`IntoEffectDriver::into_driver` 本身只做非阻塞、非 panic、无外部 effect 的 ownership
转换；所有 forward 工作从第一次 `poll_step` 开始。
`Yield { has_next: true }` 要求该 driver 可继续 poll；RuntimeTask 在本 step 返回后把 driver
所有权交回 Actor，只有 Actor 提交边界决策后才会调度下一次 poll。`has_next: false` 是携带
最后一个 inverse 的 final Yield，Done 则不产生 inverse。

## 7. FiberHandle

~~~rust
impl FiberHandle {
    pub const fn id(&self) -> FiberId;
    pub async fn state(&self) -> FiberState;
    pub async fn wait_settled(&self) -> FiberState;
    pub async fn restart(&self) -> Result<(), FiberError>;
    pub async fn update_config<T>(
        &self,
        config: T,
    ) -> Result<(), UpdateError>
    where
        T: Send + Sync + 'static;
    pub async fn replace<C: Component>(
        &self,
        component: C,
    ) -> Result<(), UpdateError>;
    pub async fn dispose(&self) -> DisposeReport;
}
~~~

`wait_settled` 等待这个 Fiber 完成可立即执行的 lifecycle reconcile，并到达 Pending、
Active、Failed 或 Disposed。缺失 Requirement 的 Pending 会立即返回；Failed 返回
`FiberState::Failed`，失败细节从 snapshot 读取；不会把 activation failure 当成等待 API
的 Rust `Err`。独立 Scope effect 即使仍在 forward 或手动 cleanup，也不妨碍 Active Fiber
被视为 lifecycle-settled；这类工作仍会阻止 Runtime quiescent。

`update_config` 要求 `TypeId::of::<T>()` 等于 Fiber descriptor 的 `config_type`；
`replace` 要求新实现返回与 Fiber 完全相同的 `'static` descriptor，且其关联 Config 类型
匹配。失败返回 `DescriptorMismatch`，不改变 snapshot 或 revision。验证成功的
`update_config`、`replace` 和 `restart` 都增加 revision 并清除失败；若失败正在 Unloading
中等待提交，新的 revision 使该失败失效。

`dispose` 幂等地提交 O-Retire，并等待 FiberCompletion 发布 Disposed 后返回。它同时把
父级中只执行同一 O-Retire 的 Core-generated mount inverse 标记为已满足，因此该 inverse
不会再调用。重复调用共享同一个 child DisposeReport。Synthetic Root 没有 FiberHandle，
也没有 mount effect。

~~~rust
pub enum FiberState {
    Pending,
    Loading,
    Active,
    Unloading,
    Failed,
    Disposed,
}
~~~

## 8. EffectHandle

~~~rust
impl EffectHandle {
    pub const fn id(&self) -> EffectId;
    pub async fn wait_forward(
        &self,
    ) -> Result<EffectForwardOutcome, EffectError>;
    pub async fn dispose(&self) -> EffectDisposeReport;
}
~~~

`dispose` 幂等；同一 effect 的所有调用等待同一个 completion。当前 iteration 必须先落地，
已产生的 inverse 按 seq 降序串行执行。

对 provide 返回的句柄，撤回只等待删除时真正引用该 BindingId 的消费者 Episode：该 Episode
已结束，或仍 Active 且 ActivationKey 与最新意图一致时通过。不跟随后续 Episode，也不等待
仅声明需求的 watcher、子树移除或无关 Effect。详见[服务语义](../semantics/services-and-coeffects.md)。

`wait_forward` 只等待 driver 到达终止 boundary，不等待 inverse cleanup：自然 Done 返回
Completed；manual dispose 或 Fiber unload 在自然 Done 前关闭 continuation 时，当前 step
落地后返回 Disposed；该 step 返回 EffectError/panic 时错误优先并返回 Err。完整 cleanup
结果由 `dispose` 或所属 Fiber/Runtime report 提供。

## 9. Typed Client、ServiceRef 与 ServiceContract

~~~rust
pub trait ServiceContract: Sized + Send + Sync + 'static {
    const ID: &'static str;
    type Provider: Send + Sync + 'static;
    type Intercept: Clone + Send + Sync + 'static;
    type Client: Clone + Send + Sync + 'static;

    fn identity_intercept() -> Self::Intercept;
    fn merge_intercept(
        base: &Self::Intercept,
        overlay: &Self::Intercept,
    ) -> Self::Intercept;
    fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint>;

    #[doc(hidden)]
    fn client(reference: ServiceRef<Self>) -> Self::Client;
}

pub struct ServiceRef<C: ServiceContract> {
    inner: Arc<ServiceRefInner<C>>,
}

pub struct CallContext<I> {
    pub caller: FiberId,
    pub caller_episode: EpisodeId,
    pub provider: FiberId,
    pub binding: BindingId,
    pub intercept: I,
}

impl<C: ServiceContract> ServiceRef<C> {
    #[doc(hidden)]
    pub fn dispatch_parts(
        &self,
    ) -> (Arc<dyn ErasedEndpoint>, CallContext<C::Intercept>);
}
~~~

`Context::service::<C>()` 构造字段私有的 ServiceRef，再调用 `C::client`，对组件返回
contract crate 中宏生成的 `C::Client`。调用方法定义在该本地 Client newtype 上；Rust 不允许
下游 crate 为 `linorun-core` 拥有的 `ServiceRef<C>` 添加 inherent methods。

ServiceRef 固定保存构造时的 CommittedView endpoint 和 effective intercept，不查询最新 Binding store。
`dispatch_parts` 与 ErasedEndpoint::as_any 是供宏生成 adapter 使用的 doc-hidden plumbing，
普通组件代码只使用 `C::Client`。Runtime 在 mount 时校验 Requirements 及相关 route
intercept，在 provide 时校验泛型 `C` 的 contract identity；provide 不读取无关 overlay。
校验后再把 `ServiceId -> Contract TypeId/vtable` 映射与操作结果原子提交；同一 ServiceId
的类型冲突返回 ServiceTypeMismatch，且命令不改变 RuntimeCore。
`Context::service` 构造 Client 时合并已验证的 declared metadata 和 route overlays。

## 10. Reports 与错误

~~~rust
pub struct EffectDisposeReport {
    pub forward_error: Option<EffectError>,
    pub cleanup_errors: Vec<CleanupFailure>,
}

pub struct DisposeReport {
    pub cleanup_errors: Vec<CleanupFailure>,
}

pub struct ShutdownReport {
    pub cleanup_errors: Vec<CleanupFailure>,
}

pub enum FailureKind {
    User,
    Panic,
}

pub struct FailureCause {
    pub kind: FailureKind,
    pub message: Arc<str>,
}

pub struct ActivationFailure {
    pub cause: FailureCause,
}

pub struct EffectError {
    pub cause: FailureCause,
}

pub struct CleanupError {
    pub cause: FailureCause,
}

pub struct CleanupFailure {
    pub owner: CleanupOwner,
    pub fiber: FiberId,
    pub episode: EpisodeId,
    pub seq: u64,
    pub label: Option<Arc<str>>,
    pub cause: CleanupError,
}

pub enum CleanupOwner {
    Activation,
    Effect(EffectId),
}

pub struct RealmMismatch {
    pub realm: RealmId,
}

pub enum ServiceTypeChannel {
    Contract,
    Intercept,
}

pub struct ServiceTypeMismatch {
    pub service: ServiceId,
    pub channel: ServiceTypeChannel,
    pub expected: TypeId,
    pub actual: TypeId,
}

impl ActivationFailure {
    pub fn user(message: impl Into<Arc<str>>) -> Self;
}

impl EffectError {
    pub fn user(message: impl Into<Arc<str>>) -> Self;
}

impl CleanupError {
    pub fn user(message: impl Into<Arc<str>>) -> Self;
}

impl From<EffectStartError> for ActivationFailure;
impl From<EffectError> for ActivationFailure;
~~~

`ActivationFailure` 可从 EffectStartError、EffectError、MountError、ProvideError、
ServiceAccessError、RealmMismatch 和 std::io::Error 转换；CleanupError 可从 std::io::Error 转换。
组件内这些操作可直接使用 `?`；服务自定义错误仍由应用显式映射。

调用级错误按操作归属：

~~~rust
pub enum MountError {
    RuntimeShuttingDown,
    InactiveContext,
    InactiveScope,
    FiberDisposed,
    DescriptorMismatch,
    ServiceTypeMismatch(ServiceTypeMismatch),
}

pub enum ProvideError {
    RuntimeShuttingDown,
    InactiveContext,
    InactiveScope,
    UndeclaredProvide,
    DuplicateBinding,
    ServiceTypeMismatch(ServiceTypeMismatch),
}

pub enum EffectStartError {
    RuntimeShuttingDown,
    InactiveScope,
}

pub enum ServiceAccessError {
    UndeclaredDependency,
    ServiceTypeMismatch(ServiceTypeMismatch),
}

pub enum FiberError {
    RuntimeShuttingDown,
    FiberDisposed,
}

pub enum UpdateError {
    RuntimeShuttingDown,
    FiberDisposed,
    DescriptorMismatch,
}
~~~

Report 的聚合范围固定如下：

- EffectDisposeReport：该 EffectId 的 forward error，以及本次 disposal 运行的 entries；
- DisposeReport：从目标 Fiber 被 O-Retire 起，到其 O-Remove 为止，目标 Fiber 以及在该窗口
  内属于其 subtree 的 descendants 所产生的 cleanup failures；
- ShutdownReport：从 Running -> ShuttingDown 起，到 Stopped 为止，Root 和全部非 Root Fiber
  的 cleanup failures。

正常 reload 中已经通过事件发送的旧 episode failure 不会在未来 dispose/shutdown report 中重复出现。
failure 发生时即追加到所有适用 collector，保留 CleanupFailure 原始 FiberId/EpisodeId；
顺序按实际 InverseFinished 提交顺序。

`RealmMismatch` 是 `isolate` 的返回类型，不是上述枚举的重复变体。`InactiveContext` 只
属于 Scope 的异步 ownership 校验；同步只读 lookup 的未声明依赖使用
`UndeclaredDependency`。所有 cleanup failure 都聚合，不阻止更早的 inverse。
`FiberDisposed` 同时覆盖已 O-Retire 和已 O-Remove 的 Fiber；retired Fiber 只接受
state/wait/dispose，不再接受 restart/update/replace 或新的 Scope mutation。
所有错误类型实现 std::error::Error + Display；ServiceTypeMismatch 的 channel 区分
ServiceContract 与 Intercept payload，RealmMismatch.realm 是来自另一 Runtime 的 handle。

同一调用同时违反多项前提时，错误优先级固定为：

1. RuntimeShuttingDown；
2. FiberDisposed，或该 API 没有此变体时的 InactiveScope；
3. InactiveScope；
4. InactiveContext；
5. DescriptorMismatch / UndeclaredProvide / UndeclaredDependency；
6. ServiceTypeMismatch / RealmMismatch；
7. DuplicateBinding。

只读 `Context::service<C>` 先按 C::ID 查 Requirement：缺失为 UndeclaredDependency，存在后
才校验 contract/intercept。RootContext::provide 跳过 UndeclaredProvide。

## 11. Send 与 Sync

- Component、Config、Provider endpoint 和 inverse Future 必须 `Send`；
- Runtime Actor 在一个逻辑 task 中串行运行；
- `ErasedEndpoint` 实现 `Send + Sync`；
- Runtime、RootContext、Context、Scope、ServiceRef、ServiceContract::Client 和 Handle 实现
  `Clone + Send + Sync`；
- Core 不在持有控制面借用或锁时调用或等待组件代码、endpoint 或 inverse。

所有会被 Core 持有的用户类型之 Drop 必须非阻塞且不得 panic；异步资源关闭写入 inverse。

ID newtype 实现 Clone/Debug/Eq/Hash/Ord；u64-backed ID 还实现 Copy。ComponentId/ServiceId
通过 `as_str` 暴露其稳定名称，其余 ID 保持 opaque。错误、report、snapshot 和
RuntimeEvent DTO 实现 Clone + Debug，保证幂等 waiter 可以返回同一值的副本。Handle、Context
和 RuntimeEvent 是 Send + Sync；RuntimeTask、用户 Future 和 inverse 至少是 Send。
