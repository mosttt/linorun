# 定义和使用服务

[简体中文](typed-service.md) | [English](../../en/tutorials/typed-service.md)

服务就是组件之间约定好的调用接口。例如业务组件依赖日志服务，不需要知道它写控制台还是文件。
声明依赖后，Linorun 会等待服务就绪，并负责服务更换时的组件重启顺序。

## 1. 定义调用方法

```rust
use linorun_core::{service_contract, service_impl, CallContext};

service_contract! {
    contract Logger {
        id: "app/logger@1",
        methods: [
            /// Writes one log entry.
            async fn info(message: String) -> Result<(), String>;
        ],
    }
}
```

这里的 Logger 是服务标识，LoggerProvider 是实现接口，LoggerClient 是调用端类型。
通常只需要在声明和 `ctx.service::<Logger>()` 中使用 Logger，不必手动创建 Client。
没有调用元数据需求时，不用写 intercept、identity、merge，默认类型是 `()`。

## 2. 编写实现

```rust
struct ConsoleLogger;

#[service_impl]
impl LoggerProvider for ConsoleLogger {
    async fn info(&self, _call: CallContext<()>, message: String) -> Result<(), String> {
        println!("{message}");
        Ok(())
    }
}
```

方法比声明多一个 CallContext 参数，其中包含调用方、提供者和调用元数据。当前例子用不到它，
所以命名为 `_call`。`#[service_impl]` 负责 Future 装箱，使用者只写普通 async 方法。
方法参数使用 owned 类型，例如 String；支持范围见[服务契约参考](../api/service-contracts.md)。

## 3. 在组件里声明并使用

```rust
use linorun_core::{component, ActivationFailure, ComponentContext};

#[component(id = "app/worker@1", requires = [Logger])]
async fn worker(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    let logger = ctx.service::<Logger>()?;
    logger.info("Worker started".to_owned())
        .await.map_err(ActivationFailure::user)?;
    Ok(())
}
```

有 requires 声明，组件才会等待 Logger 并取得正确的客户端。未声明就查找会返回
UndeclaredDependency；父组件提供了服务也不能省略该声明。
服务自己的 String 错误由组件映射为 ActivationFailure；Core 的调用错误可以直接用 `?`。

## 4. 安装并运行

把以上三段与下面的 main 放在同一程序中，使用[入门指南](../guides/getting-started.md)的依赖配置：

```rust
use std::sync::Arc;
use linorun_core::{Runtime, FiberState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::new(|task| { tokio::spawn(task); });
    let worker = runtime.mount(worker(), ()).await?;
    assert_eq!(worker.wait_settled().await, FiberState::Pending);
    runtime.provide::<Logger>(Arc::new(ConsoleLogger)).await?;
    runtime.wait_quiescent().await;
    assert_eq!(worker.state().await, FiberState::Active);
    assert!(runtime.shutdown().await.cleanup_errors.is_empty());
    Ok(())
}
```

先安装消费者时它处于 Pending；提供 Logger 后会自动启动。这里的 provide 返回一个 EffectHandle，
可手动撤回服务，也会在 shutdown 时自动撤回。

服务也可以由组件提供，声明 provides 即可：

```rust
#[component(id = "app/logging@1", provides = [Logger])]
async fn logging(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    ctx.provide::<Logger>(Arc::new(ConsoleLogger)).await?;
    Ok(())
}
```

需要异步关闭的后台任务、连接或文件，应先用 on_cleanup 登记关闭动作，再 provide 服务。
服务登记只管理可见性和引用，不能替你推导资源的关闭方法。

## 5. 服务停止时发生什么

提供者退出时，服务先停止参与新的依赖解析。消费者随后退出，它的清理函数仍然可以使用原来的
客户端完成最后一次调用；消费者清理结束后，提供者才关闭底层资源。

每次启动使用固定客户端，不会在某次调用中突然切换到另一个实现。新的提供者会在下一次启动时
被选中。服务值的变化本身不触发重启；运行库关注提供该服务的组件实例身份。

运行完整的后台服务示例：

```sh
cargo run -p linorun-core --example application
```

## 进阶：调用元数据与隔离

例如为日志附加标签，可以完整定义原有的 intercept 代数：

```rust
#[derive(Clone, Default)]
struct LogIntercept(Vec<&'static str>);

fn merge_log(base: &LogIntercept, overlay: &LogIntercept) -> LogIntercept {
    LogIntercept(base.0.iter().chain(&overlay.0).copied().collect())
}

service_contract! {
    contract TaggedLogger {
        id: "app/tagged-logger@1",
        intercept: LogIntercept,
        identity: LogIntercept::default(),
        merge: merge_log,
        methods: [
            async fn info(message: String) -> Result<(), String>;
        ],
    }
}
```

identity/merge 应为非阻塞、无 panic 的纯值操作，满足左右单位元和结合律。
元数据按组件声明、外层 Context、内层 Context 的顺序合并，不改变服务提供者或触发重启。
声明时可以写 `requires = [TaggedLogger => LogIntercept(vec!["worker"])]`。

宿主可以为不同场景建立不同服务命名空间：

```rust
let root = runtime.root();
let tenant = root.create_realm();
let tenant_root = root
    .isolate::<TaggedLogger>(tenant)?
    .intercept::<TaggedLogger>(LogIntercept(vec!["tenant"]));
```

每个服务在每个 Realm 中最多一个已注册提供者。Realm 是服务解析命名空间。
同一组件上派生 isolate 不会更换本次启动已选中的依赖；它影响之后的 mount/provide。
使用 `default_realm()` 可以显式回到默认路由。派生后的新 Client 会携带新的 intercept。

完整的手写 Component、Provider trait 和调用标签示例仍保留：

```sh
cargo run -p linorun-macros --example typed_service
```

其源码为 [typed_service.rs](../../../crates/linorun-macros/examples/typed_service.rs)。
手写 ServiceContract、ServiceRef、erased adapter、Scope/Context 和 component_descriptor! 仍可用于高级控制；
具体类型边界与手动撤回的等待条件见[服务契约](../api/service-contracts.md)和[服务语义](../semantics/services-and-coeffects.md)。
