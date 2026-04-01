use std::marker::PhantomData;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation<T, P = ()> {
    Available(Available<T, P>),
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available<T, P = ()> {
    pub payload: ObservationPayload<T, P>,
    pub freshness: Freshness,
    pub meta: ObservationMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationPayload<T, P = ()> {
    Complete(T),
    Partial { partial: P, missing: MissingSet },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Complete<T> {
    pub value: T,
    pub meta: ObservationMeta,
}

pub trait PartialPayload<T>: Sized {
    fn from_present_slots(slots: &[Option<T>]) -> Self;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSet {
    pub missing_indices: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationSource {
    Stream,
    Query,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    Stale { stale_for: Duration },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationMeta {
    pub hardware_timestamp_us: Option<u64>,
    pub host_rx_mono_us: Option<u64>,
    pub source: ObservationSource,
}

impl ObservationMeta {
    fn stream(host_rx_mono_us: Option<u64>, hardware_timestamp_us: Option<u64>) -> Self {
        Self {
            hardware_timestamp_us,
            host_rx_mono_us,
            source: ObservationSource::Stream,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotPresence<T, const N: usize> {
    pub slots: [Option<T>; N],
}

impl<T: Copy, const N: usize> PartialPayload<T> for SlotPresence<T, N> {
    fn from_present_slots(slots: &[Option<T>]) -> Self {
        assert_eq!(slots.len(), N, "slot count must match partial payload size");
        Self {
            slots: std::array::from_fn(|index| slots[index]),
        }
    }
}

pub struct SingleFrameStore<T: Copy> {
    record: Option<StoredObservation<T>>,
}

struct StoredObservation<T: Copy> {
    value: T,
    meta: ObservationMeta,
}

impl<T: Copy> SingleFrameStore<T> {
    pub fn new() -> Self {
        Self { record: None }
    }

    pub fn record(&mut self, value: T, host_rx_mono_us: u64, hardware_timestamp_us: Option<u64>) {
        self.record = Some(StoredObservation {
            value,
            meta: ObservationMeta::stream(Some(host_rx_mono_us), hardware_timestamp_us),
        });
    }

    pub fn observe(&self, now_host_mono_us: u64, freshness_window_us: u64) -> Observation<T> {
        let Some(record) = self.record.as_ref() else {
            return Observation::Unavailable;
        };

        Observation::Available(Available {
            payload: ObservationPayload::Complete(record.value),
            freshness: freshness_from_timestamp(
                now_host_mono_us,
                record.meta.host_rx_mono_us,
                freshness_window_us,
            ),
            meta: record.meta,
        })
    }
}

impl<T: Copy> Default for SingleFrameStore<T> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FrameGroupStore<
    TSlot: Copy,
    const N: usize,
    TAssembled,
    TPartial = SlotPresence<TSlot, N>,
> where
    TPartial: PartialPayload<TSlot>,
{
    slots: [Option<StoredSlot<TSlot>>; N],
    _partial: PhantomData<TPartial>,
    _assembled: PhantomData<TAssembled>,
}

struct StoredSlot<T: Copy> {
    value: T,
    meta: ObservationMeta,
}

impl<TSlot: Copy, const N: usize, TAssembled, TPartial>
    FrameGroupStore<TSlot, N, TAssembled, TPartial>
where
    TPartial: PartialPayload<TSlot>,
{
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            _partial: PhantomData,
            _assembled: PhantomData,
        }
    }

    pub fn record_slot(
        &mut self,
        slot: usize,
        value: TSlot,
        host_rx_mono_us: u64,
        hardware_timestamp_us: Option<u64>,
    ) {
        assert!(slot < N, "slot index out of range");
        self.slots[slot] = Some(StoredSlot {
            value,
            meta: ObservationMeta::stream(Some(host_rx_mono_us), hardware_timestamp_us),
        });
    }

    pub fn observe<F>(
        &self,
        now_host_mono_us: u64,
        freshness_window_us: u64,
        assemble: F,
    ) -> Observation<TAssembled, TPartial>
    where
        F: FnOnce(&[Option<TSlot>; N]) -> Option<TAssembled>,
    {
        let present_slots = self.present_slots();
        let Some(meta) = self.latest_meta() else {
            return Observation::Unavailable;
        };

        let freshness = freshness_from_timestamp(
            now_host_mono_us,
            self.oldest_present_host_rx_mono_us(),
            freshness_window_us,
        );

        if let Some(value) = assemble(&present_slots) {
            return Observation::Available(Available {
                payload: ObservationPayload::Complete(value),
                freshness,
                meta,
            });
        }

        let partial = TPartial::from_present_slots(&present_slots);
        let missing = MissingSet {
            missing_indices: self.missing_indices(),
        };

        Observation::Available(Available {
            payload: ObservationPayload::Partial { partial, missing },
            freshness,
            meta,
        })
    }

    fn present_slots(&self) -> [Option<TSlot>; N] {
        std::array::from_fn(|index| self.slots[index].as_ref().map(|slot| slot.value))
    }

    fn missing_indices(&self) -> Vec<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.is_none().then_some(index))
            .collect()
    }

    fn latest_meta(&self) -> Option<ObservationMeta> {
        self.slots
            .iter()
            .flatten()
            .max_by_key(|slot| slot.meta.host_rx_mono_us.unwrap_or(0))
            .map(|slot| slot.meta)
    }

    fn oldest_present_host_rx_mono_us(&self) -> Option<u64> {
        self.slots.iter().flatten().filter_map(|slot| slot.meta.host_rx_mono_us).min()
    }
}

impl<TSlot: Copy, const N: usize, TAssembled, TPartial> Default
    for FrameGroupStore<TSlot, N, TAssembled, TPartial>
where
    TPartial: PartialPayload<TSlot>,
{
    fn default() -> Self {
        Self::new()
    }
}

fn freshness_from_timestamp(
    now_host_mono_us: u64,
    timestamp_us: Option<u64>,
    freshness_window_us: u64,
) -> Freshness {
    let Some(timestamp_us) = timestamp_us else {
        return Freshness::Stale {
            stale_for: Duration::ZERO,
        };
    };

    let age_us = now_host_mono_us.saturating_sub(timestamp_us);
    if age_us <= freshness_window_us {
        Freshness::Fresh
    } else {
        Freshness::Stale {
            stale_for: Duration::from_micros(age_us - freshness_window_us),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_store_can_return_stale_partial_observation() {
        let mut store = FrameGroupStore::<u8, 3, [u8; 3]>::new();
        store.record_slot(0, 10, 1_000, Some(10));
        let observation = store.observe(1_100, 50, |slots| Some([slots[0]?, slots[1]?, slots[2]?]));

        match observation {
            Observation::Available(available) => {
                assert!(matches!(
                    available.payload,
                    ObservationPayload::Partial { .. }
                ));
                assert!(matches!(available.freshness, Freshness::Stale { .. }));
            },
            other => panic!("expected available partial stale, got {other:?}"),
        }
    }

    #[test]
    fn single_frame_store_never_returns_partial() {
        let mut store = SingleFrameStore::new();
        store.record(42u8, 100, Some(99));
        let observation = store.observe(100, 50);
        assert!(matches!(
            observation,
            Observation::Available(Available {
                payload: ObservationPayload::Complete(42),
                freshness: Freshness::Fresh,
                ..
            })
        ));
    }
}
