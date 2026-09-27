# Observability

[English](observability.md) | [简体中文](../../zh/operations/observability.md)

## 1. Purpose

Observability must answer:

- Is the runtime quiescent?
- What are each Fiber's current state and Target?
- Which providers are committed to each Fiber?
- Which consumers is a provider waiting for?
- Which activation/effect step or inverse is running?
- Has cleanup failed?
- How far has shutdown progressed?

## 2. RuntimeSnapshot

~~~rust
pub struct RuntimeSnapshot {
    pub sequence: u64,
    pub runtime_state: RuntimeState,
    pub quiescent: bool,
    pub fibers: Vec<FiberSnapshot>,
    pub bindings: Vec<BindingSnapshot>,
    pub effects: Vec<EffectSnapshot>,
}
~~~


A snapshot is taken within one Actor turn, so its fields are mutually consistent. DTO fields are
public; modifying a caller-owned DTO does not mutate RuntimeCore.

## 3. FiberSnapshot

~~~rust
pub struct FiberSnapshot {
    pub id: FiberId,
    pub parent: Option<FiberId>,
    pub mount_effect: Option<EffectId>,
    pub component: ComponentId,
    pub state: FiberState,
    pub retired: bool,
    pub activation_revision: u64,

    pub latest_key: Option<ActivationKeySnapshot>,
    pub episode: Option<EpisodeSnapshot>,
    pub relied_by: Vec<FiberId>,
    pub children: Vec<FiberId>,
    pub failure: Option<ActivationFailure>,
}
~~~


EpisodeSnapshot:

~~~rust
pub struct EpisodeSnapshot {
    pub id: EpisodeId,
    pub committed_key: ActivationKeySnapshot,
    pub committed_bindings: Vec<BindingId>,
    pub phase: EpisodePhase,
    pub running_step: Option<StepId>,
    pub accumulator: Vec<InverseSnapshot>,
    pub next_cleanup_seq: Option<u64>,
    pub cleanup_failures: Vec<CleanupFailure>,
}

pub struct ActivationKeySnapshot {
    pub providers: Vec<FiberId>,
    pub revision: u64,
}

pub enum EpisodePhase {
    Loading,
    Active,
    WaitingWithdrawal,
    Cleaning,
}
~~~


FiberSnapshot.failure exposes the ActivationFailure stored in Inactive when Failed. During
Unloading after L-Raise, it exposes the pending failure only while the outcome revision still
matches activation_revision. It is None in other states.

EpisodePhase maps Reloading to Loading and Active to Active. Unloading with a running inverse maps
to Cleaning; other Unloading states map to WaitingWithdrawal. running_step describes activation
only; independent effect steps appear in EffectSnapshot. next_cleanup_seq is the greatest requested
Armed sequence the scheduler could choose immediately, or None if the guard/boundary is closed or
the set is empty. WaitingWithdrawal covers both committed-consumer waits and episode forward-step
boundaries. Inspect relied_by and EffectSnapshot.running_step to identify these blockers.

## 4. BindingSnapshot

~~~rust
pub struct BindingSnapshot {
    pub id: BindingId,
    pub service: ServiceId,
    pub realm: RealmId,
    pub provider: FiberId,
    pub visible: bool,
    pub committed_consumers: Vec<FiberId>,
    pub owner_effect: EffectId,
}
~~~


committed_consumers includes only Fibers whose CommittedEntry.binding equals this BindingId.
FiberSnapshot.relied_by comes from consumers_by_provider.

## 5. EffectSnapshot

~~~rust
pub struct EffectSnapshot {
    pub id: EffectId,
    pub fiber: FiberId,
    pub episode: EpisodeId,
    pub label: Arc<str>,
    pub state: EffectSnapshotState,
    pub running_step: Option<StepId>,
    pub forward_error: Option<EffectError>,
    pub inverses: Vec<InverseSnapshot>,
}

pub enum EffectSnapshotState {
    Applying,
    Armed,
    Disposing,
}

pub struct InverseSnapshot {
    pub seq: u64,
    pub owner: CleanupOwner,
    pub label: Option<Arc<str>>,
    pub state: InverseSnapshotState,
    pub waiting_on: Vec<FiberId>,
}

pub enum InverseSnapshotState {
    Armed,
    Running,
}

~~~


EffectSnapshot.inverses projects entries owned by this EffectId from the Episode accumulator and
cleanup_running. Applying means the driver has not reached a terminal boundary. Armed means the
forward completed naturally and awaits cleanup. Disposing means disposal was requested and a step
or action remains. EffectCompletion publication and live-record removal occur in the same Actor
turn; completion is not a snapshot enum state.

running_step identifies the exact in-flight effect step. Between Actor turns, an Applying driver
is owned by a RuntimeTask, so running_step is Some; it is None when Armed. Disposing has Some while
waiting for the current step, and None while waiting for an inverse.

InverseSnapshot.waiting_on lists committed consumers still blocking a running Withdraw, sorted by
FiberId. At Binding removal, only `(FiberId, EpisodeId)` pairs whose committed views contain that
BindingId are captured; requirement watchers alone do not enter the wait set. User/RetireFiber
actions and non-running entries have empty lists. A captured Episode that has ended, or remains
Active with the latest ActivationKey, has reached the boundary. The wait does not follow later
Episodes or wait for descendant removal; it differs from public `wait_settled`. Before the
Fiber-wide guard allows selecting an inverse, FiberSnapshot.relied_by shows blocking committed consumers.

Effects sort by `(FiberId, has_no_inverse, min_seq_or_zero, EffectId)`. false sorts first. With no
inverse, the third field is 0 and the effect sorts after effects with inverses in the same Fiber.

## 6. Events

The Actor publishes events after committing state:

~~~rust
pub struct RuntimeEvent {
    pub sequence: u64,
    pub kind: RuntimeEventKind,
}
~~~


Exact event payloads:

~~~rust
pub enum RuntimeEventKind {
    RuntimeShuttingDown,
    RuntimeStopped,

    FiberInserted {
        fiber: FiberId,
        parent: Option<FiberId>,
        component: ComponentId,
    },
    FiberRetired { fiber: FiberId },
    FiberRemoved { fiber: FiberId },

    LBegin {
        fiber: FiberId,
        episode: EpisodeId,
        key: ActivationKeySnapshot,
    },
    LIter {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
    },
    LFinish {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
    },
    LDivert {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
        committed: ActivationKeySnapshot,
        latest: Option<ActivationKeySnapshot>,
    },
    LRaise {
        fiber: FiberId,
        episode: EpisodeId,
        completed_step: StepId,
        error: ActivationFailure,
    },
    LLeave {
        fiber: FiberId,
        episode: EpisodeId,
        committed: ActivationKeySnapshot,
        latest: Option<ActivationKeySnapshot>,
        removed_bindings: Vec<BindingId>,
    },
    LUnloadStarted { fiber: FiberId, episode: EpisodeId },
    LUnloadFinished { fiber: FiberId, episode: EpisodeId },

    ActivationYielded {
        fiber: FiberId,
        episode: EpisodeId,
        step: StepId,
        seq: u64,
        has_next: bool,
    },

    BindingInstalled { binding: BindingEventRef },
    BindingVisible { binding: BindingEventRef },
    BindingRemoved { binding: BindingEventRef },

    EffectStarted {
        fiber: FiberId,
        episode: EpisodeId,
        effect: EffectId,
        label: Arc<str>,
    },
    EffectYielded {
        effect: EffectId,
        step: StepId,
        seq: u64,
        has_next: bool,
    },
    EffectForwardCompleted {
        effect: EffectId,
        outcome: EffectForwardOutcome,
    },
    EffectForwardFailed { effect: EffectId, error: EffectError },
    EffectDisposeRequested { effect: EffectId },
    InverseStarted {
        fiber: FiberId,
        episode: EpisodeId,
        seq: u64,
        owner: CleanupOwner,
        label: Option<Arc<str>>,
    },
    InverseFinished {
        fiber: FiberId,
        episode: EpisodeId,
        seq: u64,
        owner: CleanupOwner,
        label: Option<Arc<str>>,
        error: Option<CleanupError>,
    },
}

pub struct BindingEventRef {
    pub id: BindingId,
    pub service: ServiceId,
    pub realm: RealmId,
    pub provider: FiberId,
    pub owner_effect: EffectId,
}
~~~


sequence records Actor commit order, not wall-clock time. RuntimeBuilder's initial commit emits
FiberInserted for synthetic Root with parent=None. Root is constructed directly as Active without
LBegin/LFinish events.

When one reducer commit produces several events, consecutive sequence values follow action order:
activation yield precedes LIter/LFinish; final EffectYielded precedes EffectForwardCompleted;
LFinish precedes BindingVisible; provider LLeave precedes its BindingRemoved. LDivert/LRaise also
precede their BindingRemoved events. BindingRemoved precedes watcher-triggered consumer
LLeave/LDivert. LUnloadStarted precedes the first Fiber-wide InverseStarted, and the last
InverseFinished precedes LUnloadFinished. Sender order matches sequence order.

The first shutdown commit emits RuntimeShuttingDown before Root lifecycle/binding events. After
all non-Root FiberRemoved events and Root LUnloadFinished, RuntimeStopped is the final shutdown
event. Root never emits FiberRemoved.

## 7. Event details

LRaise.error is an activation failure; InverseFinished with an error is a cleanup failure.
BindingRemoved marks removal from service resolution. ActivationYielded/EffectYielded.has_next
comes from the same completion: true means the driver returned to the Actor can continue, false
means a final Yield. Done does not emit Yielded.

Core-owned provide/mount operations have no user driver or StepId and do not emit EffectYielded.
Their successful commit order is:

~~~text
provide: EffectStarted < BindingInstalled < [BindingVisible] < EffectForwardCompleted
mount:   EffectStarted < FiberInserted < EffectForwardCompleted < [child LBegin]
~~~


Bracketed events occur only when their conditions hold. Core effect inverse sequences can be
observed through Effect/Inverse snapshots and subsequent InverseStarted events.

Each EffectId emits exactly one EffectDisposeRequested on first entering Disposing, whether from
manual disposal, Fiber-wide unload, or forward-failure cleanup. Forward failure orders
EffectForwardFailed before EffectDisposeRequested. L-Divert/L-Raise/L-Leave first emit their
lifecycle event, then the Fiber's BindingRemoved events, then EffectDisposeRequested by EffectId,
and finally watcher-triggered consumer lifecycle events. Already Disposing or removed effects do
not emit another request. Manual provide cleanup that removes a binding emits BindingRemoved
before InverseStarted.

The first FiberHandle::dispose to win O-Retire emits FiberRetired. If its mount effect has not
entered Disposing, its sole EffectDisposeRequested precedes that event. This host orchestration
already establishes the RetireFiber action's result, so the exactly matching accumulator entry
is removed directly, without InverseStarted/InverseFinished. If the parent already emitted the
request when entering Unloading, the host does not repeat it. Repeated disposal of a retired child
does not repeat retirement. When the parent accumulator actually selects RetireFiber, the order
is InverseStarted, FiberRetired, InverseFinished.

## 8. Trace partial order

A provider's Fiber-wide withdrawal trace must establish:

~~~text
BindingRemoved
    < Consumer LLeave or LDivert
    < Consumer LUnloadFinished
    < Provider InverseStarted
~~~


and:

~~~text
EffectYielded(seq N)
    < InverseStarted(seq N)
    < InverseFinished(seq N)
    < InverseStarted(next selected lower seq)
~~~


## 9. Metrics

Hosts derive these metrics from RuntimeEvent and RuntimeSnapshot. Core 0.1 has no built-in metrics registry.

Gauges:

- runtime_fibers
- fibers_by_state
- registered_bindings
- visible_bindings
- running_activation_steps
- running_effect_steps
- running_inverse_tasks
- committed_dependency_edges

Counters:

- activations_started
- activations_completed
- activations_diverted
- activation_failures
- effect_forward_failures
- inverse_calls
- cleanup_failures
- revision_driven_reactivations

Histograms:

- activation_step_duration
- episode_loading_duration
- consumer_drain_duration
- inverse_duration
- shutdown_duration

revision_driven_reactivations counts LLeave/LDivert caused by different committed/latest revisions.
It does not distinguish restart, config update, and implementation replacement. Events contain no
wall clock. Receivers measure histograms with the host's monotonic clock when receiving boundary
events, so these are host-observed durations.

## 10. Quiescence

Snapshot.quiescent follows the transition protocol:

- Actor state has reached a fixed point.
- No activation/effect step is running.
- No inverse is running.
- No Fiber is Reloading or Unloading.
- Each Active key matches its latest key.
- Pending has no key.
- Failed, represented by Inactive with failure=Some, is settled.
- No retired Fiber remains in the registry awaiting removal.
- No O-Remove is immediately actionable.

A quiescent runtime may contain Failed Fibers.

## 11. Logging

Logging belongs on the event Sender's receiver side. The Actor only constructs and sends
structured RuntimeEvent values; it does not invoke user logging callbacks.

Error logs use leaf FailureKind/message and event location fields. Ordinary state events do not
need error-level logging.

## 12. Ordering

Snapshot output uses stable ordering:

- Fibers by FiberId.
- Bindings by ServiceId, RealmId, BindingId.
- Effects by FiberId, has_no_inverse, min_seq_or_zero, EffectId.
- InverseSnapshot values within an Episode/Effect by sequence.
- Consumers by FiberId.
- Errors by actual inverse execution order.

Stable ordering helps testing, diffing, and reproducing issues.
