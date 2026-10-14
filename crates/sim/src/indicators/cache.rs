use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use super::types::{IndicatorKey, IndicatorSeries};
use crate::error::SimError;

#[derive(Debug)]
pub(crate) struct IndicatorCache {
    budget_bytes: usize,
    inner: Mutex<CacheInner>,
}

type Slot = Arc<OnceLock<Arc<IndicatorSeries>>>;

enum SlotClaim {
    Hit(Arc<IndicatorSeries>),
    Miss(Slot),
}

#[derive(Debug)]
struct CacheEntry {
    slot: Slot,
    bytes: usize,
    last_used: u64,
}

#[derive(Debug, Default)]
struct CacheInner {
    map: HashMap<IndicatorKey, CacheEntry>,
    used_bytes: usize,
    clock: u64,
}

const POISONED_MESSAGE: &str = "failed to lock indicator cache: another thread panicked while \
     updating the cache bookkeeping (not while computing an indicator); the process state is \
     inconsistent, restart the run";

impl IndicatorCache {
    pub(crate) fn new(budget_mb: usize) -> Self {
        IndicatorCache {
            budget_bytes: budget_mb.saturating_mul(1024 * 1024),
            inner: Mutex::new(CacheInner::default()),
        }
    }

    pub(crate) fn get_or_compute(
        &self,
        key: IndicatorKey,
        compute_fn: impl FnOnce() -> IndicatorSeries,
    ) -> Result<Arc<IndicatorSeries>, SimError> {
        let slot =
            match self.claim_slot_or_cached_series_without_holding_the_mutex_during_compute(key)? {
                SlotClaim::Hit(series) => return Ok(series),
                SlotClaim::Miss(slot) => slot,
            };

        let series = slot.get_or_init(|| Arc::new(compute_fn())).clone();

        self.register_bytes_then_drop_entry_that_alone_exceeds_the_budget(key, &slot, &series)?;
        Ok(series)
    }

    fn lock_inner(&self) -> Result<MutexGuard<'_, CacheInner>, SimError> {
        self.inner
            .lock()
            .map_err(|_| SimError::Other(anyhow::anyhow!("{POISONED_MESSAGE}")))
    }

    fn claim_slot_or_cached_series_without_holding_the_mutex_during_compute(
        &self,
        key: IndicatorKey,
    ) -> Result<SlotClaim, SimError> {
        let mut guard = self.lock_inner()?;
        guard.clock += 1;
        let now = guard.clock;
        let entry = guard.map.entry(key).or_insert_with(|| CacheEntry {
            slot: Arc::new(OnceLock::new()),
            bytes: 0,
            last_used: now,
        });
        entry.last_used = now;
        if let Some(series) = entry.slot.get() {
            return Ok(SlotClaim::Hit(series.clone()));
        }
        Ok(SlotClaim::Miss(entry.slot.clone()))
    }

    fn register_bytes_then_drop_entry_that_alone_exceeds_the_budget(
        &self,
        key: IndicatorKey,
        slot: &Slot,
        series: &Arc<IndicatorSeries>,
    ) -> Result<(), SimError> {
        let mut guard = self.lock_inner()?;
        let registered_bytes = match guard.map.get_mut(&key) {
            Some(entry) if Arc::ptr_eq(&entry.slot, slot) && entry.bytes == 0 => {
                entry.bytes =
                    estimate_bytes_by_element_count_since_eviction_needs_only_rough_sizing(series);
                entry.bytes
            }
            _ => 0,
        };
        guard.used_bytes += registered_bytes;
        if registered_bytes == 0 {
            return Ok(());
        }
        self.evict_over_budget(&mut guard, key);
        let alone_exceeds_budget =
            guard.used_bytes > self.budget_bytes && registered_bytes > self.budget_bytes;
        if !alone_exceeds_budget {
            return Ok(());
        }
        let is_ours = guard
            .map
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.slot, slot));
        if is_ours && let Some(removed) = guard.map.remove(&key) {
            guard.used_bytes -= removed.bytes;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self.inner.lock().expect("test cache mutex");
            panic!("intentional poison for test");
        }));
    }

    fn evict_over_budget(&self, inner: &mut CacheInner, protected: IndicatorKey) {
        while inner.used_bytes > self.budget_bytes {
            let victim = inner
                .map
                .iter()
                .filter(|(k, e)| **k != protected && e.bytes > 0 && e.slot.get().is_some())
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| *k);
            let Some(victim) = victim else {
                break;
            };
            if let Some(removed) = inner.map.remove(&victim) {
                inner.used_bytes -= removed.bytes;
            }
        }
    }
}

fn estimate_bytes_by_element_count_since_eviction_needs_only_rough_sizing(
    series: &IndicatorSeries,
) -> usize {
    const F64_BYTES: usize = std::mem::size_of::<f64>();
    match series {
        IndicatorSeries::Single(v) => v.len() * F64_BYTES,
        IndicatorSeries::Band {
            lower,
            middle,
            upper,
        } => (lower.len() + middle.len() + upper.len()) * F64_BYTES,
        IndicatorSeries::Channel { lower, upper } => (lower.len() + upper.len()) * F64_BYTES,
    }
}

#[cfg(test)]
mod tests;
