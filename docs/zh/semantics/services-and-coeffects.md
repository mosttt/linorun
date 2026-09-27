[简体中文](services-and-coeffects.md) | [English](../../en/semantics/services-and-coeffects.md)

# Service、Binding 与 Coeffect

## 1. Service 是运行时 coeffect

Component 用 Requirement 声明它需要从环境获得什么：

~~~rust
struct Requirement {
    service: ServiceId,
    contract: ContractVTable,
    declared_intercept: Option<InterceptValue>,
}
~~~

Requirement 是 reactive coeffect：

- 满足时，Inactive Fiber 可以 Reload；
- 不满足时，Active Fiber 必须 Leave；
- Provider 身份变化时，Consumer 必须重新 activation。

Core 0.1 中 Requirement 全部为 Required。

## 2. Service identity

ServiceId 是稳定名称，例如：

    linorun/logger@1
    app/database@1

Rust TypeId 用于本构建产物内的 adapter 校验，ServiceId 是 service identity。

ServiceContract 负责把 ServiceId、endpoint 类型和 interception metadata 连接起来。
Runtime 对每个 ServiceId 固定一个 Contract TypeId/vtable；mount 校验 descriptor
Requirements 及其相关 route intercept，provide 校验泛型 `C` 的 contract identity。两者都拒绝
同名但 TypeId 不同的 contract，再把缺少的映射与操作结果原子提交；provide 不校验与该
Binding 无关的 route intercept。

Component descriptor 宏调用 doc-hidden `Requirement::new::<C>`；该 Core 泛型构造器把
`ContractVTable::of::<C>()` 和 typed declared metadata 写入 Requirement。宏与 Runtime 都不
尝试从 TypeId 反射或查找 Rust 类型。

## 3. BindingSlot

~~~rust
struct BindingSlot {
    service: ServiceId,
    realm: RealmId,
}
~~~

同一 ServiceId 在不同 Realm 中是不同 Slot。

每个 Slot 同时最多一个已注册 BindingRecord。Core 删除记录后，Slot 可以再次注册。

## 4. Binding visibility

BindingRecord：

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

Binding 可见条件：

    BindingRecord registered
    and provider lifecycle == Active
    and !provider.retired

因此：

- Reloading Provider 可以先构造 service，但 Consumer 不会提前看到；
- L-Finish 后 service 一次变为可见；
- Provider 进入 Unloading 时，BindingRecord 从 store 删除并立即退出新解析；
- Unloading 期间旧 Consumer 仍通过自己的 CommittedView 持有 endpoint。

## 5. Target

Target 计算：

1. 解析每个 Requirement 的 Realm；
2. 查找对应 Slot；
3. 检查 Binding visible；
4. 收集 provider FiberId；
5. 任一缺失则返回 None。

~~~rust
struct Target {
    providers: Vec<FiberId>,
}
~~~

Target equality 只比较 Provider FiberId。

示例：

| 变化 | Target 是否变化 |
|---|---|
| 同 Provider 原地更新内部值 | 否 |
| Provider A 换为 Provider B，值相等 | 是 |
| BindingId 改变但 ProviderId 相同 | 否 |
| interception metadata 改变 | 否 |
| Realm 改变导致解析到另一 Provider | 是 |

## 6. CommittedView

L-Begin 捕获：

~~~rust
struct CommittedEntry {
    binding: BindingId,
    slot: BindingSlot,
    provider: FiberId,
    contract_type: TypeId,
    endpoint: Arc<dyn ErasedEndpoint>,
    declared_intercept: Option<InterceptValue>,
}
~~~

CommittedView 同时承担：

- activation 中的 dependency lookup；
- Active 期间的稳定 service view；
- cleanup 期间的旧 Provider 访问；
- relied 关系的真值来源。

Runtime 不在 episode 中途把 entry 替换为新 Binding。

## 7. Watcher

每个 Requirement 对应一个 BindingSlot watcher：

    watchers[slot] contains consumer FiberId

以下事件触发相关 watcher reconcile：

- BindingRecord 插入；
- Provider L-Finish；
- 手动 dispose provide effect 删除 BindingRecord；
- Provider 进入 Unloading 并删除 BindingRecord。

Watcher 只表示“这个 Slot 变化可能影响 Fiber”。真实 committed dependency 由 CommittedView 表示。

## 8. Withdrawal

手动撤回 Binding：

1. EffectHandle 请求 dispose；
2. cleanup scheduler 选择该 effect 的 seq；
3. Actor 仅在 Slot 当前 BindingId 仍匹配时删除 BindingRecord 并 refresh watcher，替代
   Binding 不受旧 inverse 影响；
4. Consumer 根据自己的状态 L-Divert、L-Leave 或保持相同 Target；
5. provide inverse 只等待精确 committed 到被撤回 BindingId 的 Consumer Episode；
6. inverse 释放 Core 的 endpoint 引用并完成。

通知范围仍是该 Slot 的全部 watcher；等待集合则捕获持有该 BindingId 的 installed Consumer
及其 EpisodeId。没有 committed 依赖的 watcher 不加入等待，避免在已 committed 依赖图之外
凭空增加回收等待环。

每个等待在捕获的 Episode 结束时完成，包括内部 Inactive、后续 Episode 已开始或 Fiber 已
移除；后续 Episode 不延长等待。同一 Episode 若恢复为 Active 且 episode.key 等于最新
ActivationKey，也满足等待。retired Consumer 完成自身 L-Unload 后，即使 children 尚未
移除、公共状态仍为 Unloading，旧 Episode 也已结束。这不是 `FiberHandle::wait_settled`
的公共 settled 条件，不等待 child O-Remove。

该等待不要求 Active Fiber 上无关的独立 Scope effect 完成。Consumer 可能因同一
Provider 恢复而继续持有旧 Binding 的 CommittedView；endpoint Arc 保持有效。Consumer 的
activation failure 保留在该 Consumer，不转成 Provider cleanup failure。

Provider 整体退出时，L-Leave 删除它的全部 BindingRecord，再等待所有 committed Consumer。provide inverse 持有 endpoint Arc，直到它按 LIFO 顺序运行。

## 9. relied

~~~text
relied(provider P) =
    exists installed consumer C:
        C.id != P
        and C.committed contains entry.provider == P
~~~

installed 包括 Reloading、Active 和 Unloading。

这保证：

- Loading Consumer 已提交 Provider 后，Provider 不能先回收；
- Consumer cleanup 可以使用旧 endpoint；
- Consumer 完成 L-Unload、丢弃 CommittedView 后，依赖边才消失。


## 10. Loading 中的 Target 抖动

假设 Consumer 以 Provider P 开始 Loading：

1. P 暂时退出，Target 变为 None；
2. 同一个 P 在当前异步 step 落地前重新可见；
3. Target 又恢复为 P；
4. step 落地时 ActivationKey 仍相同；
5. Consumer 继续进入 Active；
6. Consumer 的 CommittedView 仍保存最初捕获的 endpoint。

这一恢复行为由相同 Provider 身份与步骤边界上的 ActivationKey 检查决定。

旧 Binding 从 store 删除后，同 Slot 可以注册新 Binding。Target 仍只比较 Provider FiberId。

## 11. Typed Client 与 ServiceRef

Context 为 CommittedEntry 构造 Core-owned ServiceRef，再由 ServiceContract 包装为
contract-local typed Client：

~~~rust
pub struct ServiceRef<C: ServiceContract> {
    inner: Arc<ServiceRefInner<C>>,
}

pub trait ServiceContract: Sized + Send + Sync + 'static {
    type Client: Clone + Send + Sync + 'static;
    fn client(reference: ServiceRef<Self>) -> Self::Client;
}
~~~

ServiceRefInner 保存 committed endpoint、effective intercept 和 caller/provider diagnostic identity，全部字段保持私有。

Client/ServiceRef：

- 不重新查询 Binding store；
- 不参与 Target 计算；
- 不通过 Deref 暴露 erased endpoint；
- 构造时合并当前 Context 的 interception metadata；
- 只在所属 episode 的组件逻辑中使用。

调用方法定义在 contract 所属 crate 的 Client newtype 上；ServiceRef 的 doc-hidden adapter
hook 只供宏生成代码使用。


## 12. Service confinement

Scope::mount 传入的 Context 只决定 child 的 route、Realm 和 intercept。child 的 service
lookup 仍只能读取自己的 CommittedView；未声明 service 返回
`ServiceAccessError::UndeclaredDependency`。Fiber parent 关系只控制 mount/removal，不
创建隐藏的 reactive dependency。

## 13. 单 Provider 模型

每个 BindingSlot 同时对应一个 Provider。多后端服务由一个明确的 Broker Component 表示：

    Consumer -> Broker service -> backend set

Core 观察 Broker Provider 身份，Broker 的 service contract 管理后端集合与选择。
