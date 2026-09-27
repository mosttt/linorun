use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

macro_rules! numeric_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
            pub(crate) const fn new(value: u64) -> Self {
                Self(value)
            }
        }
    };
}

numeric_id!(FiberId);
numeric_id!(EpisodeId);
numeric_id!(EffectId);
numeric_id!(BindingId);
numeric_id!(StepId);

macro_rules! stable_name {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Arc<str>);

        impl $name {
            pub(crate) fn new(value: impl Into<Arc<str>>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

stable_name!(ComponentId);
stable_name!(ServiceId);

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RuntimeTag(u64);

impl RuntimeTag {
    pub(crate) fn allocate() -> Arc<Self> {
        static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(1);
        Arc::new(Self(NEXT_RUNTIME.fetch_add(1, Ordering::Relaxed)))
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RealmId {
    runtime: Arc<RuntimeTag>,
    local: u64,
}

impl RealmId {
    pub(crate) fn new(runtime: Arc<RuntimeTag>, local: u64) -> Self {
        Self { runtime, local }
    }

    pub(crate) fn belongs_to(&self, runtime: &Arc<RuntimeTag>) -> bool {
        self.runtime == *runtime
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realm_identity_includes_runtime() {
        let first = RealmId::new(RuntimeTag::allocate(), 0);
        let second = RealmId::new(RuntimeTag::allocate(), 0);

        assert_ne!(first, second);
    }

    #[test]
    fn stable_names_expose_their_value() {
        let component = ComponentId::new("app/component@1");
        let service = ServiceId::new("app/service@1");

        assert_eq!(component.as_str(), "app/component@1");
        assert_eq!(service.as_str(), "app/service@1");
    }
}
