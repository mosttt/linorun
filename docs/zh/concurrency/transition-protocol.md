# 转换协议

简体中文 | [English](../../en/concurrency/transition-protocol.md)

## 1. 状态和触发条件

| 当前状态 | 条件 | 规则 | 下一状态 |
|---|---|---|---|
| Inactive { failure: None } | key 存在 | L-Begin | Reloading |
| Reloading | step yield，key 相等 | L-Iter | Reloading |
| Reloading | step done，key 相等 | L-Finish | Active |
| Reloading | boundary 时 key 不同 | L-Divert | Unloading |
| Reloading | step error | L-Raise | Unloading |
| Active | key 不同 | L-Leave | Unloading |
| Unloading | guard 开、forward driver 已到边界 | L-Unload 启动 | Unloading |
| Unloading | episode cleanup 完成 | L-Unload 完成 | Inactive |
| Inactive { failure: Some(_) } | restart/update | clear failure + reconcile | Inactive { failure: None } 或 Reloading |

## 2. Transition ownership

一次 EpisodeId 标识一个完整 transition episode：

    Reloading -> Active -> Unloading

Fiber 同时最多持有一个 Episode。

任何 activation completion、effect completion 和 inverse completion 都携带 FiberId、EpisodeId 和自己的局部 identity。

## 3. L-Begin

Actor 内：

~~~text
key = compute_activation_key(fiber)
committed = resolve_requirements_and_capture_declared_metadata(key)
episode = Episode(key, committed, empty accumulator)
state = Reloading(episode, first StepId)
~~~

Actor 外：

    poll first activation step

CommittedView 捕获和 Reloading 状态写入是同一个控制面动作。拦截 metadata 在此捕获已验证的声明值；
新 Client 的有效 metadata 由 Context 在 Actor 外合成。

## 4. L-Iter

当前 step 成功产生：

    Yield { inverse, has_next: true } + returned driver

Actor：

1. inverse 获得新 seq；
2. 追加 accumulator；
3. 重新计算 latest key；
4. key 相等且 completion 是 `Yield { has_next: true }` 时，把返回的 driver 作为 continuation
   移入下一 RuntimeTask；
5. 否则在提交 L-Divert/L-Finish 后析构返回的 driver。

每次 spawn 前都已经提交当前 inverse。
step task 一次只 poll 一步；driver 所有权随 completion 回到 Actor，task 不自行开始下一
iteration。

Actor 写入 StepId 并把 driver 转交 RuntimeTask 的提交，就是该 step 的语义起点；首次物理
poll 不是另一条状态边界。下一 step 只有在上一 completion 的 Actor turn 中才能被提交，
两次提交之间没有可被其他 command 插入的 driver boundary。

## 5. L-Finish

最后 step 成功：

1. completion 携带 inverse 时收集该 inverse；
2. 比较 latest key；
3. 相等则 Reloading -> Active；
4. Provider Binding 开始 visible；
5. 下一次 fixed-point pass 让 watcher 观察到可见性变化。

Consumer 只可能解析到已经 Active 的 Provider。

## 6. L-Divert

成功 step completion 返回时：

1. state 保持 Reloading；
2. completion 携带 inverse 时收集它；
3. 比较完成时的 latest key；
4. 不同则进入 Unloading，不再转交返回的 driver；
5. 相同则按 has_next 继续，或 Finish。

进入 Unloading 时删除本 Fiber 已注册的 BindingRecord、把本 Episode 的全部 live
Applying/Armed effects 标记为 Disposing，并刷新 watcher。返回的 activation driver 在提交后
释放；effect task 自己持有的当前 driver 继续到 step boundary。

Target 抖动只看落地边界的最终比较。

## 7. L-Raise

step error：

1. current activation driver 结束；
2. outcome 保存 error 和 episode.key.revision；
3. Reloading -> Unloading；
4. 删除本 Fiber 已注册的 BindingRecord 并刷新 watcher；
5. 本 Episode 的全部 live Applying/Armed effects 标记为 Disposing；
6. accumulator 等待 withdrawal guard；
7. cleanup 完成后 `Inactive { failure: Some(error) }`；只有 failure revision 仍是最新时才写入。

Reloading Provider 从未 Active，因此它自己的 Binding 没有被新 Consumer 使用；但它可能作为 Consumer 依赖其他 Provider，仍需正常释放 committed dependency。

## 8. L-Leave

Actor 原子完成：

~~~text
state = Unloading(old episode)
remove provider BindingRecord from store
mark episode effect drivers Disposing
next fixed-point pass revisits watchers and provider
~~~

Leave 后：

- 新 Consumer Target 不再包含 Provider；
- 旧 Consumer 仍持有 CommittedView；
- Provider accumulator 尚未运行。

## 9. Consumer reaction

### Consumer Active

Target 改变，执行 L-Leave。

### Consumer Reloading

当前 step 落地，在 boundary 执行 L-Divert。

### Consumer Unloading

继续已有 cleanup。

### Consumer Inactive

没有 committed dependency，不影响 guard。

## 10. Withdrawal guard

Provider episode 的 guard：

    relied(provider) == false
    and all episode effect drivers are at a boundary

条件满足后，才能运行 Provider accumulator。

Unloading 状态本身请求整个 Episode cleanup，Fiber 在串行回收期间保持 Unloading。
Accumulator 完成后，Actor 删除 committed dependency、丢弃 Episode 并进入 Inactive。

## 11. Cleanup

~~~text
while armed inverse exists:
    select maximum seq
    run inverse
    await completion
    record outcome
~~~

最后：

1. 对已经到达 forward boundary、且在 accumulator/cleanup_running 中已无 entry 的 requested EffectRecord
   完成共享 completion 并移除；零 inverse effect 也按此收敛；
2. 确认 episode-owned Binding 与 live EffectRecord 已清空；
3. 从 consumers_by_provider 删除 Consumer 边；
4. 丢弃 CommittedView；
5. 丢弃 Episode；
6. 进入 Inactive。

## 12. 连锁依赖

链：

    A provides a
    B requires a, provides b
    C requires b

A Leave：

1. A L-Leave，a 不再 visible；
2. B Leave；
3. B L-Leave，b 不再 visible；
4. C Leave；
5. C cleanup；
6. B guard 释放，B cleanup；
7. A guard 释放，A cleanup。

得到顺序：

    C before B before A

## 13. 分支依赖

    A -> B
    A -> C

A Leave 后，B 和 C 可以按 Actor 调度任意先后 cleanup。A 必须等二者都退出 installed。

Core 不为互不依赖的 Fiber 规定总顺序。

## 14. Provider 切换

    Consumer Active(P)
    P leaves
    Q becomes visible

流程：

1. Consumer latest key 变为 Q；
2. Consumer cleanup 仍使用 P CommittedView；
3. Consumer L-Unload；
4. 新 L-Begin 捕获 Q；
5. 新 Episode 使用 Q。

同一 Episode 内不替换 P 为 Q。

## 15. Restart

restart 增加 revision：

    key(P, revision 1)
      ->
    key(P, revision 2)

Provider identity 未变，但 ActivationKey 改变，因此执行完整 Leave/Unload/Begin。

## 16. Dispose

O-Retire：

    retired = true
    latest key = None

它由父 Episode 的 mount inverse 或 Host 的 FiberHandle::dispose 执行。Host 路径原子写入
retired、清除 child.mount_effect，并且只把 action 精确为 `RetireFiber(child)` 的 pending
mount entry 从 accumulator 取出；父 cleanup
路径在 scheduler 选中该 entry 时写入相同状态。Child 随后沿当前状态执行 L-Divert 或
L-Leave，cleanup 完成、children 清空后 O-Remove；FiberHandle waiter 另行等待 FiberCompletion。

## 17. Manual effect dispose

Effect Applying：

- 标记 Disposing；
- 当前 step 落地；
- 停止 continuation；
- cleanup effect entries。

Effect Armed：

- 标记 Disposing；
- 进入 Fiber cleanup scheduler。

若 Fiber 已是 Unloading，scheduler 只记录请求，等 withdrawal guard 与全部 forward boundary
满足后再选择 inverse。

Effect record 已移除：

- 返回保存的 dispose result。

## 18. Manual provide dispose

1. DisposeEffect 把 EffectId 加入 cleanup request；
2. scheduler 选择该 provide inverse；
3. Actor 仅在 Slot 当前 BindingId 仍等于该 inverse 的 BindingId 时删除 BindingRecord，并
   捕获 CommittedView 持有该 BindingId 的 `(FiberId, EpisodeId)`；已占用该 Slot 的替代
   Binding 不受影响；
4. 若发生删除，按 `BindingRemoved < InverseStarted` 发布事件；
5. 后续 fixed-point pass 推进受影响 watcher。WithdrawBinding 只等待捕获的 Episode 离开
   installed，或仍是同 Episode 且达到 Active、key 与最新 ActivationKey 一致的边界；
6. inverse 释放 Core 的 endpoint 引用并完成；
7. Effect completion 发布并移除 record。

Provider Fiber 的其他 effect 和 Binding 保持原状态。

仅 watch 同 Slot 的 Fiber 不阻塞此 inverse。捕获的 Episode 离开 installed 后，即使该
Fiber 已在另一 Provider 上进入新的 Loading，也不延长旧 Binding 的撤回等待；retired
Fiber 等 child O-Remove 也不延长等待。同 Provider 重新发布同 Slot 可以恢复 Target，
因此原 Episode 可以在 Active 边界继续持有旧 endpoint，无需等待旧 BindingId 从其
CommittedView 消失。EpisodeId 仅标识本次等待对象，不参与 Target equality。

## 19. Quiescence

Runtime quiescent：

- 没有 running activation step；
- 没有 running effect forward step；
- 没有 running inverse；
- Actor state 已到 fixed point；
- 没有 Reloading 或 Unloading Fiber；
- 每个 Active Fiber 的 episode.key 等于 latest key；
- 每个 Pending Fiber 的 latest key 为 None；
- Failed Fiber 保持 `Inactive { failure: Some(_) }`，并视为 settled；
- registry 中没有 retired 但尚未移除的 Fiber；
- 没有可立即应用的 O-Remove。

`Runtime::wait_quiescent` 在满足这些条件时完成；单 Fiber 使用 `wait_settled`。

## 20. 便捷登记的边界

`Activation::run` 将普通 setup 结果包装成 Once；`on_cleanup` 将 callback 登记为独立 Effect
的 User inverse，并等待该 forward step 到达边界。它们不增加生命周期状态，也不省略
withdrawal guard、inertia 或串行 LIFO。ComponentContext 的派生 route 保留相同 Scope owner；
子 Fiber 的 removal 与已声明依赖的 cleanup 顺序仍分别遵循 parent tree 和 CommittedView。

`Activation::run` 仍只有一个 activation step；await on_cleanup 不创建可中途 L-Divert 的
activation iteration boundary。需要逐段边界时使用 `Activation::Steps` 或 `Activation::steps`。
退出期间，同一 Episode 已开始的 step 可以在 accumulator 尚未开始前补登记资源 inverse；
该 RegisterCleanup 例外仅用于收尾，普通 Scope effect/provide/mount 仍拒绝新的工作。
