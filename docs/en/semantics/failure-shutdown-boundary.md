[简体中文](../../zh/semantics/failure-shutdown-boundary.md) | [English](failure-shutdown-boundary.md)

# Failures, shutdown, and system boundaries

## 1. Failure categories

Core distinguishes:

- Descriptor or API call errors.
- Activation failure.
- Inverse failure.
- User task panic.
- Aggregated failures during Runtime shutdown.

## 2. Descriptor/API errors

The following call-level errors are returned before changing state. They do not automatically mark a Fiber Failed:

- RuntimeShuttingDown.
- DuplicateBinding.
- UndeclaredProvide.
- UndeclaredDependency (Context::service).
- InactiveContext (Scope's Context ownership validation).
- InactiveScope.
- DescriptorMismatch.
- ServiceTypeMismatch.
- RealmMismatch (the separate return type of isolate).
- FiberDisposed.

DescriptorMismatch covers a descriptor/config type mismatch during mount, a Config TypeId mismatch
in update_config, and replace failing to reuse the same static descriptor or Config type.
ServiceTypeMismatch contains ServiceId, the Contract/Intercept channel, and expected/actual
TypeIds; call-level error enums retain this payload. RealmMismatch carries the supplied RealmId
from another Runtime.

## 3. Activation failure

When an activation step returns Err:

1. Perform L-Raise.
2. Enter Unloading.
3. Undo previously successful steps.
4. Remove BindingRecords registered by this episode.
5. Enter `Inactive { failure: Some(error) }`.
6. Expose Failed publicly.

The failure is recorded on the Fiber, without propagating to its parent or siblings.

## 4. Failed retry

Service churn does not automatically retry a Failed Fiber.

The following operations clear failure:

- restart;
- config update;
- implementation replace.

Dispose directly retires the Fiber.

`Inactive { failure: Some(_) }` cannot directly perform L-Begin. Failed is a quiescent/settled terminal state; only the explicit operations above clear
its failure.

## 5. Effect forward failure

When an independent Scope effect's driver returns Err:

1. Its EffectRecord stores the forward error and enters Disposing.
2. Inverses already produced by this EffectId execute in descending seq order.
3. EffectHandle completion returns the forward error and EffectDisposeReport.
4. The Fiber retains its current lifecycle state.

Only when activation code explicitly propagates this error does the activation step return
ActivationFailure and perform L-Raise.

## 6. Inverse failure

When an inverse returns CleanupError or panics with unwinding:

- Wrap it in CleanupFailure, recording owner, seq, label, and error.
- Mark that inverse as attempted.
- Continue with earlier inverses.
- Aggregate all CleanupFailures in the report.

Runtime does not retry inverses.

Runtime cannot determine whether repeating an inverse would be a safe retry, a double release,
or a second business compensation.

EffectDisposeReport collects only its EffectId. Fiber DisposeReport collects the target subtree
from O-Retire through O-Remove. ShutdownReport collects the entire Runtime from ShuttingDown
through Stopped. Historical errors already published during normal reload are not aggregated
again in later reports.

## 7. Panic

panic=unwind:

- Capture at the activation task boundary leads to L-Raise.
- Capture at an independent effect forward task boundary produces EffectError.
- Capture at the inverse task boundary produces CleanupError, then CleanupFailure.
- The Actor task itself does not execute user callbacks.

## 8. Futures that never complete

If an activation step, effect forward step, or inverse never completes:

- An activation step keeps the Fiber Loading.
- A step or inverse in Fiber-wide cleanup keeps the Fiber Unloading.
- An independent effect forward or manual effect cleanup preserves the current lifecycle state and keeps the Runtime non-quiescent.
- Runtime snapshots show the in-progress step.
- Completion is not fabricated.
- Dropping the Future is not used as rollback.

Consequently, graceful `Runtime::shutdown` also remains pending indefinitely when these Futures
never complete; Core does not fabricate a completion result.

## 9. Runtime shutdown

Runtime has three Host states:

~~~rust
pub enum RuntimeState {
    Running,
    ShuttingDown,
    Stopped,
}
~~~

shutdown:

1. Transition from Running to ShuttingDown.
2. Reject new Root/Scope effects, mount, provide, and Fiber restart/update/replace.
3. Move the synthetic Root into Unloading and withdraw Root Bindings.
4. Perform normal dependency withdrawal.
5. Undo Root accumulator effects in LIFO order. A mount inverse retires its child when its seq is selected, without waiting for the child's lifecycle.
6. Wait for the child registry to empty and complete the Root Episode.
7. Enter Stopped.
8. Return ShutdownReport.

ShutdownReport:

~~~rust
pub struct ShutdownReport {
    pub cleanup_errors: Vec<CleanupFailure>,
}
~~~

Cleanup errors do not prevent Runtime from reaching Stopped; they make its report nonempty.

Calls requiring RuntimeState == Running uniformly return RuntimeShuttingDown in ShuttingDown or
Stopped. Read-only state/snapshot, wait/dispose, and repeated shutdown still work.
default_realm/create_realm/isolate/intercept only derive opaque identities/routes without
modifying RuntimeCore, and remain available as well.

The dedicated `RegisterCleanup` command used by `on_cleanup` permits an existing step to finish
registering resource cleanup. Registration must belong to the same Episode, that Episode must
not have started unload cleanup, and an activation or effect step must still be in flight.
Under these conditions, registration can complete even while Runtime is ShuttingDown or the
owner is retired; in Unloading it immediately joins pending cleanup. This neither permits new
effects, provisions, or mounts nor revalidates an expired Scope. See
[Effect API constraints](effects.md#12-effect-api-constraints).

## 10. Repeated shutdown

Shutdown is an idempotent Host operation:

- In Running, it starts shutdown.
- In ShuttingDown, it waits for that same shutdown process.
- In Stopped, it returns the saved final report.

## 11. Root failure

Root does not run ordinary Component activation. Failed Root effect inverses are recorded as
cleanup failures, and shutdown continues.

## 12. Semantic boundaries

Core guarantees:

- Recorded inverses are called at most once.
- Cleanup ordering.
- Dependency withdrawal ordering.
- Stable committed dependencies.
- Serialized lifecycle transitions.

The correctness of an inverse against external state belongs to the component's EffectContract.
Realm semantics concern service resolution only. The Host must explicitly await
Runtime::shutdown; Rust Drop cannot replace asynchronous cleanup.

## 13. Error records

Diagnostic errors contain at least the applicable fields:

- FiberId.
- EpisodeId.
- EffectId.
- InverseOwner/seq.
- BindingId.
- ComponentId.
- ServiceId.
- Phase.
- FailureKind and message.

Error records aid diagnosis; they do not change Target or lifecycle identity.
