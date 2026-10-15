use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

mod calc;
mod types;

pub use calc::compute;
pub use types::{IndicatorKey, IndicatorKind, IndicatorSeries};

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

#[derive(Debug)]
pub(crate) struct IndicatorCache {
    budget_bytes: usize,
    inner: Mutex<CacheInner>,
}

type Slot = Arc<OnceLock<Arc<IndicatorSeries>>>;

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

const POISONED_MESSAGE: &str = "indicator cache mutex poisoned: another thread panicked while \
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
    ) -> Arc<IndicatorSeries> {
        let slot =
            match self.claim_slot_or_cached_series_without_holding_the_mutex_during_compute(key) {
                Ok(series) => return series,
                Err(slot) => slot,
            };

        let series = slot.get_or_init(|| Arc::new(compute_fn())).clone();

        self.register_bytes_then_drop_entry_that_alone_exceeds_the_budget(key, &slot, &series);
        series
    }

    fn claim_slot_or_cached_series_without_holding_the_mutex_during_compute(
        &self,
        key: IndicatorKey,
    ) -> Result<Arc<IndicatorSeries>, Slot> {
        let mut guard = self.inner.lock().expect(POISONED_MESSAGE);
        guard.clock += 1;
        let now = guard.clock;
        let entry = guard.map.entry(key).or_insert_with(|| CacheEntry {
            slot: Arc::new(OnceLock::new()),
            bytes: 0,
            last_used: now,
        });
        entry.last_used = now;
        if let Some(series) = entry.slot.get() {
            return Ok(series.clone());
        }
        Err(entry.slot.clone())
    }

    fn register_bytes_then_drop_entry_that_alone_exceeds_the_budget(
        &self,
        key: IndicatorKey,
        slot: &Slot,
        series: &Arc<IndicatorSeries>,
    ) {
        let mut guard = self.inner.lock().expect(POISONED_MESSAGE);
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
            return;
        }
        self.evict_over_budget(&mut guard, key);
        let alone_exceeds_budget =
            guard.used_bytes > self.budget_bytes && registered_bytes > self.budget_bytes;
        if !alone_exceeds_budget {
            return;
        }
        let is_ours = guard
            .map
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.slot, slot));
        let removed = if is_ours {
            guard.map.remove(&key)
        } else {
            None
        };
        if let Some(removed) = removed {
            guard.used_bytes -= removed.bytes;
        }
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

#[cfg(test)]
mod tests;
