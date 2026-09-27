# 定义和调用服务

[简体中文](service-contracts.md) | [English](../../en/api/service-contracts.md)

服务让组件通过明确的方法调用彼此，并让 Linorun 知道停止时谁应先清理。一个完整的服务
包含契约、实现和使用方。下面的日志服务不需要调用元数据：

```rust
use std::sync::Arc;
use linorun_core::{
    component, service_contract, service_impl, ActivationFailure, CallContext, ComponentContext,
};

service_contract! {
    pub contract Logger {
        id: "app/logger@1",
        methods: [
            async fn info(message: String) -> Result<(), String>;
        ],
    }
}

pub struct ConsoleLogger;

#[service_impl]
impl LoggerProvider for ConsoleLogger {
    async fn info(&self, _: CallContext<()>, message: String) -> Result<(), String> {
        println!("{message}");
        Ok(())
    }
}

#[component(id = "app/logging@1", provides = [Logger])]
async fn logging(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    ctx.provide::<Logger>(Arc::new(ConsoleLogger)).await?;
    Ok(())
}

#[component(id = "app/reporter@1", requires = [Logger])]
async fn reporter(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    let logger = ctx.service::<Logger>()?;
    logger.info("reporter ready".to_owned()).await
        .map_err(ActivationFailure::user)?;
    Ok(())
}
```

挂载 `reporter()` 和 `logging()` 即可，先后顺序不影响依赖声明：如果日志服务尚未出现，
`reporter` 会保持 `Pending`；服务出现后自动启动。`mount` 返回不表示组件已经启动完成，
需要时等待 `fiber.wait_settled().await`。
如果先挂载了缺少依赖的使用方，应先等待提供方启动完成，再检查使用方状态；依赖缺失时，
`wait_settled` 可以返回 `Pending`，不会等待未来才出现的服务。

宿主也可以直接调用 `runtime.provide::<Logger>(Arc::new(ConsoleLogger)).await` 提供服务，
再挂载使用方。由组件提供还是由宿主提供，取决于谁拥有实现的生命周期。

## 契约生成哪些类型

上面的 `service_contract!` 生成：

| 名称 | 用途 |
| --- | --- |
| `Logger` | 契约类型，用在 `requires`、`provides`、`service` 和 `provide` 中 |
| `LoggerProvider` | 实现方需要实现的方法接口 |
| `LoggerClient` | `ctx.service::<Logger>()` 返回的可克隆客户端 |

`Logger` 的实现类型是 `Arc<dyn LoggerProvider>`。客户端直接转发到本次启动所选中的实现，
普通服务方法调用不经过 Runtime Actor 命令队列。

`id` 应使用 `namespace/name@major`，例如 `app/logger@1`。同一个 Runtime 内，同一个
服务 ID 对应同一种 Rust 契约类型；两个分别定义的契约不能只靠相同字符串冒充彼此。

方法参数使用标识符和拥有所有权的类型，例如 `String`、`Vec<T>` 或 `Arc<T>`。
`#[service_impl]` 处理装箱异步 Future，方法保持 `async fn` 写法。它支持非泛型的 trait
impl，方法接收 `&self`，然后是 `CallContext<I>` 和业务参数；需要其他实现形状时可使用
下文的显式 Provider 接口。

## 清理期间仍调用原来的实现

使用方可以在清理回调中继续调用依赖：

```rust
let logger = ctx.service::<Logger>()?;
ctx.on_cleanup("reporter shutdown", move || async move {
    logger.info("reporter stopped".to_owned()).await
        .map_err(linorun_core::CleanupError::user)?;
    Ok(())
}).await?;
```

一次启动所选择的服务保持不变。即使新的日志实现已经注册，旧客户端仍访问旧实现；在
同一次运行的上下文中重新调用 `service`，提供方身份也不会改变。组件下次启动时才取得
新的依赖。这份固定依赖称为 **CommittedView**。

提供方整体停止时，Linorun 先让服务退出新依赖解析，随后等待已经使用它的组件完成清理，再允许
提供方关闭更早登记的资源。这就是为什么使用方必须在 `requires` 中声明服务。

客户端及其调用应在所属的这次运行内使用。不要将它存入全局变量并在组件清理完毕后继续
调用；客户端持有的引用不等于运行时为它增加了一项独立生命周期依赖。

## 服务注册与资源关闭

`ctx.provide::<Logger>(provider).await` 返回 `EffectHandle`。组件停止时自动撤回这项
注册，也可以使用该句柄提前撤回。

手动撤回一项注册时，Runtime 通知所有关注该服务位置的组件，但只等待撤回时实际持有
这个 BindingId 的已提交使用方，并记录它们的 `(FiberId, EpisodeId)`。仅声明或关注相同
服务位置、却没有使用这项注册的组件，不进入等待集合。

对每个已记录的使用方，只需当时那次 Episode 已经离开；之后新启动的 Episode 不会延长
等待。如果仍是原 Episode，则它需要处于 Active，且 key 与最新 ActivationKey 相同——
同一提供方恢复时可以达到这一边界。

自身清理完成、仍在等待子组件移除的 retired 实例也已交还旧 Episode，即使公共状态还是
`Unloading`。因此，这一等待不等同于 `FiberHandle::wait_settled()`，也不等待整个后代树
移除、使用方后续启动或 Active 组件中无关的独立 effect 完成。

同一提供方恢复时，使用方可能保持原来的运行和旧客户端，手动撤回仍可完成；它不是强制
收回所有旧客户端。使用方自身启动失败也不会被转换成提供方的清理错误。

服务注册只管理 Binding 和实现的引用。数据库连接池、后台任务等底层资源由提供方自行
建立并登记清理：先等待资源清理登记完成，再发布服务。这样逆序清理会先撤回服务、等待
使用方退出，再关闭底层资源。

同一个服务和 Realm 只能同时存在一个可见注册；冲突返回 `DuplicateBinding`。同一服务
需要多个独立实现时使用不同 Realm，见[常用概念](../guides/concepts.md)。

## 为调用附加元数据

需要日志标签、追踪字段或服务自己的策略信息时，给契约定义 `Intercept`。下面使用标签
列表，合并规则是按外到内追加：

```rust
use linorun_core::service_contract;

#[derive(Clone, Default)]
pub struct LogTags(pub Vec<String>);

fn merge_tags(base: &LogTags, overlay: &LogTags) -> LogTags {
    LogTags(base.0.iter().chain(&overlay.0).cloned().collect())
}

service_contract! {
    pub contract TaggedLogger {
        id: "app/tagged-logger@1",
        intercept: LogTags,
        identity: LogTags::default(),
        merge: merge_tags,
        methods: [
            async fn info(message: String) -> Result<(), String>;
        ],
    }
}
```

提供方通过调用上下文读取元数据：

```rust
#[service_impl]
impl TaggedLoggerProvider for ConsoleLogger {
    async fn info(&self, call: CallContext<LogTags>, message: String) -> Result<(), String> {
        println!("[{}] {message}", call.intercept.0.join("/"));
        Ok(())
    }
}
```

使用方需要声明 `TaggedLogger` 依赖，然后从派生上下文取得客户端：

```rust
let tagged = ctx.intercept::<TaggedLogger>(LogTags(vec!["worker".to_owned()]));
let logger = tagged.service::<TaggedLogger>()?;
logger.info("ready".to_owned()).await.map_err(ActivationFailure::user)?;
```

`intercept` 不修改原上下文，也不选择不同的服务实现。已有客户端保存其创建时的元数据；
从派生上下文获取的新客户端可以携带新的元数据，仍使用本次运行固定的提供方。

需要让子组件启动时就携带元数据，可先对挂载用的上下文调用 `intercept`。组件声明也支持
`requires = [TaggedLogger => LogTags(...)]`，设置该依赖的初始值。合并顺序为声明值，随后
从外到内逐层合并上下文附加值；没有声明值时从 `identity` 开始。

`identity` 与 `merge` 应是非阻塞、不 panic 的纯值运算，满足左右单位元和结合律，并保持
输入值不变。用契约自己的测试验证这些规则。元数据登记后应保持不可变值语义，外部别名
不能暗中修改合并结果。Linorun 负责传递信息，字段的业务含义和执行由服务实现负责。

没有元数据需求时，整体省略 `intercept`、`identity` 和 `merge`，得到默认的 `()`。

## CallContext 的诊断字段

每次 Provider 方法都收到一个 `CallContext<I>`：

| 字段 | 含义 |
| --- | --- |
| `caller` | 调用方组件实例 |
| `caller_episode` | 调用方本次运行 |
| `provider` | 本次调用固定的提供方实例 |
| `binding` | 所使用的服务注册 |
| `intercept` | 合并后的调用元数据 |

这些字段适用于日志、追踪或服务自己的认证适配，不改变依赖目标的相等性。没有需要时，
Provider 方法可以用 `_` 忽略整个 `CallContext<()>`。

## 显式实现 Provider

Provider trait 使用可动态分发的装箱 Future。需要手写实现时，上面 `LoggerProvider` 的
方法形式是：

```rust
use std::{future::Future, pin::Pin};

impl LoggerProvider for ConsoleLogger {
    fn info<'a>(
        &'a self,
        _: CallContext<()>,
        message: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            println!("{message}");
            Ok(())
        })
    }
}
```

此代码与 `#[service_impl]` 示例是同一实现的两种写法，选择其中一种即可。

每个调用 Future 都会持有 endpoint 所需的引用直到调用完成。调用本身应在使用方的工作
或清理中等待完成；返回流或启动其他异步工作的契约，应自行定义这些工作的归属和结束
条件。提供方整体退出的撤回保护以使用方结束其已提交依赖的运行为边界，不单独统计每个
在途 RPC 或流；手动撤回单项注册则使用上文固定的使用方 Episode 边界。

## 自定义 ServiceContract 与底层客户端

需要手写契约适配器时，`ServiceContract` 将以下内容连接起来：

```rust
pub trait ServiceContract: Sized + Send + Sync + 'static {
    const ID: &'static str;
    type Provider: Send + Sync + 'static;
    type Intercept: Clone + Send + Sync + 'static;
    type Client: Clone + Send + Sync + 'static;

    fn identity_intercept() -> Self::Intercept;
    fn merge_intercept(base: &Self::Intercept, overlay: &Self::Intercept)
        -> Self::Intercept;
    fn erase(provider: Self::Provider) -> Arc<dyn ErasedEndpoint>;
    fn client(reference: ServiceRef<Self>) -> Self::Client;
}
```

上面是接口形状，`ErasedEndpoint` 与 `ServiceRef` 来自 `linorun_core`。普通应用使用宏即可；
自定义契约仍可直接实现该 trait。

`ServiceRef<C>` 由 Core 创建，保存固定 endpoint、调用身份和元数据。契约的本地 Client
类型持有它，再提供具体方法。客户端类型属于契约所在 crate，因此下游不需要为
`linorun-core` 的外部类型增加固有方法。

宏生成的 endpoint 将 Provider 包在私有适配器中，通过 `dispatch_parts` 与 `as_any`
完成类型恢复。它们是公开但隐藏于常规文档的适配接口；普通组件使用类型化客户端。
`erase` 与 `client` 只做所有权包装和类型适配，必须非阻塞、不 panic、不建立外部资源。
契约实现负责保证自己的 endpoint 与客户端匹配。

同一服务 ID 使用不同契约类型、不同 intercept 类型或不匹配的已声明依赖，会返回
`ServiceTypeMismatch`。Runtime 先验证类型再提交挂载或服务注册，失败不提交部分拓扑。
已构造客户端的每次调用不会重新查询最新 Binding。

更完整的错误契约、路由和所有权定义见 [Core API](core-api.md)、
[核心数据模型](../architecture/data-model.md)和 [运行时规范](../reference/runtime-specification.md)。
