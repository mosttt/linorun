use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use crate::{DisposeReport, EffectDisposeReport, EffectError, EffectForwardOutcome};

pub(super) struct FiberCompletion {
    disposed: AtomicBool,
    report: Mutex<Option<DisposeReport>>,
}

pub(super) struct EffectCompletion {
    forward: Mutex<Option<Result<EffectForwardOutcome, EffectError>>>,
    report: Mutex<Option<EffectDisposeReport>>,
    disposed: AtomicBool,
}

impl EffectCompletion {
    pub(super) fn new() -> Self {
        Self {
            forward: Mutex::new(None),
            report: Mutex::new(None),
            disposed: AtomicBool::new(false),
        }
    }

    pub(super) fn complete_forward(&self, result: Result<EffectForwardOutcome, EffectError>) {
        let mut forward = self.forward.lock().expect("effect forward mutex poisoned");
        if forward.is_none() {
            *forward = Some(result);
        }
    }

    pub(super) fn forward(&self) -> Option<Result<EffectForwardOutcome, EffectError>> {
        self.forward
            .lock()
            .expect("effect forward mutex poisoned")
            .clone()
    }

    pub(super) fn finish(&self, report: EffectDisposeReport) {
        *self
            .report
            .lock()
            .expect("effect completion mutex poisoned") = Some(report);
        self.disposed.store(true, Ordering::Release);
    }

    pub(super) fn report(&self) -> Option<EffectDisposeReport> {
        if !self.disposed.load(Ordering::Acquire) {
            return None;
        }
        self.report
            .lock()
            .expect("effect completion mutex poisoned")
            .clone()
    }
}

impl FiberCompletion {
    pub(super) fn new() -> Self {
        Self {
            disposed: AtomicBool::new(false),
            report: Mutex::new(None),
        }
    }

    pub(super) fn finish(&self, report: DisposeReport) {
        *self.report.lock().expect("fiber completion mutex poisoned") = Some(report);
        self.disposed.store(true, Ordering::Release);
    }

    pub(super) fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::Acquire)
    }

    pub(super) fn report(&self) -> Option<DisposeReport> {
        if !self.is_disposed() {
            return None;
        }
        self.report
            .lock()
            .expect("fiber completion mutex poisoned")
            .clone()
    }
}
