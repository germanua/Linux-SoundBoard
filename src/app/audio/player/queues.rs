use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;

pub(super) struct SampleQueue {
    pub(super) samples: VecDeque<f32>,
    capacity: usize,
}

impl SampleQueue {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.samples.len()
    }

    pub(super) fn push_slice(&mut self, input: &[f32]) {
        let overflow = self
            .samples
            .len()
            .saturating_add(input.len())
            .saturating_sub(self.capacity);
        if overflow > 0 {
            log::debug!(
                "SampleQueue overflow: dropping {} oldest samples (capacity={})",
                overflow,
                self.capacity
            );
            for _ in 0..overflow {
                let _ = self.samples.pop_front();
            }
        }
        self.samples.extend(input.iter().copied());
    }

    pub(super) fn pop_into(&mut self, output: &mut [f32]) -> usize {
        let mut dequeued = 0;
        for sample in output.iter_mut() {
            if let Some(value) = self.samples.pop_front() {
                *sample = value;
                dequeued += 1;
            } else {
                *sample = 0.0;
            }
        }
        dequeued
    }

    pub(super) fn trim_oldest_to(&mut self, max_len: usize) -> usize {
        let overflow = self.samples.len().saturating_sub(max_len);
        if overflow > 0 {
            self.samples.drain(..overflow);
        }
        overflow
    }
}

pub(super) struct ProcessQueues {
    pub(super) local: SampleQueue,
    pub(super) virtual_out: SampleQueue,
    pub(super) mic_in: SampleQueue,
}

impl ProcessQueues {
    pub(super) fn new(local_capacity: usize, virtual_capacity: usize, mic_capacity: usize) -> Self {
        Self {
            local: SampleQueue::new(local_capacity),
            virtual_out: SampleQueue::new(virtual_capacity),
            mic_in: SampleQueue::new(mic_capacity),
        }
    }
}

/// Shared-ownership handle to the inter-thread sample queues. Wraps
/// `Arc<Mutex<ProcessQueues>>` and deliberately exposes only `try_lock` so
/// production code cannot accidentally block the PipeWire main loop or the
/// RT callback. The `#[cfg(test)]` `lock()` method exists only for unit tests
/// that need to inspect queue state after filling them synchronously.
#[derive(Clone)]
pub(super) struct RtSharedQueues(Arc<Mutex<ProcessQueues>>);

impl RtSharedQueues {
    pub(super) fn new(queues: ProcessQueues) -> Self {
        Self(Arc::new(Mutex::new(queues)))
    }

    pub(super) fn try_lock(&self) -> Option<parking_lot::MutexGuard<'_, ProcessQueues>> {
        self.0.try_lock()
    }

    #[cfg(test)]
    pub(super) fn lock(&self) -> parking_lot::MutexGuard<'_, ProcessQueues> {
        self.0.lock()
    }
}
