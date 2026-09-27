[简体中文](fiber-lifecycle.md) | [English](../../en/semantics/fiber-lifecycle.md)

# Fiber 生命周期

## 1. Component 与 Fiber

Component 是定义；Fiber 是实例。Fiber 持有自己的：

- parent；
- descriptor；
- config 和 implementation snapshot；
- route；
- lifecycle state；
- Episode accumulator；
- child set。

多个 Fiber 可以共享同一个 Component implementation，但生命周期完全独立。

Runtime 还包含一个 synthetic Root Fiber。它没有普通 Component activation，初始为 Active，并拥有 RootContext 创建的全部顶层 effects。
Running 时它的 key 固定为空 Target/revision 0；ShuttingDown/Stopped 时 key 为 None。Root
完成 cleanup 后保持 Inactive，不重新 activation，也不 O-Remove。

## 2. 内部四态

~~~text
Inactive
   |
   | L-Begin
   v
Reloading
   |       |
   |       | L-Divert / L-Raise
   |       v
   |    Unloading
   |
   | L-Finish
   v
Active
   |
   | L-Leave
   v
Unloading
   |
   | L-Unload
   v
Inactive
~~~

公共状态按以下优先级投影；已移除和 retired 条件优先于普通 Inactive：

| 内部状态 | Public |
|---|---|
| registry 已移除 | Disposed |
| Inactive、retired、仍在 registry | Unloading |
| Inactive { failure: None } | Pending |
| Reloading | Loading |
| Active | Active |
| Unloading | Unloading |
| Inactive { failure: Some(_) } | Failed |

`Inactive { failure: Some(_) }` 同时 retired 时，在移除前显示 Unloading。Failed 是
settled，也可以存在于 quiescent Runtime。

## 3. Target

Fiber 的 latest ActivationKey 由：

- retired flag；
- Requirement resolution；
- activation revision；

共同决定。

retired 或任一 Required service 缺失时，没有 ActivationKey。

Target 本身只包含 Provider FiberId；revision 是 ActivationKey 的独立字段。

## 4. O-Insert

创建 Fiber：

1. 分配 FiberId；
2. parent 指向已有 Fiber 或 Root；
3. Lifecycle revision 为 0，state 为 `Inactive { failure: None }`；
4. next_inverse_seq 为 1，`cleanup_running` 为 None；
5. 注册 watcher；
6. 添加到 parent.children；
7. reconcile。

Fiber 在依赖未满足时保持 Pending。

## 5. L-Begin

当 Pending Fiber 有 ActivationKey：

1. 捕获 Binding；
2. 构造 CommittedView，并保存已验证的 declared interception metadata；
3. 捕获 implementation/config；
4. 分配 EpisodeId；
5. 建立空 accumulator；
6. 进入 Reloading；
7. 构造 Init driver，并在 Actor 提交后由第一 RuntimeTask 调用 `Component::activate`、poll
   第一 activation step。

从这一刻起 Fiber 是 installed，并会阻止其 Provider 过早 cleanup。

## 6. Reloading

Reloading 可以产生三类结果。

### 6.1 L-Iter

completion 为 `Yield { inverse, has_next: true }`、归还 driver，且最新 ActivationKey 仍等于
episode.key：

- 收集 inverse；
- 保持 Reloading；
- Actor 把返回的 driver 移入下一 RuntimeTask。

### 6.2 L-Finish

completion 为 final Yield 或 Done，且 key 仍相等：

- 收集 inverse；
- 进入 Active；
- Provider Binding 对新 Consumer 变为可见；
- refresh watcher。

### 6.3 L-Divert

边界时 key 不同：

- 当前成功 completion 携带的 inverse 仍先收集；
- 不把返回的 driver 转交给下一 RuntimeTask；
- 进入 Unloading；
- 删除本 Fiber 已注册的 BindingRecord；
- 把本 Episode 的全部 live Applying/Armed effects 标记为 Disposing；
- 在提交状态后析构返回的 activation driver。

如果 key 在异步 step 落地前恢复相等，则不 divert。

step 在 Actor 写入 StepId 并把 driver 转交 RuntimeTask 的提交时已经开始；即使执行器还没有
首次 poll，之后的 key 变化也必须等待该 step completion。

### 6.4 L-Raise

step 返回错误：

- 停止 activation；
- 进入 Unloading；
- 删除本 Fiber 已注册的 BindingRecord；
- outcome 保存 failure；
- 把本 Episode 的全部 live Applying/Armed effects 标记为 Disposing；
- 在提交状态后析构返回的 activation driver；
- 回收 accumulator。

## 7. Active

Active Fiber：

- 向新 Consumer 提供已注册 Binding；
- 使用固定 CommittedView；
- 可以创建普通 effect；
- 等待 latest ActivationKey 变化。

以下动作改变 ActivationKey：

- Provider 身份变化；
- Requirement 不再满足；
- config update；
- implementation update；
- restart；
- retire。

## 8. L-Leave

Active episode.key 与 latest key 不同：

1. 进入 Unloading；
2. 删除 Provider 已注册的 BindingRecord，使 Slot 可立即复用；
3. refresh Consumer；
4. 将本 Episode 的全部 live EffectRecord 标记为 Disposing；Applying driver 只完成当前 step；
5. 等待 in-flight step 和 withdrawal guard。

Leave 不执行 inverse。它只改变组件的拓扑可见性和生命周期状态。

已开始的 activation/effect step 落地时仍可通过专用 `RegisterCleanup` 登记已取得资源的
inverse；只有同一 Episode、尚未开始 unload cleanup 且仍有 in-flight step 时，才在
shutdown 或 retire 后允许这一收尾操作。该 inverse 加入相同的 accumulator 并参与回收，
不重新开放新的 effect、provide 或 mount。

## 9. Unloading

Unloading 分为逻辑上的两个阶段：

### 9.1 Waiting

等待所有 CommittedView 指向本 Provider 的 installed Consumer 完成 L-Unload。

Consumer 自己仍持有旧 CommittedView。

### 9.2 Recovering

guard 释放后：

- 等本 Episode 的全部 effect driver 到达边界；
- 按 seq 降序运行 inverse；
- 直接终结在 accumulator/cleanup_running 中已无 entry 的 requested EffectRecord，零 inverse effect 不产生伪
  inverse；
- 记录 cleanup error；
- 清空 live EffectRecord；
- 丢弃 CommittedView；
- 进入 Inactive。

这两个阶段由 LifecycleState::Unloading、`cleanup_running` 和 effect 状态共同表达。

## 10. Unloading 之后

### 正常退出

- retired：等待 child 清空并 O-Remove；
- latest key 为 None：保持 Pending；
- latest key 为 Some：立即 L-Begin 新 episode。

### Activation failure

进入 `Inactive { failure: Some(error) }`，公共状态为 Failed。

Failed 不自动 retry。restart、config update 或 implementation replace 清除 failure。

### Cleanup error

通过 InverseFinished 事件和当前适用的 Effect/Fiber/shutdown collector 记录，然后仍进入
正常终局。

## 11. O-Retire 与 O-Remove

dispose Fiber 首先执行 O-Retire：

    retired = true
    latest key = None

Active Fiber 正常 L-Leave；Reloading Fiber 在 step boundary L-Divert。

Fiber 只有同时满足下列条件才 O-Remove：

- retired；
- Inactive；
- effects 已结束；
- Binding 已删除；
- children 为空。

O-Remove 后 FiberHandle 报告 Disposed。

公开的 dispose 请求直接执行 O-Retire，并且只能把父级尚未运行、action 精确为
`RetireFiber(child)` 的 mount entry 从 accumulator 取出，不能满足其他 inverse。FiberHandle
另行等待 FiberCompletion；父 scheduler 已先选择时公开请求只等待，重复 dispose 共享 child
completion，不会留下第二条 Armed retire 路径。

O-Retire 一经 Actor 提交，restart/update/replace 和新的 Scope mutation 返回
FiberDisposed/InactiveScope；state、wait_settled 和重复 dispose 继续工作。
前述同一 Episode 的 in-flight step 收尾 cleanup 登记仍可完成，不适用于过期 Scope。

## 12. Settled 与 quiescent

单个 Fiber 的 `wait_settled` 在完成可立即执行的 lifecycle reconcile，并达到以下任一
边界时完成：

- Inactive、无 failure、无 ActivationKey（Pending）；
- Active 且 episode.key 等于最新 ActivationKey；
- Inactive、带 failure（Failed）；
- 已从 registry 移除（Disposed）。

Unloading、Reloading，以及 retired 但仍等待 child 的 Fiber 都不是 settled。Runtime 的
`wait_quiescent` 还要求所有 Fiber 满足上述条件，并且没有任何 activation/effect forward
step、inverse、reconcile work 或待执行 O-Remove。

settled 只描述 Fiber lifecycle。Active Fiber 上独立 Scope effect 的 forward 或手动 cleanup
可以仍在运行；此时 `wait_settled` 可以完成，但 Runtime 不是 quiescent。

手动 provide withdrawal 通知全部相关 watcher，但只等待精确 committed 到被撤回 BindingId
的 Consumer Episode。捕获的 Episode 结束即完成等待（包括内部 Inactive、后续 Episode、
或已移除），后续 Episode 不延长等待；同一 Episode 恢复为 Active 且 key 最新也满足等待。
它不使用上述公共 settled 条件，也不等待 child O-Remove；未 committed 该 Binding 的
parent watcher 不会阻塞 child 的手动 withdrawal。
完整条件见 [Service withdrawal](services-and-coeffects.md#8-withdrawal)。

## 13. Parent/child

mount child 是父 Fiber 的 effect。父级 Fiber unload 按 accumulator seq 选择其 inverse 并
retire child；Host 也可通过 FiberHandle 先行 O-Retire，并取出精确匹配的 entry。两种情况
下 parent 都可以继续自己的更早 inverse，parent O-Remove 才等待 child 从 registry 移除。

Parent tree 与 dependency graph 是不同关系：

- parent 表达谁创建谁；
- committed dependency 表达谁必须先于谁 cleanup。

仅有 parent 关系不自动提供 service cleanup ordering。需要顺序时使用 Requirement。

## 14. Restart

restart：

1. 清除 activation failure；
2. activation_revision 加一；
3. 若当前处于 Unloading，使尚未提交的 failure outcome 失效；
4. reconcile。

Active Fiber 完整执行 Leave/Unload/Begin，不原地再次调用 activate。

Loading Fiber 不并行启动新 activation；当前 step 落地后 divert，再按新 revision Begin。

## 15. Config 与 implementation update

update 写入 Fiber 的 latest snapshot 并增加 revision。

进行中 Episode 继续持有旧 snapshot，直到 L-Unload 完成。新 Episode 捕获新 snapshot。

如果 failure 在 Unloading 中等待，只有 failure revision 仍等于最新 activation_revision 时，
L-Unload 才会把它写入 Inactive；更新操作因此不会被旧失败覆盖。

Descriptor 与 route 在 Fiber 生命周期内固定。`replace` 要求同一 `'static` descriptor；
`update_config` 要求输入 TypeId 等于 descriptor.config_type。Requirement、provides、Config
类型或 route 改变时 retire 旧 Fiber 并 mount 新 Fiber。

## 16. Inertia 示例

### 目标消失且未恢复

    t0 Loading(P)
    t1 P disappears
    t2 current step lands
    t3 L-Divert
    t4 cleanup
    t5 Pending

### 同 Provider 在落地前恢复

    t0 Loading(P)
    t1 P disappears
    t2 P reappears
    t3 current step lands
    t4 key still equals P
    t5 Active

### Provider 换成 Q

    t0 Loading(P)
    t1 target becomes Q
    t2 current step lands
    t3 L-Divert and cleanup P episode
    t4 L-Begin(Q)

所有轨迹始终只有一个 transition。
