# BTC Market Reaction Visualization Engine

**Production-grade real-time crypto analytics platform**
Deterministic · Statistical · No ML · No Sentiment

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                  DATA INGESTION LAYER                        │
│                                                             │
│  Binance WebSocket (primary)       CoinGecko REST (fallback)│
│  wss://.../stream?streams=         /simple/price?ids=...    │
│  BTCUSDT@kline_1m × 20 coins      polled every 60s          │
│                 │                              │             │
│                 └──────────┬───────────────────┘             │
│                            ▼                                 │
│              broadcast::channel<Candle>  (cap=1024)          │
└────────────────────────────┬────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────┐
│                  PROCESSING LAYER                            │
│                                                             │
│  PriceBuffer (per coin, DashMap)                            │
│  ├─ closes:  VecDeque<f64>  [last 80+1 closes]              │
│  ├─ returns: VecDeque<f64>  [last 80 returns]               │
│  └─ volumes: VecDeque<f64>  [for liquidity confidence]      │
│                                                             │
│  Returns: r_t = (P_t - P_{t-1}) / P_{t-1}                  │
│  O(1) push/pop — no full-history recomputation              │
└────────────────────────────┬────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────┐
│                  ANALYTICS ENGINE                            │
│                                                             │
│  Per candle-close event (tokio task, one per coin batch)    │
│                                                             │
│  1. Pearson ρ                                               │
│     ρ = corr(r_BTC[t-N..t], r_coin[t-N..t])                │
│     Pure O(N) — Welford-style accumulators where N=window   │
│                                                             │
│  2. Reaction Strength R                                     │
│     R = avg(|r_coin|) / avg(|r_BTC|)                        │
│     R > 1 → coin amplifies BTC moves                        │
│     R < 1 → coin dampens BTC moves                          │
│                                                             │
│  3. Lag Detection  k* ∈ {0,1,2,3,4,5,6}                    │
│     ρ(k) = corr(r_BTC[t], r_coin[t+k])                     │
│     k* = argmax_k ρ(k)                                      │
│     7 Pearson calls × O(N) = O(7N) per coin per tick        │
│                                                             │
│  4. Lag Factor  L = e^{-k*}                                 │
│     k*=0 → L=1.000  (synchronous)                          │
│     k*=1 → L=0.368                                         │
│     k*=6 → L=0.002  (heavily penalized)                    │
│                                                             │
│  5. Confidence (noise suppression)                          │
│     base_conf × (1 / (1 + 2·σ_corr))                       │
│     where σ_corr = stddev of last 60 ρ values              │
│     → unstable correlation degrades ranking weight          │
│     → low-trust coins (WBT, HYPE) pre-penalized in config   │
│                                                             │
│  6. Score = max(ρ,0) × R × L × Confidence                  │
│     Negative correlation → 0 (no directional signal)        │
└────────────────────────────┬────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────┐
│                  SCORING ENGINE  (1s interval)               │
│                                                             │
│  Reads all CoinMetrics from DashMap                         │
│  Sorts by Score DESC                                        │
│  Assigns 1-indexed ranks                                    │
│  Classifies tiers:                                          │
│    Tier A: Score > 1.3  → TRADEABLE                         │
│    Tier B: 0.8–1.3     → WATCH                              │
│    Tier C: < 0.8       → IGNORE                             │
│  Rebuilds N×N cross-correlation heatmap matrix              │
│  Writes ranking + heatmap to SharedState (RwLock)           │
└────────────────────────────┬────────────────────────────────┘
                             │
┌────────────────────────────▼────────────────────────────────┐
│                  API LAYER  (Axum, port 8080)               │
│                                                             │
│  GET  /ranking              → Vec<RankedCoin> JSON          │
│  GET  /coin-metrics/:sym    → Full metrics for one coin     │
│  GET  /heatmap-data         → N×N correlation matrix        │
│  GET  /time-series/:sym     → Rolling closes + returns      │
│  WS   /ws/feed              → 1s push of ranking snapshot   │
│  GET  /health               → "ok"                          │
└─────────────────────────────────────────────────────────────┘
```

---

## Mathematical Specification

### 1. Returns
```
r_t = (P_t - P_{t-1}) / P_{t-1}
```
Simple percentage returns over 1-minute candle closes.

### 2. Pearson Correlation
```
ρ = Σ[(r_BTC_i - μ_BTC)(r_coin_i - μ_coin)] / √[Σ(r_BTC_i - μ_BTC)² · Σ(r_coin_i - μ_coin)²]
```
Rolling window N = 80 candles (~80 minutes of data).
Returns 0.0 if denominator < 1e-12 (flat series protection).

### 3. Reaction Strength
```
R = avg(|r_coin|) / avg(|r_BTC|)
```
Measures how strongly a coin moves relative to BTC.
R > 1.5 = high volatility amplifier
R ≈ 1.0 = symmetric reaction
R < 0.5 = dampened (stablecoin-like, DAI)

### 4. Lag Detection
```
ρ(k) = corr(r_BTC(t), r_coin(t + k))    k ∈ {0..6}
k*   = argmax_k ρ(k)
```
A lag k* = 2 means the coin takes 2 minutes to react to BTC moves.

### 5. Lag Transformation
```
L = e^{-k*}
```
Exponential decay penalizes slow reactors:

| k* | L      |
|----|--------|
| 0  | 1.000  |
| 1  | 0.368  |
| 2  | 0.135  |
| 3  | 0.050  |
| 6  | 0.002  |

### 6. Confidence Adjustment
```
σ_corr    = stddev(ρ_history[last 60])
stability = 1 / (1 + 2·σ_corr)
confidence = base_conf × stability
```
Coins with erratic correlation patterns are downweighted.

### 7. Final Score
```
Score = max(ρ, 0) × R × L × Confidence
```

---

## Tier System

| Tier | Score Range | Label      |
|------|-------------|------------|
| A    | > 1.3       | TRADEABLE  |
| B    | 0.8 – 1.3   | WATCH      |
| C    | < 0.8       | IGNORE     |

---

## Market Universe

| Symbol | Category         | Base Confidence |
|--------|------------------|-----------------|
| ETH    | Core BTC         | 0.97            |
| SOL    | Core BTC         | 0.93            |
| BNB    | Core BTC         | 0.95            |
| LTC    | Core BTC         | 0.90            |
| AVAX   | Core BTC         | 0.91            |
| LINK   | Core BTC         | 0.88            |
| BCH    | Hybrid           | 0.84            |
| ADA    | Hybrid           | 0.85            |
| DOT    | Hybrid           | 0.83            |
| XRP    | Hybrid           | 0.82            |
| UNI    | Hybrid           | 0.80            |
| XLM    | Hybrid           | 0.78            |
| MATIC  | Volatile         | 0.75            |
| ATOM   | Volatile         | 0.73            |
| DOGE   | Volatile         | 0.72            |
| XMR    | Noise            | 0.60            |
| DAI    | Noise (stablecoin) | 0.55          |
| WBT    | Noise (new)      | 0.45            |
| HYPE   | Noise (new)      | 0.40            |

---

## Performance Characteristics

- **Memory per coin**: ~80 × 8 bytes × 3 buffers = ~2 KB per coin = ~40 KB total
- **Compute per tick**: 7 Pearson calls × O(80) × 19 coins ≈ 10,640 operations
- **Latency**: sub-millisecond analytics update per candle close
- **WS push**: 1s interval, JSON serialization ~2 KB per snapshot

---

## Build & Run

```bash
# Build (release)
cargo build --release

# Run (reads config from env / defaults)
RUST_LOG=info ./target/release/engine

# Run benchmarks
cargo bench

# Run tests
cargo test
```

---

## API Examples

```bash
# Current ranking
curl http://localhost:8080/ranking | jq '.[0:5]'

# Detailed metrics for ETH
curl http://localhost:8080/coin-metrics/ETH

# Heatmap matrix
curl http://localhost:8080/heatmap-data | jq '.symbols, .matrix[0]'

# Live WebSocket feed
wscat -c ws://localhost:8080/ws/feed
```
