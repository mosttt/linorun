#![forbid(unsafe_code)]

//! Start components when their services are available and clean up their resources when they stop.
//!
//! Start with [`Runtime`], the [`component`] macro, and [`ComponentContext`]. Service providers use
//! [`service_contract`] and [`service_impl`]. The lower-level lifecycle and effect APIs remain
//! available for components that need explicit step boundaries.

pub use linorun_macros::{component, component_descriptor, service_contract, service_impl};

mod component;
mod context;
#[path = "effect.rs"]
mod effect_impl;
mod error;
mod id;
mod lifecycle;
mod observability;
mod route;
mod runtime;
mod service;
mod state;
mod topology;

pub use component::{
    Activation, ActivationFuture, ActivationStream, Component, ComponentDescriptor, Requirement,
};
pub use context::{ComponentContext, Context, Scope};
pub use effect_impl::{
    DriverEvent, EffectDriver, IntoEffectDriver, InverseFn, InverseFuture, effect,
};
pub use error::{
    ActivationFailure, CleanupError, CleanupFailure, CleanupOwner, DisposeReport,
    EffectDisposeReport, EffectError, EffectStartError, FailureCause, FailureKind, FiberError,
    MountError, ProvideError, RealmMismatch, ServiceAccessError, ServiceTypeChannel,
    ServiceTypeMismatch, ShutdownReport, UpdateError,
};
pub use id::{BindingId, ComponentId, EffectId, EpisodeId, FiberId, RealmId, ServiceId, StepId};
pub use observability::{
    ActivationKeySnapshot, BindingEventRef, BindingSnapshot, EffectSnapshot, EpisodeSnapshot,
    FiberSnapshot, InverseSnapshot, RuntimeEvent, RuntimeEventKind, RuntimeSnapshot,
};
pub use runtime::{
    EffectHandle, FiberHandle, RootContext, Runtime, RuntimeBuilder, RuntimeTask, TaskSpawner,
};
pub use service::{CallContext, ErasedEndpoint, ServiceContract, ServiceRef};
pub use state::{
    EffectForwardOutcome, EffectSnapshotState, EpisodePhase, FiberState, InverseSnapshotState,
    RuntimeState,
};
