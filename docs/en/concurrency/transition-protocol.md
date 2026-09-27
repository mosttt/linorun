# Transition protocol

[简体中文](../../zh/concurrency/transition-protocol.md) | English

## 1. States and triggers

| Current state | Condition | Rule | Next state |
|---|---|---|---|
| Inactive { failure: None } | A key exists | L-Begin | Reloading |
| Reloading | Step yields, key equal | L-Iter | Reloading |
| Reloading | Step done, key equal | L-Finish | Active |
| Reloading | Key differs at boundary | L-Divert | Unloading |
| Reloading | Step error | L-Raise | Unloading |
| Active | Key differs | L-Leave | Unloading |
| Unloading | Guard open, forward drivers at boundary | L-Unload starts | Unloading |
| Unloading | Episode cleanup complete | L-Unload completes | Inactive |
| Inactive { failure: Some(_) } | restart/update | Clear failure and reconcile | Inactive { failure: None } or Reloading |

## 2. Transition ownership

One EpisodeId identifies a complete transition episode:

    Reloading -> Active -> Unloading

A Fiber holds at most one Episode at a time.

Every activation, effect, and inverse completion carries FiberId, EpisodeId, and its own local identity.

## 3. L-Begin

Inside the Actor:

~~~text
key = compute_activation_key(fiber)
committed = resolve_requirements_and_capture_declared_metadata(key)
episode = Episode(key, committed, empty accumulator)
state = Reloading(episode, first StepId)
~~~

Outside the Actor:

    poll first activation step

Capturing CommittedView and writing Reloading are one control-plane action. Interception metadata captured here consists of validated declared values. Context combines effective metadata for new Clients outside the Actor.

## 4. L-Iter

The current step successfully produces:

    Yield { inverse, has_next: true } + returned driver

The Actor:

1. Gives the inverse a new seq.
2. Appends it to the accumulator.
3. Recomputes the latest key.
4. If keys match and the completion is `Yield { has_next: true }`, moves the returned driver into the next RuntimeTask as a continuation.
5. Otherwise destroys the returned driver after committing L-Divert/L-Finish as applicable.

The current inverse has been committed before each spawn. A step task polls one step at a time and returns driver ownership to the Actor with its completion; it never starts the next iteration independently.

The Actor commit that writes StepId and transfers the driver into RuntimeTask is the semantic start of the step. The first physical poll is not another state boundary. The next step can only be committed in the Actor turn for the previous completion. Another command cannot interleave within that turn's completion/continuation decision.

## 5. L-Finish

The final step succeeds:

1. Collect its inverse if the completion contains one.
2. Compare the latest key.
3. If equal, change Reloading to Active.
4. Provider Bindings become visible.
5. The next fixed-point pass exposes visibility changes to watchers.

Consumers can only resolve Providers that are already Active.

## 6. L-Divert

When a successful step completion returns:

1. State is initially still Reloading.
2. Collect the inverse if present.
3. Compare the latest key at completion time.
4. If different, enter Unloading without forwarding the returned driver.
5. If equal, continue according to has_next or finish.

Entering Unloading removes the Fiber's registered BindingRecords, marks all live Applying/Armed effects in the Episode Disposing, and refreshes watchers. The returned activation driver is released after commit. An effect task still owning its current driver continues to its step boundary.

Target churn is judged only by the final comparison at the landing boundary.

## 7. L-Raise

On a step error:

1. End the current activation driver.
2. Save the error and episode.key.revision in the outcome.
3. Change Reloading to Unloading.
4. Remove the Fiber's registered BindingRecords and refresh watchers.
5. Mark every live Applying/Armed effect in the Episode Disposing.
6. Wait for the withdrawal guard before running the accumulator.
7. After cleanup, enter `Inactive { failure: Some(error) }`, recording the failure only if its revision is still current.

A Reloading Provider was never Active, so new Consumers have not used its Bindings. It may itself consume other Providers and must still release those committed dependencies normally.

## 8. L-Leave

The Actor atomically performs:

~~~text
state = Unloading(old episode)
remove provider BindingRecord from store
mark episode effect drivers Disposing
next fixed-point pass revisits watchers and provider
~~~

After Leave:

- New Consumer Targets no longer contain the Provider.
- Existing Consumers still retain their CommittedViews.
- The Provider accumulator has not started.

## 9. Consumer reactions

### Active Consumer

Its Target changes, so it performs L-Leave.

### Reloading Consumer

Its current step lands, then it performs L-Divert at the boundary.

### Unloading Consumer

It continues existing cleanup.

### Inactive Consumer

It has no committed dependency and does not affect the guard.

## 10. Withdrawal guard

The Provider episode's guard is:

    relied(provider) == false
    and all episode effect drivers are at a boundary

Only after both conditions hold may the Provider accumulator run.

Unloading itself requests cleanup of the entire Episode. The Fiber remains Unloading throughout serial cleanup. Once the accumulator finishes, the Actor removes committed dependencies, drops the Episode, and enters Inactive.

## 11. Cleanup

~~~text
while armed inverse exists:
    select maximum seq
    run inverse
    await completion
    record outcome
~~~

Finally:

1. Complete shared results and remove requested EffectRecords that reached a forward boundary and have no remaining entry in the accumulator/cleanup_running. Zero-inverse effects converge the same way.
2. Verify that Episode-owned Bindings and live EffectRecords are empty.
3. Remove Consumer edges from consumers_by_provider.
4. Drop the CommittedView.
5. Drop the Episode.
6. Enter Inactive.

## 12. Dependency chains

A chain:

    A provides a
    B requires a, provides b
    C requires b

When A leaves:

1. A performs L-Leave; a is no longer visible.
2. B leaves.
3. B performs L-Leave; b is no longer visible.
4. C leaves.
5. C cleans up.
6. B's guard opens; B cleans up.
7. A's guard opens; A cleans up.

The resulting order is:

    C before B before A

## 13. Dependency branches

    A -> B
    A -> C

After A leaves, B and C may clean up in either Actor-scheduled order. A must wait until both leave the installed state.

Core specifies no total order among independent Fibers.

## 14. Switching Providers

    Consumer Active(P)
    P leaves
    Q becomes visible

Sequence:

1. The Consumer's latest key changes to Q.
2. Consumer cleanup still uses the CommittedView for P.
3. The Consumer completes L-Unload.
4. A new L-Begin captures Q.
5. The new Episode uses Q.

P is not replaced by Q within the same Episode.

## 15. Restart

Restart increments revision:

    key(P, revision 1)
      ->
    key(P, revision 2)

Provider identity is unchanged, but ActivationKey changes, causing a complete Leave/Unload/Begin sequence.

## 16. Dispose

O-Retire:

    retired = true
    latest key = None

The parent Episode's mount inverse or the host's FiberHandle::dispose performs this operation. The host path atomically marks retired, clears child.mount_effect, and takes only the pending mount entry whose action is exactly `RetireFiber(child)` from the accumulator. The parent cleanup path writes the same state when the scheduler selects that entry. The child then follows its current state through L-Divert or L-Leave and reaches O-Remove after cleanup and child removal. The FiberHandle waiter separately waits for FiberCompletion.

## 17. Manual effect disposal

For an Applying effect:

- Mark it Disposing.
- Let the current step land.
- Stop the continuation.
- Clean up the effect's entries.

For an Armed effect:

- Mark it Disposing.
- Enter the Fiber cleanup scheduler.

If the Fiber is already Unloading, the scheduler records the request and selects no inverse until the withdrawal guard and all forward boundaries are satisfied.

If the effect record has already been removed:

- Return the saved disposal result.

## 18. Manual provide disposal

1. DisposeEffect requests cleanup for the EffectId.
2. The scheduler selects its provide inverse.
3. The Actor removes the BindingRecord only if the Slot's current BindingId still equals that inverse's BindingId, and captures `(FiberId, EpisodeId)` pairs whose CommittedView holds that BindingId. A replacement Binding already in the Slot is unaffected.
4. If removal occurs, emit events in `BindingRemoved < InverseStarted` order.
5. Later fixed-point passes advance affected watchers. WithdrawBinding waits only until each captured Episode is no longer installed or the same Episode reaches Active with a key equal to the latest ActivationKey.
6. The inverse releases Core's endpoint reference and completes.
7. Publish the Effect completion and remove its record.

The Provider Fiber's other effects and Bindings keep their existing states.

A Fiber merely watching the same Slot does not block this inverse. Once a captured Episode is no longer installed, that Fiber cannot extend the old Binding's withdrawal wait by entering a new Loading Episode on another Provider. A retired Fiber waiting for child O-Remove does not extend it either. Republishing the same Slot from the same Provider can restore Target, allowing the original Episode to continue with its old endpoint at an Active boundary without waiting for the old BindingId to disappear from its CommittedView. EpisodeId identifies the subject of this wait only; it does not participate in Target equality.

## 19. Quiescence

A Runtime is quiescent when:

- No activation step is running.
- No effect forward step is running.
- No inverse is running.
- Actor state has reached a fixed point.
- No Fiber is Reloading or Unloading.
- Every Active Fiber's episode.key equals its latest key.
- Every Pending Fiber's latest key is None.
- Failed Fibers remain `Inactive { failure: Some(_) }` and count as settled.
- No retired Fiber remains in the registry awaiting removal.
- No O-Remove can be applied immediately.

`Runtime::wait_quiescent` completes when these conditions hold. Use `wait_settled` for a single Fiber.

## 20. Convenience registration boundaries

`Activation::run` wraps an ordinary setup result as Once. `on_cleanup` registers its callback as the User inverse of an independent Effect and waits for that forward step's boundary. Neither adds lifecycle states or bypasses the withdrawal guard, inertia, or serial LIFO. A derived ComponentContext route retains the same Scope owner. Child removal and cleanup ordering for declared dependencies continue to follow the parent tree and CommittedView respectively.

`Activation::run` still consists of one activation step. Awaiting on_cleanup creates no activation iteration boundary where an intermediate L-Divert can occur. Use `Activation::Steps` or `Activation::steps` for those boundaries. During exit, an already started step in the same Episode can register remaining resource inverses before its accumulator starts. This RegisterCleanup exception is for finishing only; ordinary Scope effect/provide/mount still reject new work.
