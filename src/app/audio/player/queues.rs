use std::collections::VecDeque;

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
        for _ in 0..overflow {
            let _ = self.samples.pop_front();
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

    pub(super) fn pop_samples(&mut self, count: usize) -> Vec<f32> {
        let mut output = vec![0.0; count];
        let _ = self.pop_into(&mut output);
        output
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
