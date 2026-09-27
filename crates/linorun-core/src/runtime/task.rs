use std::{
    any::Any,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use futures_util::{FutureExt, future::poll_fn};

use crate::{
    ActivationFailure, CleanupError, EffectError, EffectId, EpisodeId, FiberId, InverseFn, StepId,
};

use super::{
    ActivationCompletion, ActivationDriver, Command, EffectStepCompletion, ErasedEffectDriver,
    RuntimeHandle, RuntimeTask,
};

pub(super) fn activation_task(
    runtime: RuntimeHandle,
    fiber: FiberId,
    episode: EpisodeId,
    step: StepId,
    mut driver: ActivationDriver,
) -> RuntimeTask {
    Box::pin(async move {
        let result = AssertUnwindSafe(poll_fn(|context| driver.poll_step(context)))
            .catch_unwind()
            .await
            .unwrap_or_else(|panic| Err(ActivationFailure::panic(panic_message(panic))));
        runtime.post(Command::ActivationFinished {
            fiber,
            episode,
            step,
            completion: ActivationCompletion { result, driver },
        });
    })
}

pub(super) fn effect_task(
    runtime: RuntimeHandle,
    fiber: FiberId,
    episode: EpisodeId,
    effect: EffectId,
    step: StepId,
    mut driver: ErasedEffectDriver,
) -> RuntimeTask {
    Box::pin(async move {
        let result = AssertUnwindSafe(poll_fn(|context| driver.as_mut().poll_step(context)))
            .catch_unwind()
            .await
            .unwrap_or_else(|panic| Err(EffectError::panic(panic_message(panic))));
        runtime.post(Command::EffectStepFinished {
            fiber,
            episode,
            effect,
            step,
            completion: EffectStepCompletion { result, driver },
        });
    })
}

pub(super) fn inverse_task(
    runtime: RuntimeHandle,
    fiber: FiberId,
    episode: EpisodeId,
    seq: u64,
    inverse: InverseFn,
) -> RuntimeTask {
    Box::pin(async move {
        let future = catch_unwind(AssertUnwindSafe(inverse));
        let result = match future {
            Ok(future) => AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .unwrap_or_else(|panic| Err(CleanupError::panic(panic_message(panic)))),
            Err(panic) => Err(CleanupError::panic(panic_message(panic))),
        };
        runtime.post(Command::InverseFinished {
            fiber,
            episode,
            seq,
            result,
        });
    })
}

fn panic_message(panic: Box<dyn Any + Send>) -> Arc<str> {
    if let Some(message) = panic.downcast_ref::<&str>() {
        return Arc::from(*message);
    }
    if let Some(message) = panic.downcast_ref::<String>() {
        return Arc::from(message.as_str());
    }
    Arc::from("user code panicked with a non-string payload")
}
