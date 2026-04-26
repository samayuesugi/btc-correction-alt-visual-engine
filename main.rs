// ============================================================
// BTC Market Reaction Visualization Engine
// Entry Point — production-grade real-time crypto analytics
// ============================================================
//
// Architecture layers (bottom → top):
//   ingestion  → analytics → scoring → api + viz
//
// All mathematics are deterministic / statistical only.
// No ML, no sentiment — pure time-series analysis.

use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, warn};
use tracing_subscriber::{fmt, EnvFilter};

mod config;
mod domain;
mod ingestion;
mod analytics;
mod scoring;
mod api;

use crate::config::EngineConfig;
use crate::domain::SharedState;
use crate::ingestion::{BinanceWsIngester, CoinGeckoFallback};
use crate::analytics::AnalyticsEngine;
use crate::scoring::ScoringEngine;
use crate::api::ApiServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── tracing / logging setup ──────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env()
            .add_directive("btc_market_reaction_engine=info".parse()?))
        .with_target(false)
        .compact()
        .init();

    info!("╔══════════════════════════════════════════════════╗");
    info!("║  BTC Market Reaction Visualization Engine v0.1  ║");
    info!("║  Deterministic Statistical Analytics Platform   ║");
    info!("╚══════════════════════════════════════════════════╝");

    // ── configuration ────────────────────────────────────────
    let cfg = EngineConfig::load()?;
    info!("Config loaded: window={} candles, coins={}",
          cfg.rolling_window, cfg.market_universe.len());

    // ── shared state (Arc-wrapped, DashMap per coin) ─────────
    let state = Arc::new(SharedState::new(&cfg));

    // ── broadcast channel: raw candle events → all consumers ─
    // capacity = 1024 ticks; no blocking on slow consumers
    let (candle_tx, _) = broadcast::channel(1024);

    // ── analytics engine ─────────────────────────────────────
    // Subscribes to candle_tx, maintains rolling buffers,
    // computes correlation / lag / reaction strength per coin.
    let analytics = AnalyticsEngine::new(
        Arc::clone(&state),
        candle_tx.subscribe(),
        cfg.rolling_window,
    );
    let analytics_handle = tokio::spawn(async move {
        if let Err(e) = analytics.run().await {
            warn!("Analytics engine error: {e}");
        }
    });

    // ── scoring engine ───────────────────────────────────────
    // Reads computed metrics from state, applies scoring
    // formula, emits ranked snapshot on interval.
    let scoring = ScoringEngine::new(Arc::clone(&state), cfg.scoring_interval_ms);
    let scoring_handle = tokio::spawn(async move {
        scoring.run().await;
    });

    // ── data ingestion ───────────────────────────────────────
    // Primary: Binance WebSocket (real-time kline/1m)
    // Fallback: CoinGecko REST polled every 60 s
    let binance = BinanceWsIngester::new(
        cfg.binance_ws_url.clone(),
        cfg.market_universe.iter().map(|c| c.symbol.clone()).collect(),
        candle_tx.clone(),
    );

    let coingecko = CoinGeckoFallback::new(
        cfg.coingecko_base_url.clone(),
        cfg.market_universe.iter().map(|c| c.coingecko_id.clone()).collect(),
        candle_tx.clone(),
    );

    let ingestion_handle = tokio::spawn(async move {
        binance.run_with_fallback(coingecko).await;
    });

    // ── REST / WebSocket API server ──────────────────────────
    let api = ApiServer::new(Arc::clone(&state), cfg.api_port);
    let api_handle = tokio::spawn(async move {
        if let Err(e) = api.run().await {
            warn!("API server error: {e}");
        }
    });

    info!("All subsystems started. API listening on :{}", cfg.api_port);

    // ── await all — propagate panics ─────────────────────────
    tokio::try_join!(
        analytics_handle,
        scoring_handle,
        ingestion_handle,
        api_handle,
    )?;

    Ok(())
}

