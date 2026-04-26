// ============================================================
// domain.rs — Core domain types and shared state container
// ============================================================
//
// SharedState is the single source of truth written by the
// analytics engine and read by scoring + API layers.
// All fields use DashMap (concurrent hashmap) so writes per
// coin never contend with reads for other coins.

use std::collections::VecDeque;
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

use crate::config::{CoinConfig, EngineConfig};

// ── Tier classification ───────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Tier {
    A, // Score > 1.3  → TRADEABLE
    B, // Score 0.8–1.3 → WATCH
    C, // Score < 0.8  → IGNORE
}

impl Tier {
    pub fn from_score(s: f64) -> Self {
        if s > 1.3 { Tier::A }
        else if s > 0.8 { Tier::B }
        else { Tier::C }
    }
}

// ── Category ─────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    CoreBtcFollower,
    Hybrid,
    Volatile,
    Noise,
}

// ── Raw 1-minute candle (normalized from any source) ─────
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    pub symbol:    String,
    pub open_time: DateTime<Utc>,
    pub open:      f64,
    pub high:      f64,
    pub low:       f64,
    pub close:     f64,
    pub volume:    f64,
    pub is_closed: bool,   // false = partial/live
    pub source:    DataSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DataSource { Binance, CoinGecko }

// ── Per-coin rolling buffer ───────────────────────────────
// Holds last N close prices and derived log-returns.
// VecDeque gives O(1) push_back / pop_front for the ring.
#[derive(Debug)]
pub struct PriceBuffer {
    pub window:   usize,
    pub closes:   VecDeque<f64>,
    pub returns:  VecDeque<f64>,   // r_t = (P_t - P_{t-1}) / P_{t-1}
    pub volumes:  VecDeque<f64>,
    pub updated:  Option<DateTime<Utc>>,
}

impl PriceBuffer {
    pub fn new(window: usize) -> Self {
        Self {
            window,
            closes:  VecDeque::with_capacity(window + 1),
            returns: VecDeque::with_capacity(window),
            volumes: VecDeque::with_capacity(window),
            updated: None,
        }
    }

    /// Push a new close price, derive return, evict old entries.
    pub fn push(&mut self, close: f64, volume: f64, ts: DateTime<Utc>) {
        if let Some(&prev) = self.closes.back() {
            let ret = if prev > 0.0 { (close - prev) / prev } else { 0.0 };
            self.returns.push_back(ret);
            if self.returns.len() > self.window { self.returns.pop_front(); }
        }
        self.closes.push_back(close);
        if self.closes.len() > self.window + 1 { self.closes.pop_front(); }
        self.volumes.push_back(volume);
        if self.volumes.len() > self.window { self.volumes.pop_front(); }
        self.updated = Some(ts);
    }

    pub fn len(&self) -> usize { self.returns.len() }
    pub fn returns_slice(&self) -> Vec<f64> { self.returns.iter().copied().collect() }
    pub fn avg_abs_return(&self) -> f64 {
        if self.returns.is_empty() { return 0.0; }
        self.returns.iter().map(|r| r.abs()).sum::<f64>() / self.returns.len() as f64
    }
}

// ── Computed coin metrics ─────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CoinMetrics {
    pub symbol:           String,
    /// Pearson ρ over rolling window
    pub correlation:      f64,
    /// avg(|r_coin|) / avg(|r_BTC|)
    pub reaction_strength: f64,
    /// k* = argmax_k ρ(r_BTC(t), r_coin(t+k))
    pub optimal_lag:      i32,
    /// L = e^{-k*}
    pub lag_factor:       f64,
    /// Confidence [0,1] from liquidity + age heuristics
    pub confidence:       f64,
    /// Final score = ρ × R × L × Confidence
    pub score:            f64,
    pub tier:             Option<Tier>,
    pub category:         Option<Category>,
    /// Rolling correlation history (last 60 ticks) for charts
    pub corr_history:     Vec<f64>,
    /// Lag cross-correlations ρ(k) for k = 0..=6
    pub lag_profile:      Vec<f64>,
    pub last_price:       f64,
    pub price_change_pct: f64,
    pub updated_at:       Option<DateTime<Utc>>,
}

// ── Ranked entry (scoring engine output) ─────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedCoin {
    pub rank:              usize,
    pub symbol:            String,
    pub correlation:       f64,
    pub reaction_strength: f64,
    pub lag:               i32,
    pub score:             f64,
    pub tier:              Tier,
    pub category:          Category,
    pub confidence:        f64,
}

// ── Heatmap data (for /heatmap-data) ─────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatmapData {
    pub symbols:  Vec<String>,
    /// Row-major correlation matrix (symbols × symbols)
    pub matrix:   Vec<Vec<f64>>,
    pub snapshot_at: DateTime<Utc>,
}

// ── Shared application state ──────────────────────────────
pub struct SharedState {
    /// Per-coin rolling buffers (write: analytics, read: scoring/api)
    pub buffers: DashMap<String, RwLock<PriceBuffer>>,
    /// Computed metrics per coin
    pub metrics: DashMap<String, CoinMetrics>,
    /// Current ranked list (updated by scoring engine)
    pub ranking: RwLock<Vec<RankedCoin>>,
    /// Heatmap snapshot
    pub heatmap: RwLock<Option<HeatmapData>>,
    /// Config snapshot for downstream use
    pub coins: Vec<CoinConfig>,
}

impl SharedState {
    pub fn new(cfg: &EngineConfig) -> Self {
        let buffers = DashMap::new();
        let metrics = DashMap::new();
        for coin in &cfg.market_universe {
            buffers.insert(
                coin.symbol.clone(),
                RwLock::new(PriceBuffer::new(cfg.rolling_window)),
            );
            metrics.insert(coin.symbol.clone(), CoinMetrics {
                symbol:   coin.symbol.clone(),
                category: Some(coin.category.clone()),
                confidence: coin.base_confidence,
                ..Default::default()
            });
        }
        Self {
            buffers,
            metrics,
            ranking: RwLock::new(Vec::new()),
            heatmap: RwLock::new(None),
            coins: cfg.market_universe.clone(),
        }
    }

    pub fn btc_returns(&self) -> Vec<f64> {
        self.buffers.get("BTC")
            .map(|b| b.read().returns_slice())
            .unwrap_or_default()
    }
}
