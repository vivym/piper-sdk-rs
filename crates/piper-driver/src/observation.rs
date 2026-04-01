use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation<T> {
    Available(Available<T>),
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available<T> {
    pub payload: ObservationPayload<T>,
    pub freshness: Freshness,
    pub meta: ObservationMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationPayload<T> {
    Complete(T),
    Partial { missing: MissingSet },
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
    fn new(
        host_rx_mono_us: Option<u64>,
        hardware_timestamp_us: Option<u64>,
        source: ObservationSource,
    ) -> Self {
        Self {
            hardware_timestamp_us,
            host_rx_mono_us,
            source,
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
        self.record_with_source(
            value,
            host_rx_mono_us,
            hardware_timestamp_us,
            ObservationSource::Stream,
        );
    }

    pub(crate) fn record_with_source(
        &mut self,
        value: T,
        host_rx_mono_us: u64,
        hardware_timestamp_us: Option<u64>,
        source: ObservationSource,
    ) {
        self.record = Some(StoredObservation {
            value,
            meta: ObservationMeta::new(Some(host_rx_mono_us), hardware_timestamp_us, source),
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

pub struct FrameGroupStore<TSlot: Copy, const N: usize, TAssembled> {
    slots: [Option<StoredSlot<TSlot>>; N],
    _assembled: std::marker::PhantomData<TAssembled>,
}

struct StoredSlot<T: Copy> {
    value: T,
    meta: ObservationMeta,
}

impl<TSlot: Copy, const N: usize, TAssembled> FrameGroupStore<TSlot, N, TAssembled> {
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            _assembled: std::marker::PhantomData,
        }
    }

    pub fn record_slot(
        &mut self,
        slot: usize,
        value: TSlot,
        host_rx_mono_us: u64,
        hardware_timestamp_us: Option<u64>,
    ) {
        self.record_slot_with_source(
            slot,
            value,
            host_rx_mono_us,
            hardware_timestamp_us,
            ObservationSource::Stream,
        );
    }

    pub(crate) fn record_slot_with_source(
        &mut self,
        slot: usize,
        value: TSlot,
        host_rx_mono_us: u64,
        hardware_timestamp_us: Option<u64>,
        source: ObservationSource,
    ) {
        assert!(slot < N, "slot index out of range");
        self.slots[slot] = Some(StoredSlot {
            value,
            meta: ObservationMeta::new(Some(host_rx_mono_us), hardware_timestamp_us, source),
        });
    }

    pub fn observe<F>(
        &self,
        now_host_mono_us: u64,
        freshness_window_us: u64,
        assemble: F,
    ) -> Observation<TAssembled>
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

        let missing_indices = self.missing_indices();
        if missing_indices.is_empty() {
            return Observation::Unavailable;
        }

        Observation::Available(Available {
            payload: ObservationPayload::Partial {
                missing: MissingSet { missing_indices },
            },
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

impl<TSlot: Copy, const N: usize, TAssembled> Default for FrameGroupStore<TSlot, N, TAssembled> {
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
    fn observation_unavailable_is_reported() {
        let single = SingleFrameStore::<u8>::new();
        assert!(matches!(single.observe(100, 50), Observation::Unavailable));

        let store = FrameGroupStore::<u8, 3, [u8; 3]>::new();
        assert!(matches!(
            store.observe(100, 50, |_| Some([0, 0, 0])),
            Observation::Unavailable
        ));
    }

    #[test]
    fn group_store_can_return_fresh_partial_observation() {
        let mut store = FrameGroupStore::<u8, 3, [u8; 3]>::new();
        store.record_slot(0, 10, 1_000, Some(10));

        let observation = store.observe(1_020, 50, |_| None);

        match observation {
            Observation::Available(available) => {
                assert!(matches!(
                    available.payload,
                    ObservationPayload::Partial { .. }
                ));
                assert!(matches!(available.freshness, Freshness::Fresh));
            },
            other => panic!("expected available partial fresh, got {other:?}"),
        }
    }

    #[test]
    fn group_store_can_return_complete_fresh_observation() {
        let mut store = FrameGroupStore::<u8, 3, [u8; 3]>::new();
        store.record_slot(0, 10, 1_000, Some(11));
        store.record_slot(1, 11, 1_001, Some(12));
        store.record_slot(2, 12, 1_002, Some(13));

        let observation = store.observe(1_020, 50, |slots| {
            Some([slots[0].unwrap(), slots[1].unwrap(), slots[2].unwrap()])
        });

        assert!(matches!(
            observation,
            Observation::Available(Available {
                payload: ObservationPayload::Complete([10, 11, 12]),
                freshness: Freshness::Fresh,
                ..
            })
        ));
    }

    #[test]
    fn group_store_can_return_complete_stale_observation() {
        let mut store = FrameGroupStore::<u8, 2, [u8; 2]>::new();
        store.record_slot(0, 7, 900, Some(70));
        store.record_slot(1, 8, 901, Some(80));

        let observation = store.observe(1_100, 50, |slots| {
            Some([slots[0].unwrap(), slots[1].unwrap()])
        });

        assert!(matches!(
            observation,
            Observation::Available(Available {
                payload: ObservationPayload::Complete([7, 8]),
                freshness: Freshness::Stale { .. },
                ..
            })
        ));
    }

    #[test]
    fn group_store_reports_explicit_missing_indices() {
        let mut store = FrameGroupStore::<u8, 3, [u8; 3]>::new();
        store.record_slot(1, 20, 1_000, Some(20));

        let observation = store.observe(1_010, 50, |_| None);

        match observation {
            Observation::Available(available) => match available.payload {
                ObservationPayload::Partial { missing } => {
                    assert_eq!(missing.missing_indices, vec![0, 2]);
                },
                other => panic!("expected partial payload, got {other:?}"),
            },
            other => panic!("expected available observation, got {other:?}"),
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

    #[test]
    fn metadata_propagates_through_single_frame_store() {
        let mut store = SingleFrameStore::new();
        store.record_with_source(42u8, 123, Some(456), ObservationSource::Query);

        match store.observe(130, 50) {
            Observation::Available(available) => {
                assert_eq!(available.meta.host_rx_mono_us, Some(123));
                assert_eq!(available.meta.hardware_timestamp_us, Some(456));
                assert_eq!(available.meta.source, ObservationSource::Query);
            },
            other => panic!("expected available observation, got {other:?}"),
        }
    }

    #[test]
    fn metadata_propagates_through_group_store() {
        let mut store = FrameGroupStore::<u8, 2, [u8; 2]>::new();
        store.record_slot_with_source(0, 1, 1_000, Some(10), ObservationSource::Query);
        store.record_slot_with_source(1, 2, 1_010, Some(11), ObservationSource::Query);

        match store.observe(1_020, 50, |slots| {
            Some([slots[0].unwrap(), slots[1].unwrap()])
        }) {
            Observation::Available(available) => {
                assert_eq!(available.meta.host_rx_mono_us, Some(1_010));
                assert_eq!(available.meta.hardware_timestamp_us, Some(11));
                assert_eq!(available.meta.source, ObservationSource::Query);
            },
            other => panic!("expected available observation, got {other:?}"),
        }
    }
}
