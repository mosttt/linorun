[简体中文](context-isolation-interception.md) | [English](../../en/semantics/context-isolation-interception.md)

# Context、isolate 与 intercept

## 1. Context 的职责

Context 是组件对当前 Episode 的只读视图：

- 从 CommittedView 获取已声明 service；
- 派生 isolation route；
- 派生 interception metadata；
- 携带当前 Fiber/Episode 调用归属。

可变操作放在 Scope：effect、provide 和 mount。Context 与 Scope 共同组成
ComponentContext，但 service 读取始终受 Requirement 约束。

ComponentContext 的便捷方法委托给这两个既有入口；`on_cleanup` 为已取得资源登记 inverse。
它保留 Episode 身份校验，并允许同一 Episode 的 in-flight step 在 shutdown 或 retire 开始
后完成必要登记，具体条件见 [Effect API 约束](effects.md#12-effect-api-约束)。派生或克隆
Context 不会使过期 Scope 重新有效，也不会改变 activation step 的边界。

## 2. 不可变派生

~~~rust
struct ContextRoute {
    parent: Option<Arc<ContextRoute>>,
    isolate_delta: Map<ServiceId, RealmId>,
    intercept_delta: Map<ServiceId, InterceptValue>,
}
~~~

Context::isolate 和 Context::intercept 都返回新 Context：

~~~rust
let tenant = ctx.isolate::<Database>(tenant_realm)?;
let traced = tenant.intercept::<Http>(trace_metadata);
~~~

原 Context 不变化。Route 的 parent 只用于 isolation 和 intercept metadata 的解析，
不是 service capability 继承链。

## 3. Realm resolution

对 ServiceId S：

1. 从当前 route 向 parent 查找最近的 `isolate_delta[S]`；
2. 若没有覆盖，使用 Runtime 的 default Realm；
3. 构造 `BindingSlot(S, Realm)`。

同名 service 在不同 Realm 中不冲突。RealmId 由同步的 RootContext::create_realm 通过
RuntimeHandle 原子分配，并绑定到一个 Runtime；Context::isolate 只接受同一 Runtime 的
RealmId。RootContext/Context::default_realm 返回同一 RuntimeTag/local 0；在外层 isolate 后，
把它传入内层 isolate 可把某个 ServiceId 的后续路由重设回 default。分配或读取这些 RealmId
不修改 Fiber/Binding 状态。

## 4. mount

在派生 Context 上 mount child：

- Context 必须由当前 ComponentContext 派生；
- child 保存该 route；
- child requirements 使用该 route 解析；
- watcher 注册到解析后的 Slot；
- route 在 child Fiber 生命周期内固定。

Fiber 的 parent 只记录 mount/removal 所属关系。创建一个新派生 Context 不会改变已有
Fiber 的 Target。

当前 Fiber 的 `Context::service` 仍读取该 Episode 已提交的 entry；在同一 Context 上后来
派生 isolate 不会把这个 lookup 重定向到另一 Realm。另一 Realm 的读取必须由在该 route 下
mount、并以自己的 Requirement 提交依赖的 child Fiber 完成。

## 5. provide

Scope::provide 显式接收一个由当前 ComponentContext 派生的 Context，并使用其 route：

~~~text
root.provide(DB)                         -> default realm
scope.provide(&tenant_ctx, DB)           -> tenant realm
~~~

descriptor 中 provides 只声明 ServiceId；实际 BindingSlot 由调用 route 决定。

## 6. declared service lookup

当前 Fiber 声明了 ServiceId 时：

1. 在 CommittedView 中按 Requirement index 查找；
2. 构造 entry.endpoint 的 opaque ServiceRef，并包装为 contract-local `C::Client`；
3. 不查询最新 Binding store；
4. Unloading 中仍返回同一 entry。

service 不在 requirements 中时，lookup 返回 `ServiceAccessError::UndeclaredDependency`。
Context 持有创建时的 CommittedView；组件必须遵守 Episode 使用期，Core 不在同步 lookup
中查询 Actor 当前状态。Core 也不沿 parent Fiber 或 mount Context 隐式查找未声明 service。

组件只能读取声明的服务依赖。需要参与 activation、reload 或 cleanup ordering 的 service
必须成为 Requirement。

## 7. Interception

Interception 是 service-specific metadata：

~~~rust
pub trait ServiceContract: Sized + Send + Sync + 'static {
    type Intercept: Clone + Send + Sync + 'static;

    fn identity_intercept() -> Self::Intercept;
    fn merge_intercept(
        base: &Self::Intercept,
        overlay: &Self::Intercept,
    ) -> Self::Intercept;
}
~~~

`Requirement.declared_intercept == None` 时，合并基点是 `identity_intercept()`。有效
metadata 的顺序是：

1. identity；
2. declared metadata；
3. route 上从外到内的 overlays。

merge_intercept 必须以 identity_intercept 为左右单位元并满足结合律，使分层 metadata
能够按固定顺序合成。

Core 的 erased metadata 同时带有 contract_type 与 intercept_type。mount 校验 child Requirements
对应的 declared/route metadata；provide 只校验泛型 `C` 的 contract identity，不读取无关
route intercept。缺少的 `ServiceId -> Contract TypeId/vtable` 映射与操作结果在同一 Actor
提交中写入，校验失败不改变 RuntimeCore。`Context::service` 还会校验该 Context 后来派生的
overlay，并按上述顺序构造 Client/ServiceRef。

示例：

~~~text
HTTP identity: timeout = none
Requirement: timeout = 30s
outer route: trace_id = abc
inner route: timeout = 5s
effective: trace_id = abc, timeout = 5s
~~~

## 8. Intercept 不改变 topology

intercept 不参与 BindingSlot、Requirement satisfaction、Target equality、watcher 注册
或 reload 决策。因此从 ctx 派生 traced_ctx 不会使现有 Fiber restart。

## 9. Typed Client 与 ServiceRef

ServiceRef 保存 committed endpoint、合并后的 interception metadata 和 caller/Episode
诊断字段。字段私有，并由 ServiceContract 包装为 contract-local `C::Client`；调用经过宏
生成的 adapter。

Client/ServiceRef 的使用期归属于创建它的 Episode。组件必须把异步调用、stream 和缓存句柄纳入
该 Episode 的 effect 与 cleanup，并在 Episode 完成后停止使用它。

## 10. RootContext

RootContext 用于 Host：

- 创建顶层 effect；
- mount 顶层 Component；
- provide Host-owned service；
- 取得 default Realm、创建 Realm 并派生 route。

Root effect 仍进入同一 Episode accumulator。RootContext 不是绕过 effect tracking 的管理
后门。

## 11. Isolation 的边界

Realm 定义进程内 service resolution domain，用于决定 Requirement 和 provide 对应的
BindingSlot。Realm 不改变 Component 的静态 Requirement 集合，也不构成额外的租户权限
系统；多租户隔离由上层 Runtime/Realm 管理策略负责。
