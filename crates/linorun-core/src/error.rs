use std::{any::TypeId, error::Error, fmt, sync::Arc};

use crate::{EffectId, EpisodeId, FiberId, RealmId, ServiceId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureKind {
    User,
    Panic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureCause {
    pub kind: FailureKind,
    pub message: Arc<str>,
}

impl FailureCause {
    fn user(message: impl Into<Arc<str>>) -> Self {
        Self {
            kind: FailureKind::User,
            message: message.into(),
        }
    }

    pub(crate) fn panic(message: impl Into<Arc<str>>) -> Self {
        Self {
            kind: FailureKind::Panic,
            message: message.into(),
        }
    }
}

impl fmt::Display for FailureCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

macro_rules! leaf_error {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct $name {
            pub cause: FailureCause,
        }

        impl $name {
            pub fn user(message: impl Into<Arc<str>>) -> Self {
                Self {
                    cause: FailureCause::user(message),
                }
            }

            pub(crate) fn panic(message: impl Into<Arc<str>>) -> Self {
                Self {
                    cause: FailureCause::panic(message),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.cause.fmt(formatter)
            }
        }

        impl Error for $name {}
    };
}

leaf_error!(ActivationFailure);
leaf_error!(EffectError);
leaf_error!(CleanupError);

impl From<EffectStartError> for ActivationFailure {
    fn from(error: EffectStartError) -> Self {
        Self::user(error.to_string())
    }
}

impl From<EffectError> for ActivationFailure {
    fn from(error: EffectError) -> Self {
        Self { cause: error.cause }
    }
}

macro_rules! activation_error_from {
    ($($error:ty),+ $(,)?) => {
        $(impl From<$error> for ActivationFailure {
            fn from(error: $error) -> Self {
                Self::user(error.to_string())
            }
        })+
    };
}

activation_error_from!(
    MountError,
    ProvideError,
    ServiceAccessError,
    RealmMismatch,
    std::io::Error
);

impl From<std::io::Error> for CleanupError {
    fn from(error: std::io::Error) -> Self {
        Self::user(error.to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupOwner {
    Activation,
    Effect(EffectId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupFailure {
    pub owner: CleanupOwner,
    pub fiber: FiberId,
    pub episode: EpisodeId,
    pub seq: u64,
    pub label: Option<Arc<str>>,
    pub cause: CleanupError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceTypeChannel {
    Contract,
    Intercept,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceTypeMismatch {
    pub service: ServiceId,
    pub channel: ServiceTypeChannel,
    pub expected: TypeId,
    pub actual: TypeId,
}

impl fmt::Display for ServiceTypeMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "service {} has a mismatched {:?} type",
            self.service, self.channel
        )
    }
}

impl Error for ServiceTypeMismatch {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RealmMismatch {
    pub realm: RealmId,
}

impl RealmMismatch {
    pub(crate) fn new(realm: RealmId) -> Self {
        Self { realm }
    }
}

impl fmt::Display for RealmMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("realm belongs to another runtime")
    }
}

impl Error for RealmMismatch {}

macro_rules! operation_error {
    (
        $name:ident {
            unit: [$($unit:ident),* $(,)?],
            wrapped: [$($wrapped:ident($inner:ty)),* $(,)?]
        }
    ) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub enum $name {
            $($unit,)*
            $($wrapped($inner),)*
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    $(Self::$unit => formatter.write_str(operation_error!(@message $unit)),)*
                    $(Self::$wrapped(inner) => inner.fmt(formatter),)*
                }
            }
        }

        impl Error for $name {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                match self {
                    $(Self::$unit => None,)*
                    $(Self::$wrapped(inner) => Some(inner),)*
                }
            }
        }
    };
    (@message RuntimeShuttingDown) => { "runtime is shutting down or stopped" };
    (@message InactiveContext) => { "context episode is no longer active" };
    (@message InactiveScope) => { "scope is no longer active" };
    (@message FiberDisposed) => { "fiber is retired or disposed" };
    (@message DescriptorMismatch) => { "component descriptor or configuration type does not match" };
    (@message UndeclaredProvide) => { "service is not declared by the component" };
    (@message DuplicateBinding) => { "binding slot is already occupied" };
    (@message UndeclaredDependency) => { "service is not a declared dependency" };
}

operation_error!(MountError {
    unit: [
        RuntimeShuttingDown,
        InactiveContext,
        InactiveScope,
        FiberDisposed,
        DescriptorMismatch,
    ],
    wrapped: [ServiceTypeMismatch(ServiceTypeMismatch)]
});

operation_error!(ProvideError {
    unit: [
        RuntimeShuttingDown,
        InactiveContext,
        InactiveScope,
        UndeclaredProvide,
        DuplicateBinding,
    ],
    wrapped: [ServiceTypeMismatch(ServiceTypeMismatch)]
});

operation_error!(EffectStartError {
    unit: [RuntimeShuttingDown, InactiveScope],
    wrapped: []
});

operation_error!(ServiceAccessError {
    unit: [UndeclaredDependency],
    wrapped: [ServiceTypeMismatch(ServiceTypeMismatch)]
});

operation_error!(FiberError {
    unit: [RuntimeShuttingDown, FiberDisposed],
    wrapped: []
});

operation_error!(UpdateError {
    unit: [RuntimeShuttingDown, FiberDisposed, DescriptorMismatch],
    wrapped: []
});

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectDisposeReport {
    pub forward_error: Option<EffectError>,
    pub cleanup_errors: Vec<CleanupFailure>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisposeReport {
    pub cleanup_errors: Vec<CleanupFailure>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShutdownReport {
    pub cleanup_errors: Vec<CleanupFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_error_preserves_kind_and_message() {
        let error = EffectError::user("forward failed");

        assert_eq!(error.cause.kind, FailureKind::User);
        assert_eq!(error.to_string(), "forward failed");
    }
}
