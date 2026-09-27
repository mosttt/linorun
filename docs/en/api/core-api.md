# Core API reference

[English](core-api.md) | [简体中文](../../zh/api/core-api.md)

This document describes the public API of Linorun Core 0.1. The runtime specification defines lifecycle semantics;
this reference defines Rust types and call boundaries. Implementations may hide fields but must
preserve these return values and completion conditions.

## 1. Top-level types

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


`ComponentDescriptor::new` is intended for macros and static definitions. Duplicate Requirement
ServiceIds, duplicate provided ServiceIds, or using the reserved `linorun/root@1` ID for an ordinary
component are descriptor definition errors. Construction panics before publishing the descriptor,
without producing a partially mountable object. The generic Config supplies `config_type`.

Handles and contexts are lightweight Clone values. Drop does not implicitly dispose them: the
runtime owns their actual lifecycle. Nor does Runtime pretend to perform asynchronous shutdown
in Drop. Hosts must explicitly await shutdown before releasing their last Runtime/RootContext
handle; otherwise Core does not guarantee effect cleanup.

The public read-only RuntimeSnapshot and Fiber/Binding/Effect/Inverse DTO fields and exact
RuntimeEvent/RuntimeEventKind payloads are specified in [observability](../operations/observability.md).
Internal LifecycleState, EffectState, RunningInverse, and accumulator entries remain crate-private.

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


`Runtime::new` delegates to `Runtime::builder(spawner).build()`. Any
`Fn(RuntimeTask) + Send + Sync + 'static` closure can be a TaskSpawner. Runtime also provides
mount, provide, effect, and on_cleanup with the same signatures as its default `root()`; all retain
Root ownership. Use `root()` to derive service isolation or call metadata.

TaskSpawner::spawn must submit tasks and return immediately, without polling inline or panicking.
RuntimeBuilder stores a type-erased spawner. Its only other option is event_sender, an unbounded
standard-library Sender. After committing state, the Actor sends events in sequence order rather
than invoking callbacks. A disconnected receiver stops diagnostic output without changing runtime
state. The deterministic executor belongs to `linorun-testkit`, not the production Builder.

`wait_quiescent` waits for the runtime-wide quiescence condition. `shutdown` is graceful: it does
not discard user futures, inverse futures, or continuations. A user step that never finishes can
therefore leave shutdown pending indefinitely.

When RuntimeState is not Running, new Root/Scope effects, mounts, publications, and Fiber
restart/update/replace return RuntimeShuttingDown. Read-only queries, waits, disposal, repeated
shutdown, and default_realm/create_realm/isolate/intercept operations that do not mutate
RuntimeCore remain available.
An already running activation or Effect step may still use its original Episode's `on_cleanup`
to register cleanup while handing back acquired resources. Registration closes once user cleanup
starts; an expired Episode cannot use this exception to regain authority.

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


Root effect, mount, and provide operations are tracked Root-owned effects reclaimed by shutdown.
RuntimeHandle's atomic identity allocator allocates RealmId without mutating RuntimeCore;
default_realm constructs local 0 under the same RuntimeTag. RootContext cannot bypass tracking
or directly mutate the Fiber registry. RootContext::provide is exempt from ordinary component
provides declarations; UndeclaredProvide applies to Scope::provide.

## 4. Component and activation

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


`#[component]` generates a component factory with the same name as an async function; the attribute
holds its required and provided services. `#[service_impl]` generates provider implementations
from ordinary async methods. All four macros are re-exported by `linorun_core`; see
[component authoring](component-authoring.md) and [service contracts](service-contracts.md).
`Activation::run` accepts a setup Future returning `Result<(), ActivationFailure>` and adapts it to
Once without an additional returned inverse. `once` and `steps` box their corresponding inputs.
All original trait, enum, and driver capabilities remain available.

Once is one coarse inertial step: Some(inverse) yields once, while None finishes. Each Steps item
yields once; the end of its stream produces Done. Both check the latest ActivationKey at step
boundaries; errors trigger L-Raise.

L-Begin does not call Component::activate inside the Actor. Core creates an Init driver holding the
implementation, ComponentContext, and config. The first RuntimeTask calls activate and polls the
first step. Synchronous method panics and Future/Stream poll panics are both converted to
ActivationFailure at that activation task boundary.

The descriptor macro calls the public doc-hidden Requirement::new::<C> and
ComponentDescriptor::new::<Config> constructors to generate a LazyLock<ComponentDescriptor>.
Its resolved address is a unique static identity within the process. Core records Config, contract,
and intercept TypeIds; macros do not access ContractVTable or InterceptValue directly.

## 5. ComponentContext, Context, and Scope

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


ComponentContext implements Clone. Its convenience methods use the stored Context and Scope;
isolate/intercept derivations retain the same Scope owner. RootContext, Scope, and Runtime also
provide on_cleanup with the signature above. It records an inverse through the existing Effect
path and awaits registration before returning, preserving reverse order across sequential
registrations. It does not call user cleanup code during registration. The returned EffectHandle
supports early disposal.
If dispose/shutdown starts during resource acquisition, the already running activation or Effect
step may still register cleanup for the acquired resource. Complete registration within that
tracked step; detached background work cannot retain an old Context to register resources indefinitely.

Context reads only the CommittedView of the Fiber Episode that created it; it does not implicitly
search parent Fibers. Undeclared services return ServiceAccessError::UndeclaredDependency.
Scope provide/mount require a Context derived from the same Runtime, current Fiber, and Episode;
both operations become ordinary tracked effects.

Deriving isolate on a current Fiber does not reselect its committed endpoint. It affects subsequent
mount/provide operations using that Context. Intercept overlays apply to newly constructed clients.
default_realm constructs local 0 under the RuntimeTag without contacting the Actor. After an outer
isolation, pass this realm to isolate::<C> to explicitly restore C's future routing to the default.

Core fixes provide/mount labels to `linorun:provide:{ServiceId}` and
`linorun:mount:{ComponentId}`. Ordinary effects use the caller-supplied label.

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


Activation uses the same event shape with its separate ActivationFailure type. Forward errors and
inverse cleanup errors remain distinct. The four effect constructors only create drivers. The
forward closure runs in a TaskSpawner task after the Actor commits EffectStarted, never early on
the API call stack or inside an Actor turn. Custom types can implement EffectDriver/IntoEffectDriver.
into_driver itself must be a nonblocking, non-panicking ownership conversion without external
effects. Forward work begins on the first poll_step.

Yield with has_next=true requires a driver that can be polled again. The RuntimeTask returns its
ownership to the Actor after that step; only the Actor's committed boundary decision schedules the
next poll. has_next=false denotes the final Yield carrying the last inverse; Done carries no inverse.

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


wait_settled waits for immediately actionable lifecycle reconciliation to reach Pending, Active,
Failed, or Disposed. Pending due to a missing Requirement returns immediately. Failed returns
FiberState::Failed; read failure details from a snapshot. An activation failure is not converted into
a Rust Err from this waiting API. Independent Scope forward work or manual cleanup does not prevent
an Active Fiber from being lifecycle-settled, but does prevent runtime-wide quiescence.

update_config requires TypeId::of::<T>() to match the descriptor's config_type. replace requires the
exact same static descriptor and matching Config type. A mismatch returns DescriptorMismatch
without changing the snapshot or revision. Successful update_config, replace, and restart increment
the revision and clear failure; a new revision invalidates a failure still awaiting commitment during
Unloading.

dispose idempotently submits O-Retire and waits for FiberCompletion to publish Disposed. It also
satisfies the corresponding Core-generated mount inverse in the parent, preventing duplicate
O-Retire. Repeated calls share the child's DisposeReport. Synthetic Root has no FiberHandle or
mount effect.

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


dispose is idempotent: all calls for an effect wait for the same completion. The current iteration
must land first, and existing inverses run serially in descending sequence order.

For a handle returned by provide, withdrawal waits only for consumer Episodes that actually held
that BindingId at removal: an Episode passes once it ends or remains Active with the latest
ActivationKey. The wait does not follow later Episodes or include unrelated requirement watchers,
descendant removal, or independent Effects. See [service semantics](../semantics/services-and-coeffects.md).

wait_forward waits for the driver's terminal boundary, not inverse cleanup. Natural Done returns
Completed. If manual disposal or Fiber unload ends the continuation before natural completion,
it returns Disposed after the current step lands. An EffectError or panic from that step takes
precedence and returns Err. Full cleanup results come from dispose or the owning Fiber/Runtime report.

## 9. Typed Client, ServiceRef, and ServiceContract

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


Context::service::<C>() constructs a ServiceRef with private fields, then calls C::client to return
the macro-generated client owned by the contract crate. Methods are defined on that local newtype:
Rust does not permit downstream crates to add inherent methods to Core-owned ServiceRef<C>.

ServiceRef retains its committed endpoint and effective intercept; it never queries the latest
Binding store. dispatch_parts and ErasedEndpoint::as_any are doc-hidden adapter plumbing for
macros; ordinary components use C::Client. Mount validates Requirements and their relevant route
intercepts. Provide validates C's contract identity, ignoring unrelated overlays. Validation precedes
an atomic commit of the ServiceId-to-contract TypeId/vtable mapping and operation result. Conflicts
return ServiceTypeMismatch without mutating RuntimeCore. Context::service merges validated declared
metadata and route overlays when constructing a client.

## 10. Reports and errors

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


ActivationFailure converts from EffectStartError, EffectError, MountError, ProvideError,
ServiceAccessError, RealmMismatch, and std::io::Error. CleanupError converts from std::io::Error.
Component code can use `?` for these operations; application-specific service errors still need
explicit mapping.

Operation errors are grouped by ownership:

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


Report collection windows are fixed:

- EffectDisposeReport contains that EffectId's forward error and entries run during its disposal.
- DisposeReport contains cleanup failures from the target Fiber and descendants belonging to its
  subtree during the interval from its O-Retire to O-Remove.
- ShutdownReport contains Root and non-Root cleanup failures from Running → ShuttingDown through Stopped.

Failures from old episodes already emitted during ordinary reloads do not appear again in later
disposal or shutdown reports. Failures enter every applicable collector as they occur, preserving
original FiberId/EpisodeId and actual InverseFinished commit order.

RealmMismatch is isolate's return error, not a duplicated variant of the enums above. InactiveContext
belongs to Scope's asynchronous ownership validation; undeclared read-only lookup returns
UndeclaredDependency. Cleanup failures are aggregated without blocking earlier inverses.
FiberDisposed covers both retired and removed Fibers. Retired Fibers accept state/wait/dispose,
but no restart/update/replace or new Scope mutations. All errors implement std::error::Error and
Display. ServiceTypeMismatch.channel distinguishes contract and intercept payload mismatches;
RealmMismatch.realm is a handle from another Runtime.

When several preconditions fail, the precedence is:

1. RuntimeShuttingDown.
2. FiberDisposed, or InactiveScope if the API lacks that variant.
3. InactiveScope.
4. InactiveContext.
5. DescriptorMismatch / UndeclaredProvide / UndeclaredDependency.
6. ServiceTypeMismatch / RealmMismatch.
7. DuplicateBinding.

Read-only Context::service<C> first finds a Requirement by C::ID. A missing entry returns
UndeclaredDependency before contract/intercept validation. RootContext::provide skips UndeclaredProvide.

## 11. Send and Sync

- Component, Config, provider endpoints, and inverse futures must be Send.
- The Runtime Actor executes serially in one logical task.
- ErasedEndpoint implements Send + Sync.
- Runtime, RootContext, ComponentContext, Context, Scope, ServiceRef, ServiceContract::Client,
  and handles implement Clone + Send + Sync.
- Core never calls or awaits component code, endpoints, or inverses while borrowing or locking the control plane.

Drop for user types retained by Core must neither block nor panic. Asynchronous resource shutdown
belongs in an inverse.

ID newtypes implement Clone/Debug/Eq/Hash/Ord; u64-backed IDs also implement Copy.
ComponentId/ServiceId expose stable names through as_str; other IDs remain opaque. Error, report,
snapshot, and RuntimeEvent DTOs implement Clone + Debug so idempotent waiters can receive copies
of the same value. Handles, contexts, and RuntimeEvent are Send + Sync; RuntimeTask, user futures,
and inverses are at least Send.
