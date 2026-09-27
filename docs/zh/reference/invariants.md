[简体中文](invariants.md) | [English](../../en/reference/invariants.md)

# 不变量速查

本文件用于实现 review、debug assertion 和测试。

## I1：Inverse 至多调用一次

~~~text
forall inverse:
    call_count(inverse) <= 1
~~~

实现条件：

- accumulator entry 直接唯一持有 InverseAction，User 变体内部持有 FnOnce；
- scheduler 用 Vec::remove take entry，User action 随后只由一个 RuntimeTask 持有；
- 完成后 entry/action 不放回 accumulator；
- 所有 inverse ownership 汇合到 Fiber.cleanup_running、RuntimeTask 和 Episode accumulator；
- FiberHandle 的 Host O-Retire 只能在同一 reducer 中 take 精确匹配的
  `RetireFiber(child)` entry，不能满足其他 action；父 scheduler 已先选择时，
  Host 只等待 FiberCompletion。

## I2：严格逆序、串行回收

~~~text
forall fiber:
    running_inverse_count <= 1

when starting inverse x:
    x.seq == max(armed_requested_seq)
~~~

实现条件：

- Fiber 只有一个 `cleanup_running`；
- Fiber-wide cleanup 等全部 effect driver 到达边界；
- completion 前不选择下一个 inverse；
- error completion 也清除 `cleanup_running` 并继续。

## I3：先 Leave，后 Provider cleanup

~~~text
Fiber-wide Provider inverse starts
    =>
Provider lifecycle == Unloading
and no registered BindingRecord belongs to Provider
~~~

实现条件：

- 进入 Unloading 的 reducer 先写 lifecycle、删除 BindingRecord 并刷新 watcher；
- Fiber-wide inverse 只在 withdrawal guard 释放后启动。

手动 provide dispose 选择该 inverse 后、发布 InverseStarted 前，Actor 删除对应 BindingRecord 并刷新 watcher。

## I4：先 Consumer，后 Provider

~~~text
Provider Fiber-wide accumulator starts its first inverse
    =>
forall installed consumer:
    consumer.committed does not reference provider Fiber
~~~

实现条件：

- L-Begin 添加 committed dependency；
- L-Unload 删除 committed dependency；
- is_relied_on 是 cleanup 前置条件。
- Fiber 为 Unloading 时，尚未开始的 Disposing effects 也受同一前置条件约束；此前
  已运行的 manual inverse 先落地。

## I5：CommittedView 固定

~~~text
during one Episode:
    committed.key is constant
    committed.entries are constant
~~~

实现条件：

- CommittedView 使用 Arc；
- Episode 的 committed 字段在构造后保持不可变；
- service lookup 只读 Episode.committed。

## I6：Target 只比较 Provider 身份

~~~text
Target = ordered Vec<FiberId>
~~~

实现条件：

- target builder 只读取 Requirement、Binding visibility 和 provider；
- BindingId、endpoint、value、metadata 不进入 Target；
- FiberId 在 Runtime 生命周期内不复用。

## I7：转换具有 inertia

~~~text
forall fiber:
    running_lifecycle_step <= 1
    episode_count <= 1
~~~

实现条件：

- Reloading 保存唯一 StepId；effect 只保存一个 `running_step`；
- Actor 只保存 identity；driver 由一个 RuntimeTask 独占，并随 step completion 把
  所有权交回 Actor；只有 has_next Yield 才能再次 poll；
- StepId 与 RuntimeTask ownership 在同一 Actor 提交中建立，该提交就是 step start；
  首次 executor poll 之前不存在另一条可取消状态；
- Target 变化只改变下一 fixed-point pass 读取的最新意图；
- in-flight step completion 后再决定 next state；
- Unloading 完成前不创建新 Episode。

`Activation::run` 保留整个 body 的单 step inertia；`on_cleanup` 不创建 activation boundary。
已有 step 在 shutdown/retire 后继续落地时，同一 Episode、尚未开始 unload cleanup 且仍有
in-flight step 的 `RegisterCleanup` 收尾登记可以继续；这不开放新的外部工作或过期 Scope。

## 结构不变量

### ComponentDescriptor

- requirements 中 ServiceId 唯一且顺序稳定；
- provides 是 ServiceId set；
- ComponentId 在 descriptor 生命周期内固定。

### Fiber tree

~~~text
fiber.parent == P
    => P.children contains fiber

P.children contains C
    => C.parent == P
~~~

每个 parent 已在 registry；O-Remove 前 children 为空。
Synthetic Root 和 retired Fiber 的 mount_effect 为 None；每个未 retired 的普通 Fiber，其
mount_effect 指向 parent 当前 Episode 中唯一的 mount EffectRecord；该 record 为 Armed 或
Disposing，对应 InverseEntry 为 Armed 或 Running。

### Binding

~~~text
one BindingSlot -> at most one registered BindingRecord
binding.provider exists
binding.owner_effect exists
binding.contract_type == contracts[binding.slot.service].contract_type
~~~

WithdrawBinding 只在 Slot 当前 BindingId 与 action 匹配时删除记录；旧 action 不能删除同
Slot 后来注册的替代 Binding。

visibility：

~~~text
visible(binding)
    iff binding is registered
        and provider.state == Active
        and !provider.retired
~~~

### Watcher

每个 Fiber Requirement 解析出的 Slot 都包含该 Fiber。Fiber remove 后不再出现于 watcher。

### Episode

- Inactive 不持有 Episode；
- Reloading、Active、Unloading 各持有一个 Episode；
- Episode.id 与 EffectRecord.episode 一致；
- Episode implementation/config snapshot 在 episode 内固定；
- `cleanup_running` 与 cleanup report collectors 常驻 FiberRecord。

### Effect

- inverse seq 在 Fiber 内唯一；
- Episode.accumulator 唯一持有 InverseAction；User action 唯一持有 InverseFn；
- Running inverse 已从 accumulator take，其 action 只在 task/cleanup_running 路径上；
- Effect record 移除表示它的全部 inverse 已完成；
- requested Effect 在 forward boundary 且 accumulator/cleanup_running 中没有 inverse 时必须直接完成；零
  inverse effect 不创建占位 inverse；
- registered Binding.owner_effect 指向同 Fiber/Episode 的 EffectRecord。

### Committed dependency

- CommittedEntry 保存完整 BindingId、provider、slot 和 endpoint snapshot；
- CommittedEntry.contract_type 与 Requirement.contract 和 Runtime contract map 一致；
- 当前 store 若仍含同一 BindingId，其 provider 和 slot 与 entry 一致；
- Requirement index 唯一且顺序稳定。

## 转换前后条件

### L-Begin

前：

    Inactive { failure: None }
    cleanup_running == None
    latest key exists

后：

    Reloading
    committed.key == captured latest key
    accumulator empty

### L-Iter

前：

    Reloading
    completion matches EpisodeId
    latest key == episode.key

后：

    new inverse Armed
    seq increases
    returned driver moved into exactly one next RuntimeTask

### L-Finish

前：

    Reloading
    activation done
    latest key == episode.key

后：

    Active
    registered Provider Binding becomes visible

### L-Divert

前：

    Reloading at boundary
    latest key != episode.key

后：

    Unloading
    owned BindingRecord removed
    never transiently Active

### L-Raise

前：

    Reloading step error

后：

    Unloading with ActivationFailed outcome carrying the failure revision
    owned BindingRecord removed
    accumulated inverse retained

### L-Leave

前：

    Active
    latest key != episode.key

后：

    Unloading
    owned BindingRecord removed
    watcher observes the next fixed-point pass

### L-Unload

启动前：

    Unloading
    relied == false
    all forward drivers at boundary

启动后：

    lifecycle remains Unloading
    scheduler takes accumulator entries by descending seq

完成前：

    accumulator empty
    cleanup_running == None
    no live EffectRecord for Episode

完成后：

    Inactive
    no registered BindingRecord for Episode
    cleanup_running == None
    Episode dropped
    committed dependency removed

## 派生的图约束

根据服务可见性、Episode 和退出顺序的约束，可以推导两个可观察结果：

- 互需 Component 没有环外 visible Provider 时，相关 Fiber 保持 Pending，不产生 committed edge；
- 已 committed 的依赖边不会形成环，withdrawal guard 不会因依赖环产生结构性死锁；用户
  Future 永不结束仍可让它持续等待。

这两条是 I1-I7 的推论，不新增 Core 不变量，也不要求运行时添加环检测或打破机制。

实现中的时序依据是：L-Begin 只捕获已经 Active 的 Provider，所以每条 committed edge
指向更早开始的 Provider Episode；Consumer 仍 installed 时，withdrawal guard 阻止该
Provider Episode 完成后换成新 Episode。沿 committed edge 的 Episode 开始顺序严格变早，
不能绕回原点。这一论证不覆盖任意 watcher 等待、parent removal 或用户 Future 自行建立的
等待关系，不能据此宣称任意用户 await 都不会死锁。

因此手动 provide withdrawal 通知全部相关 watcher，但只把精确 committed 到该 BindingId
的 Consumer Episode 加入等待，捕获 `(FiberId, EpisodeId)`。单纯 watcher 不是依赖，不能
凭它添加等待边。捕获的 Episode 结束，或同一 Episode 达到 Active 且 key 最新时，等待
完成；后续 Episode 不延长等待。这样手动等待也沿真实 committed 边，不在其之外形成
纯 Core 回收等待环。它不等待公共 settled 或 child O-Remove，也不改变 I4 对 Fiber-wide
cleanup 的 installed Consumer guard。

## 自动检查

Core reducer 单元测试直接检查 crate-private lifecycle/topology record；确定性场景在选定边界调用
`Scenario::snapshot()`，它先取得 Actor-consistent RuntimeSnapshot，再由
`linorun_testkit::check_snapshot` 验证：

- Fiber tree 与 mount effect ownership；
- Binding visibility、owner effect 与 committed consumer index；
- `relied_by` 和 committed Provider edge 的双向一致；
- Episode/Effect identity 与 inverse owner 投影；
- Fiber-local seq 唯一且至多一个 Running inverse。

检查器只读取 DTO，不修改或修复 Runtime 状态。
