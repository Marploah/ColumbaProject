use std::collections::HashMap;
use std::time::{Duration, Instant};
use crate::quant::LiquidityWalls;

const WALL_SIGNIFICANCE_MULTIPLIER: f64 = 3.0;
const PRUNE_AFTER_SECS: u64 = 300;

/// Enriched info for a single significant depth wall, including persistence and spoof signals.
#[derive(Debug, Clone)]
pub struct WallInfo {
    pub price: f64,
    pub size: f64,
    /// Consecutive polls this wall has appeared.
    pub persistence_polls: u32,
    /// 0.0 = likely real institutional order, 1.0 = likely spoof.
    pub spoof_score: f32,
    /// Wall disappeared then reappeared at same price — probable iceberg / replenishment.
    pub replenishment_detected: bool,
}

/// Analytics derived by tracking depth walls across multiple polls.
/// Internal only — not serialized to the wire.
#[derive(Debug, Clone, Default)]
pub struct LiquidityAnalytics {
    pub bid_wall: Option<WallInfo>,
    pub ask_wall: Option<WallInfo>,
    /// True when any primary wall carries a spoof_score > 0.65.
    pub spoof_alert: bool,
    pub feed_healthy: bool,
}

// ── per-level tracking entry (internal) ─────────────────────────────────────

struct WallEntry {
    price: f64,
    last_size: f64,
    last_present_at: Instant,
    total_polls_present: u32,
    total_polls_absent: u32,
    currently_present: bool,
    replenished: bool,
}

impl WallEntry {
    fn spoof_score(&self) -> f32 {
        let present = self.total_polls_present;
        let absent = self.total_polls_absent;
        if present >= 6 {
            return 0.05; // many consecutive polls → almost certainly a real resting order
        }
        if absent > 0 && present <= 2 {
            return 0.80; // appeared briefly then vanished without being filled
        }
        if present > 0 && absent > present {
            return 0.55; // more absent than present over lifetime
        }
        0.25 // young wall, normal uncertainty
    }

    fn to_wall_info(&self) -> WallInfo {
        WallInfo {
            price: self.price,
            size: self.last_size,
            persistence_polls: self.total_polls_present,
            spoof_score: self.spoof_score(),
            replenishment_detected: self.replenished,
        }
    }
}

// ── WallTracker ──────────────────────────────────────────────────────────────

/// Stateful tracker for significant order-book walls across depth polls.
/// Call `update()` on each poll; read `snapshot_walls()` / `snapshot_analytics()`.
pub struct WallTracker {
    bid_entries: HashMap<u64, WallEntry>,
    ask_entries: HashMap<u64, WallEntry>,
    cached_walls: LiquidityWalls,
    cached_analytics: LiquidityAnalytics,
}

impl Default for WallTracker {
    fn default() -> Self {
        Self {
            bid_entries: HashMap::new(),
            ask_entries: HashMap::new(),
            cached_walls: LiquidityWalls::default(),
            cached_analytics: LiquidityAnalytics::default(),
        }
    }
}

impl WallTracker {
    /// Quantize price to centavo-level integer key (avoids float HashMap keys).
    pub fn price_key(price: f64) -> u64 {
        (price * 100.0).round() as u64
    }

    /// Return all levels whose size is ≥ 3× the mean size.
    pub fn find_significant(levels: &[[String; 2]]) -> Vec<(f64, f64)> {
        let parsed: Vec<(f64, f64)> = levels
            .iter()
            .filter_map(|[p, q]| {
                let price = p.parse::<f64>().ok()?;
                let qty = q.parse::<f64>().ok()?;
                Some((price, qty))
            })
            .collect();
        if parsed.is_empty() {
            return vec![];
        }
        let mean_qty = parsed.iter().map(|(_, q)| q).sum::<f64>() / parsed.len() as f64;
        let threshold = mean_qty * WALL_SIGNIFICANCE_MULTIPLIER;
        parsed.into_iter().filter(|(_, q)| *q >= threshold).collect()
    }

    fn update_side(entries: &mut HashMap<u64, WallEntry>, significant: &[(f64, f64)], now: Instant) {
        // Mark all tracked entries as absent this poll.
        for e in entries.values_mut() {
            e.currently_present = false;
        }

        // Update from the new significant levels.
        for &(price, size) in significant {
            let key = Self::price_key(price);
            let entry = entries.entry(key).or_insert_with(|| WallEntry {
                price,
                last_size: size,
                last_present_at: now,
                total_polls_present: 0,
                total_polls_absent: 0,
                currently_present: false,
                replenished: false,
            });
            // Replenishment: was absent in a prior poll, now back at same price.
            if !entry.currently_present && entry.total_polls_absent > 0 {
                entry.replenished = true;
            }
            entry.currently_present = true;
            entry.last_size = size;
            entry.last_present_at = now;
            entry.total_polls_present += 1;
        }

        // Increment absent count for entries missing this poll.
        for entry in entries.values_mut() {
            if !entry.currently_present {
                entry.total_polls_absent += 1;
            }
        }

        // Prune levels not seen in > 5 minutes.
        let cutoff = now.checked_sub(Duration::from_secs(PRUNE_AFTER_SECS)).unwrap_or(now);
        entries.retain(|_, e| e.last_present_at > cutoff);
    }

    /// Ingest a fresh depth snapshot. `bids`/`asks` are the raw `[price, qty]` string arrays
    /// from the Binance depth REST response.
    pub fn update(&mut self, bids: &[[String; 2]], asks: &[[String; 2]], now: Instant) {
        let sig_bids = Self::find_significant(bids);
        let sig_asks = Self::find_significant(asks);

        Self::update_side(&mut self.bid_entries, &sig_bids, now);
        Self::update_side(&mut self.ask_entries, &sig_asks, now);

        // Pick the largest wall on each side from currently-present entries.
        let best_bid = self.bid_entries.values()
            .filter(|e| e.currently_present)
            .max_by(|a, b| a.last_size.partial_cmp(&b.last_size).unwrap_or(std::cmp::Ordering::Equal));
        let best_ask = self.ask_entries.values()
            .filter(|e| e.currently_present)
            .max_by(|a, b| a.last_size.partial_cmp(&b.last_size).unwrap_or(std::cmp::Ordering::Equal));

        self.cached_walls = LiquidityWalls {
            bid_wall_price: best_bid.map(|e| e.price),
            bid_wall_size: best_bid.map(|e| e.last_size),
            ask_wall_price: best_ask.map(|e| e.price),
            ask_wall_size: best_ask.map(|e| e.last_size),
        };

        let bid_info = best_bid.map(|e| e.to_wall_info());
        let ask_info = best_ask.map(|e| e.to_wall_info());
        let spoof_alert = bid_info.as_ref().map_or(false, |w| w.spoof_score > 0.65)
            || ask_info.as_ref().map_or(false, |w| w.spoof_score > 0.65);

        self.cached_analytics = LiquidityAnalytics {
            bid_wall: bid_info,
            ask_wall: ask_info,
            spoof_alert,
            feed_healthy: true,
        };
    }

    /// Called on fetch error — marks feed unhealthy and clears cached walls.
    pub fn mark_unhealthy(&mut self) {
        self.cached_analytics.feed_healthy = false;
        self.cached_walls = LiquidityWalls::default();
    }

    /// Called on symbol change — clears all history so stale levels don't persist.
    pub fn reset(&mut self) {
        self.bid_entries.clear();
        self.ask_entries.clear();
        self.cached_walls = LiquidityWalls::default();
        self.cached_analytics = LiquidityAnalytics::default();
    }

    pub fn snapshot_walls(&self) -> LiquidityWalls {
        self.cached_walls.clone()
    }

    pub fn snapshot_analytics(&self) -> LiquidityAnalytics {
        self.cached_analytics.clone()
    }
}

// ── unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn level(price: f64, qty: f64) -> [String; 2] {
        [price.to_string(), qty.to_string()]
    }

    fn make_levels(pairs: &[(f64, f64)]) -> Vec<[String; 2]> {
        pairs.iter().map(|&(p, q)| level(p, q)).collect()
    }

    #[test]
    fn significant_wall_filters_below_threshold() {
        // sizes: [2, 2, 2, 20] → mean = 6.5, threshold = 19.5 → only 20.0 qualifies
        let levels = make_levels(&[(100.0, 2.0), (99.0, 2.0), (98.0, 2.0), (97.0, 20.0)]);
        let sig = WallTracker::find_significant(&levels);
        assert_eq!(sig.len(), 1);
        assert!((sig[0].0 - 97.0).abs() < 1e-9);
    }

    #[test]
    fn persistence_increments_across_polls() {
        let mut tracker = WallTracker::default();
        let now = Instant::now();
        // sizes: [2, 2, 2, 20] → mean 6.5, threshold 19.5 → 20.0 qualifies
        let bids = make_levels(&[(100.0, 2.0), (99.0, 2.0), (98.0, 2.0), (97.0, 20.0)]);
        let empty: Vec<[String; 2]> = vec![];

        tracker.update(&bids, &empty, now);
        tracker.update(&bids, &empty, now + Duration::from_secs(10));

        let analytics = tracker.snapshot_analytics();
        let wall = analytics.bid_wall.expect("should have bid wall");
        assert_eq!(wall.persistence_polls, 2);
        assert!(wall.spoof_score < 0.3, "persistent wall should have low spoof score, got {}", wall.spoof_score);
    }

    #[test]
    fn spoof_detection_fires_after_brief_appearance() {
        let mut tracker = WallTracker::default();
        let now = Instant::now();
        // Wall present: sizes [2,2,2,20] → 20 qualifies (mean 6.5, threshold 19.5)
        let bids_with_wall = make_levels(&[(100.0, 2.0), (99.0, 2.0), (98.0, 2.0), (97.0, 20.0)]);
        // Wall absent: sizes all small → nothing qualifies
        let bids_no_wall = make_levels(&[(100.0, 2.0), (99.0, 2.0), (98.0, 2.0), (97.0, 2.5)]);
        let empty: Vec<[String; 2]> = vec![];

        // Wall appears one poll then disappears.
        tracker.update(&bids_with_wall, &empty, now);
        tracker.update(&bids_no_wall, &empty, now + Duration::from_secs(10));

        // The entry at 97.0 should exist (tracked even when absent) with high spoof score.
        let key = WallTracker::price_key(97.0);
        let entry = tracker.bid_entries.get(&key).expect("entry for 97.0 should exist");
        assert!(
            entry.spoof_score() > 0.65,
            "wall seen once then gone should have high spoof score, got {}",
            entry.spoof_score()
        );
    }

    #[test]
    fn replenishment_detected_on_reappearance() {
        let mut tracker = WallTracker::default();
        let now = Instant::now();
        let bids_wall = make_levels(&[(100.0, 2.0), (99.0, 2.0), (98.0, 2.0), (97.0, 20.0)]);
        let bids_no_wall = make_levels(&[(100.0, 2.0), (99.0, 2.0), (98.0, 2.0), (97.0, 2.5)]);
        let empty: Vec<[String; 2]> = vec![];

        tracker.update(&bids_wall, &empty, now);
        tracker.update(&bids_no_wall, &empty, now + Duration::from_secs(10));
        tracker.update(&bids_wall, &empty, now + Duration::from_secs(20));

        let analytics = tracker.snapshot_analytics();
        let wall = analytics.bid_wall.expect("should have bid wall after replenishment");
        assert!(wall.replenishment_detected, "should detect replenishment");
    }

    #[test]
    fn reset_clears_all_entries() {
        let mut tracker = WallTracker::default();
        let now = Instant::now();
        let bids = make_levels(&[(100.0, 2.0), (97.0, 20.0)]);
        let empty: Vec<[String; 2]> = vec![];

        tracker.update(&bids, &empty, now);
        tracker.reset();

        assert!(tracker.bid_entries.is_empty());
        assert!(tracker.snapshot_walls().bid_wall_price.is_none());
    }

    #[test]
    fn wire_walls_match_best_present_entry() {
        let mut tracker = WallTracker::default();
        let now = Instant::now();
        // sizes: [2, 2, 2, 50] → mean 14.0, threshold 42.0 → only 50 qualifies
        let bids = make_levels(&[(100.0, 2.0), (99.0, 2.0), (98.0, 2.0), (97.0, 50.0)]);
        let empty: Vec<[String; 2]> = vec![];

        tracker.update(&bids, &empty, now);

        let walls = tracker.snapshot_walls();
        assert!((walls.bid_wall_size.unwrap() - 50.0).abs() < 1e-9);
        assert!((walls.bid_wall_price.unwrap() - 97.0).abs() < 1e-9);
    }
}
