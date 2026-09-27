# 编写组件

[简体中文](component-authoring.md) | [English](../../en/api/component-authoring.md)

组件负责启动一项功能，并告诉 Linorun 如何在停止时收回资源。通常只需要一个异步函数：

```rust
use std::sync::Arc;
use linorun_core::{component, ActivationFailure, ComponentContext};

struct WorkerConfig {
    name: String,
}

#[component(id = "app/worker@1")]
async fn worker(
    ctx: ComponentContext,
    config: Arc<WorkerConfig>,
) -> Result<(), ActivationFailure> {
    let name = config.name.clone();
    println!("start {name}");

    ctx.on_cleanup("stop worker", move || async move {
        println!("stop {name}");
        Ok(())
    }).await?;

    Ok(())
}
```

`worker()` 创建组件定义；`runtime.mount(worker(), config).await` 启动一个实例。函数返回
`Ok(())` 表示启动完成，组件会继续保持运行，已登记的清理操作留到停止时执行。
不需要配置时，省略第二个参数，挂载时传 `()`。

完整启动程序见[开始使用](../guides/getting-started.md)。本页先介绍日常写法，最后介绍
`Component`、分步启动和 `Context`/`Scope` 的进阶接口。

## 先登记清理，再继续启动

调用 `ctx.on_cleanup(...).await?` 后，清理操作已经登记完成。顺序登记的两个操作：

```rust
ctx.on_cleanup("close database", move || async move {
    database.close().await?;
    Ok(())
}).await?;

ctx.on_cleanup("stop worker", move || async move {
    worker.stop().await?;
    Ok(())
}).await?;
```

停止时先等待 `stop worker` 完成，再执行 `close database`。其中 `database` 和 `worker`
代表应用自己的资源，关闭方法的错误需要能转换为 `CleanupError`。

同一个组件实例内，Linorun 对已登记的清理操作按登记顺序的逆序串行执行，每个操作至多
执行一次。某项清理失败会记录错误，然后继续清理更早的资源。不同组件没有一个统一的
全局清理顺序，它们之间的必要顺序由声明的服务依赖和父子关系决定。

资源建立后应立即登记对应清理；后续代码返回错误时，Linorun 会回收已经登记的部分。
如果资源建立到登记之间就发生错误或 panic，运行时无法推断该资源该如何关闭。

如果 `dispose` 或 `shutdown` 在已开始的启动步骤或 effect 步骤中途发生，该步骤仍可收尾：
只要还是同一 Episode、尚未开始执行卸载清理，就能用 `on_cleanup` 登记刚取得资源的清理，
随后按原有顺序回收。这允许已有步骤交还资源，不允许再启动新的 effect、服务或子组件；
过期 Episode 的登记仍会被拒绝。

后台任务同样需要登记清理：清理函数发出停止信号，并等待任务退出。`Drop` 只负责普通
Rust 值的释放，异步关闭应写在清理函数中；丢弃组件句柄不会停止组件。

## 调用其他组件提供的服务

先在属性里声明依赖，再从上下文取出客户端：

```rust
#[component(id = "app/reporter@1", requires = [Logger])]
async fn reporter(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    let logger = ctx.service::<Logger>()?;
    logger.info("reporter started".to_owned()).await
        .map_err(ActivationFailure::user)?;

    ctx.on_cleanup("reporter stopped", move || async move {
        logger.info("reporter stopped".to_owned()).await
            .map_err(linorun_core::CleanupError::user)?;
        Ok(())
    }).await?;
    Ok(())
}
```

这里的 `Logger` 定义见[定义和调用服务](service-contracts.md)。服务缺失时，组件等待，
函数尚不会执行；依赖就绪后才开始启动。读取未声明的服务会返回 `UndeclaredDependency`。
组件作为另一个组件的子组件，也需要自行声明依赖。

服务提供方整体停止时，Linorun 先让使用方退出。使用方清理期间仍能通过已有客户端或
`ctx.service()` 访问本次启动时的提供方；它不会突然切换到新提供方。新的提供方就绪后，
组件可以重新启动并取得新客户端。

## 向其他组件提供服务

在 `provides` 中声明服务，然后登记实现：

```rust
#[component(id = "app/logging@1", provides = [Logger])]
async fn logging(ctx: ComponentContext) -> Result<(), ActivationFailure> {
    ctx.provide::<Logger>(Arc::new(ConsoleLogger)).await?;
    Ok(())
}
```

如果实现拥有数据库连接池、监听端口或后台任务，应按以下顺序建立它们：

1. 建立底层资源，登记并等待其清理操作完成登记。
2. `ctx.provide::<Service>(provider).await?` 发布服务。

这样停止时先撤回服务，等使用方清理完毕，再关闭底层资源。`provide` 管理服务注册及其
引用；它不会替应用猜测如何异步关闭连接池或停止任务。

未在 `provides` 中声明的服务返回 `UndeclaredProvide`；同一服务命名空间已有实现时返回
`DuplicateBinding`。宿主可直接使用 `runtime.provide::<Service>(provider).await` 登记服务。

通过 `provide` 返回的句柄手动撤回单项服务时，只等待撤回时实际持有该 Binding 的使用方
及其当时那次运行。那次运行结束后，不会继续等待使用方后来重新启动的工作；仅关注相同
服务命名空间的组件会收到变更通知，但不进入等待集合。同一提供方恢复时，使用方也可以
在原运行重新稳定后继续使用旧客户端。详见[服务注册与资源关闭](service-contracts.md#服务注册与资源关闭)。

## 挂载子组件

```rust
ctx.mount(worker(), WorkerConfig {
    name: "child".to_owned(),
}).await?;
```

父组件负责子组件的生命周期，父组件退出时子组件也会退出。挂载返回的 `FiberHandle`
可以查询状态、更新配置或提前停止子组件。

需要为子组件选择独立的服务命名空间时，先派生上下文：

```rust
let local = ctx.isolate::<Database>(realm)?;
local.mount(tenant_worker(), tenant_config).await?;
```

`realm` 由同一个 Runtime 的 `root().create_realm()` 创建。该操作影响派生上下文的服务
注册和子组件路由，不会替换当前组件已经拿到的依赖。详见[常用概念](../guides/concepts.md)。

## 更新配置、重启和停止

挂载得到的句柄一直代表同一个组件实例：

```rust
let fiber = runtime.mount(worker(), WorkerConfig {
    name: "first".to_owned(),
}).await?;

fiber.wait_settled().await;
fiber.update_config(WorkerConfig { name: "second".to_owned() }).await?;
fiber.wait_settled().await;

fiber.restart().await?;
fiber.wait_settled().await;

let report = fiber.dispose().await;
```

`update_config` 和 `restart` 提交变更；等待启动或清理结束用 `wait_settled`。配置更新会先
清理旧的一次运行，再用新配置启动，不会修改正在运行的异步函数手里的 `Arc<Config>`。
配置与组件定义应当具有不可变快照语义，运行时可变业务状态放进受管理资源或服务实现。

`fiber.replace(component)` 可替换组件实现，要求沿用同一个静态 descriptor 和配置类型。
需要共享 descriptor 的不同实现时，使用下面的显式 `Component` 接口。

`dispose` 永久停止该实例并返回清理报告；整个程序退出前应显式等待
`runtime.shutdown().await`。已经开始的启动步骤和清理函数会运行到完成，运行时不强行
取消它们，因此这些函数需要有明确的完成条件。

## 独立撤销或分步建立资源

需要独立的资源句柄，或逐步建立可分别撤销的资源时，使用 `ctx.effect`：

```rust
use linorun_core::effect;

let handle = ctx.effect("worker", effect::once(|| async {
    println!("start worker");
    Ok(Some(effect::inverse(|| async {
        println!("stop worker");
        Ok(())
    })))
})).await?;

handle.wait_forward().await?;
```

`effect(...).await` 表示任务已交给 Runtime，启动工作可能仍在执行；`wait_forward` 等待
启动结束，返回 `Completed` 或因撤销结束的 `Disposed`。不要把收到句柄当成资源已经可用。
`on_cleanup(...).await` 已经等待清理操作登记，无需再次调用 `wait_forward`。

`effect::sync` 接收同步建立操作，`effect::once` 接收一个异步建立操作，`effect::iter`
和 `effect::steps` 分别接收同步迭代器与异步流，每次产出一个清理操作。在两个不能交换
顺序的 effect 之间使用 `wait_forward`；并发发起时，实际登记顺序由完成先后决定。

`handle.dispose().await` 提前撤销这个 effect。清理函数内不要等待同一组件另一个普通
effect 的 `dispose()`；让运行时安排它们的逆序清理。实际执行清理期间也不能再建立新的
受管理 effect、服务或子组件；临时工作应在清理函数返回前结束。

## 显式实现 Component

组件定义需要持有字段、共享 descriptor，或启动逻辑本身需要返回多步流时，可以直接实现
`Component`。常规异步启动可用 `Activation::run`：

```rust
use std::sync::Arc;
use linorun_core::{
    component_descriptor, Activation, Component, ComponentContext, ComponentDescriptor,
};

component_descriptor! {
    static WORKER: () {
        id: "app/custom-worker@1",
        requires: [],
        provides: [],
    }
}

struct Worker;

impl Component for Worker {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        &WORKER
    }

    fn activate(&self, ctx: ComponentContext, _: Arc<()>) -> Activation {
        Activation::run(async move {
            ctx.on_cleanup("stop worker", || async { Ok(()) }).await?;
            Ok(())
        })
    }
}
```

`Activation::once` 接收返回 `Result<Option<InverseFn>, ActivationFailure>` 的 Future；
`Activation::steps` 接收逐项返回 `Result<InverseFn, ActivationFailure>` 的流。多步启动
每产出一项清理操作，就给运行时一个检查依赖或配置变化的边界，适合需要逐项回收的复杂
建立过程。自定义资源驱动可以实现 `EffectDriver`，通过 `IntoEffectDriver` 交给
`ctx.effect`；协议和各 adapter 的返回类型见 [Core API](core-api.md)。

需要给依赖声明附加服务元数据时，两种组件声明都支持 `Service => value`：

```rust
component_descriptor! {
    static REPORTER: () {
        id: "app/custom-reporter@1",
        requires: [Logger => LoggerIntercept::default()],
        provides: [],
    }
}
```

属性宏的对应形式为 `#[component(id = "app/reporter@1", requires = [Logger => value])]`。
重复声明、保留的 Root ID 等属于组件定义错误，在静态 descriptor 初始化时暴露。

## 显式使用 Context 与 Scope

`ctx.context()` 提供依赖读取和派生路由，`ctx.scope()` 提供资源登记和挂载。需要明确分开
传递这两个职责时，可以直接使用它们：

```rust
let context = ctx.context().isolate::<Database>(realm)?;
let scope = ctx.scope();
let child = scope.mount(&context, tenant_worker(), tenant_config).await?;
```

`scope.provide::<Service>(&context, provider)` 按指定路由注册服务，
`scope.effect(label, adapter)` 在当前组件登记资源。Context 和 Scope 必须属于同一
Runtime、组件实例和本次运行。它们与直接调用 `ComponentContext` 的方法使用相同的
生命周期规则，不建立另一套资源所有权。

## 处理错误

启动函数返回 `ActivationFailure` 时，已经登记的资源会被清理，该实例最终显示为
`Failed`，其他独立组件继续运行。Linorun API 错误可以通过 `?` 转换，业务错误可用
`ActivationFailure::user` 补充上下文。

清理函数返回 `CleanupError` 时，错误进入 `dispose` 或 `shutdown` 的报告，运行时继续
回收其他资源，不自动重试。不要在返回错误前再次手动执行已经交给运行时的清理操作。
对外发送的邮件、消息等不可撤销行为，应由业务定义补偿或幂等策略。

Component、配置、服务实现和 Future 的 `Drop` 应非阻塞且不 panic；服务客户端与受管理
资源的使用应在所属的这次运行结束前完成。完整的状态机和所有权约束见
[运行时规范](../reference/runtime-specification.md)。
