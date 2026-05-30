use crate::quant::UnifiedMarketState;
use crate::state::derivatives::{BasisRegime, BasisState, GlobalOIState, LiquidationState};
use crate::state::liquidity::LiquidityAnalytics;
use crate::state::sentiment::FearGreedState;
use crate::state::volatility::VolatilityRegime;

/// A semantic interpretation of a market signal with confidence and severity.
#[derive(Debug, Clone)]
pub struct SemanticSignal {
    pub label: String,
    pub confidence: f32,
    pub explanation: String,
    pub severity: Option<String>,
}

impl SemanticSignal {
    fn unknown(label: &str, reason: &str) -> Self {
        Self {
            label: label.to_string(),
            confidence: 0.2,
            explanation: reason.to_string(),
            severity: None,
        }
    }
}

/// All semantic signals derived from a single `UnifiedMarketState` snapshot.
#[derive(Debug, Clone)]
pub struct AiSemanticState {
    pub leverage_state: SemanticSignal,
    pub volatility_state: SemanticSignal,
    pub liquidity_state: SemanticSignal,
    pub orderflow_state: SemanticSignal,
    pub directional_bias: SemanticSignal,
    pub liquidation_state: SemanticSignal,
    pub oi_divergence_state: SemanticSignal,
    pub basis_state: SemanticSignal,
    pub sentiment_state: SemanticSignal,
    /// Cross-signal contradictions detected. Each entry is a human-readable warning.
    pub contradictions: Vec<String>,
}

/// Converts raw `UnifiedMarketState` metrics into semantic signals.
/// Called by `format_market_brief` in ai.rs — not stored on the state struct
/// to avoid a `quant` → `state::semantic` → `quant` import cycle.
pub struct SignalInterpreter;

impl SignalInterpreter {
    pub fn interpret(state: &UnifiedMarketState) -> AiSemanticState {
        let leverage_state = Self::interpret_leverage(state);
        let volatility_state = Self::interpret_volatility(state);
        let liquidity_state = Self::interpret_liquidity(state);
        let orderflow_state = Self::interpret_orderflow(state);
        let directional_bias = Self::interpret_directional(state);
        let liquidation_state = Self::interpret_liquidations(&state.liquidations);
        let oi_divergence_state = Self::interpret_cross_exchange_oi(&state.global_oi);
        let basis_state = Self::interpret_basis(&state.basis);

        let sentiment_state = Self::interpret_sentiment(&state.sentiment);

        let contradictions = Self::detect_contradictions(
            &leverage_state,
            &orderflow_state,
            &liquidation_state,
            &basis_state,
            &sentiment_state,
            state,
        );

        AiSemanticState {
            leverage_state,
            volatility_state,
            liquidity_state,
            orderflow_state,
            directional_bias,
            liquidation_state,
            oi_divergence_state,
            basis_state,
            sentiment_state,
            contradictions,
        }
    }

    fn interpret_leverage(state: &UnifiedMarketState) -> SemanticSignal {
        let Some(fr) = state.funding_rate else {
            return SemanticSignal::unknown("funding unknown", "funding rate not yet available");
        };

        // Confidence drops when OI data is a proxy (less reliable leverage signal).
        let confidence: f32 = if state.oi_is_real { 0.9 } else { 0.55 };
        let fr_pct = fr * 100.0;

        if fr > 0.001 {
            SemanticSignal {
                label: "leverage overcrowding — longs".to_string(),
                confidence,
                explanation: format!(
                    "funding {:.4}% — heavy long premium, elevated short squeeze risk",
                    fr_pct
                ),
                severity: Some("high".to_string()),
            }
        } else if fr > 0.0003 {
            SemanticSignal {
                label: "mild long bias".to_string(),
                confidence,
                explanation: format!("funding {:.4}% — moderate long positioning", fr_pct),
                severity: None,
            }
        } else if fr < -0.001 {
            SemanticSignal {
                label: "leverage overcrowding — shorts".to_string(),
                confidence,
                explanation: format!(
                    "funding {:.4}% — heavy short premium, elevated long squeeze risk",
                    fr_pct
                ),
                severity: Some("high".to_string()),
            }
        } else if fr < -0.0003 {
            SemanticSignal {
                label: "mild short bias".to_string(),
                confidence,
                explanation: format!("funding {:.4}% — moderate short positioning", fr_pct),
                severity: None,
            }
        } else {
            SemanticSignal {
                label: "leverage neutral".to_string(),
                confidence,
                explanation: format!("funding {:.4}% — balanced positioning", fr_pct),
                severity: None,
            }
        }
    }

    fn interpret_volatility(state: &UnifiedMarketState) -> SemanticSignal {
        let Some(atr) = state.atr_14 else {
            return SemanticSignal::unknown(
                "volatility unknown",
                "insufficient candle history for ATR-14",
            );
        };

        let vol = &state.volatility;
        let atr_pct = atr / state.last_price * 100.0;

        let pctile_str = vol
            .atr_percentile
            .map(|p| format!(" | {:.0}th pctile", p))
            .unwrap_or_default();

        let dir_str = match vol.expanding {
            Some(true) => " (expanding)",
            Some(false) => " (contracting)",
            None => "",
        };

        let confidence = if vol.regime_confidence > 0.0 { vol.regime_confidence } else { 0.5 };

        match &vol.regime {
            VolatilityRegime::Compression => SemanticSignal {
                label: format!("volatility compression{dir_str}"),
                confidence,
                explanation: format!(
                    "ATR {:.2}% of price — tight range, breakout coiling{pctile_str}",
                    atr_pct
                ),
                severity: None,
            },
            VolatilityRegime::Normal => SemanticSignal {
                label: format!("normal volatility{dir_str}"),
                confidence,
                explanation: format!(
                    "ATR {:.2}% of price — balanced conditions{pctile_str}",
                    atr_pct
                ),
                severity: None,
            },
            VolatilityRegime::Elevated => SemanticSignal {
                label: format!("elevated volatility{dir_str}"),
                confidence,
                explanation: format!(
                    "ATR {:.2}% of price — high vol, widen stops{pctile_str}",
                    atr_pct
                ),
                severity: Some("elevated".to_string()),
            },
            VolatilityRegime::Extreme => SemanticSignal {
                label: format!("extreme volatility{dir_str}"),
                confidence,
                explanation: format!(
                    "ATR {:.2}% of price — crisis/event conditions, avoid entries{pctile_str}",
                    atr_pct
                ),
                severity: Some("high".to_string()),
            },
            VolatilityRegime::Unknown => SemanticSignal::unknown(
                "volatility regime unknown",
                "insufficient candle history for regime classification",
            ),
        }
    }

    fn interpret_liquidity(state: &UnifiedMarketState) -> SemanticSignal {
        Self::interpret_liquidity_analytics(&state.liquidity_analytics, state)
    }

    fn interpret_liquidity_analytics(
        analytics: &LiquidityAnalytics,
        state: &UnifiedMarketState,
    ) -> SemanticSignal {
        let bid = analytics.bid_wall.as_ref();
        let ask = analytics.ask_wall.as_ref();

        // Degraded feed: fall back to basic wall presence from wire data.
        if !analytics.feed_healthy {
            let bw = state.liquidity_walls.bid_wall_price;
            let aw = state.liquidity_walls.ask_wall_price;
            return match (bw, aw) {
                (Some(b), Some(a)) => SemanticSignal {
                    label: "liquidity walls present both sides".to_string(),
                    confidence: 0.45,
                    explanation: format!(
                        "bid wall {:.4} | ask wall {:.4} (feed degraded, no persistence data)",
                        b, a
                    ),
                    severity: None,
                },
                (Some(b), None) => SemanticSignal {
                    label: "support present, resistance thin".to_string(),
                    confidence: 0.4,
                    explanation: format!("bid wall {:.4} (feed degraded)", b),
                    severity: None,
                },
                (None, Some(a)) => SemanticSignal {
                    label: "resistance present, support thin".to_string(),
                    confidence: 0.4,
                    explanation: format!("ask wall {:.4} (feed degraded)", a),
                    severity: None,
                },
                (None, None) => SemanticSignal {
                    label: "liquidity vacuum".to_string(),
                    confidence: 0.35,
                    explanation: "no significant walls visible (feed degraded)".to_string(),
                    severity: Some("caution".to_string()),
                },
            };
        }

        // Spoof alert takes priority — unreliable walls mislead directional reads.
        if analytics.spoof_alert {
            let detail = Self::wall_summary_line(bid, ask);
            return SemanticSignal {
                label: "potential order-book spoof detected".to_string(),
                confidence: 0.70,
                explanation: format!(
                    "wall appeared briefly without price crossing it — treat walls as unreliable | {}",
                    detail
                ),
                severity: Some("caution".to_string()),
            };
        }

        // Build enriched description for each present wall.
        let bid_desc = bid.map(|w| {
            let persistence = if w.persistence_polls >= 6 {
                "persistent"
            } else if w.persistence_polls >= 3 {
                "established"
            } else {
                "new"
            };
            let replenish = if w.replenishment_detected { " (replenished)" } else { "" };
            format!("bid {:.4} [{persistence} ×{}{}]", w.price, w.persistence_polls, replenish)
        });
        let ask_desc = ask.map(|w| {
            let persistence = if w.persistence_polls >= 6 {
                "persistent"
            } else if w.persistence_polls >= 3 {
                "established"
            } else {
                "new"
            };
            let replenish = if w.replenishment_detected { " (replenished)" } else { "" };
            format!("ask {:.4} [{persistence} ×{}{}]", w.price, w.persistence_polls, replenish)
        });

        // Confidence scales with wall persistence — ephemeral walls are less meaningful.
        let base_confidence: f32 = match (bid, ask) {
            (Some(b), Some(a)) => {
                0.60 + 0.04 * (b.persistence_polls.min(5) + a.persistence_polls.min(5)) as f32
            }
            (Some(b), None) | (None, Some(b)) => {
                0.55 + 0.05 * b.persistence_polls.min(5) as f32
            }
            (None, None) => 0.60,
        };

        match (bid_desc, ask_desc) {
            (Some(b), Some(a)) => SemanticSignal {
                label: "liquidity walls both sides".to_string(),
                confidence: base_confidence.min(0.90),
                explanation: format!("{b} | {a}"),
                severity: None,
            },
            (Some(b), None) => SemanticSignal {
                label: "strong support, no visible resistance".to_string(),
                confidence: base_confidence.min(0.88),
                explanation: b,
                severity: None,
            },
            (None, Some(a)) => SemanticSignal {
                label: "resistance present, support thin".to_string(),
                confidence: base_confidence.min(0.88),
                explanation: a,
                severity: None,
            },
            (None, None) => SemanticSignal {
                label: "liquidity vacuum".to_string(),
                confidence: 0.60,
                explanation: "no significant walls on either side".to_string(),
                severity: Some("caution".to_string()),
            },
        }
    }

    fn wall_summary_line(
        bid: Option<&crate::state::liquidity::WallInfo>,
        ask: Option<&crate::state::liquidity::WallInfo>,
    ) -> String {
        match (bid, ask) {
            (Some(b), Some(a)) => format!("bid {:.4} score={:.2} | ask {:.4} score={:.2}", b.price, b.spoof_score, a.price, a.spoof_score),
            (Some(b), None) => format!("bid {:.4} spoof_score={:.2}", b.price, b.spoof_score),
            (None, Some(a)) => format!("ask {:.4} spoof_score={:.2}", a.price, a.spoof_score),
            (None, None) => "no walls".to_string(),
        }
    }

    fn interpret_orderflow(state: &UnifiedMarketState) -> SemanticSignal {
        let cvd = state.confluence.cvd_slope;
        let of = &state.orderflow;
        let confidence: f32 = if state.cvd_seeded { 0.85 } else { 0.5 };
        let warming = if !state.cvd_seeded { " (CVD warming up)" } else { "" };

        // Sweep/rejection overrides other flow labels — it is the strongest short-term signal.
        if of.sweep_detected {
            let dir = of.sweep_direction.as_deref().unwrap_or("unknown");
            let (label, detail) = match dir {
                "ask" => (
                    "ask sweep — bull trap rejection",
                    "upper wick > 1.2×ATR + bearish close; probable distribution / stop hunt above",
                ),
                "bid" => (
                    "bid sweep — bear trap rejection",
                    "lower wick > 1.2×ATR + bullish close; probable accumulation / stop hunt below",
                ),
                _ => ("sweep detected", "large wick with directional rejection"),
            };
            return SemanticSignal {
                label: label.to_string(),
                confidence: confidence * 0.95,
                explanation: format!(
                    "{} | CVD slope {:.2} | buy% {:.0}%{}",
                    detail,
                    cvd,
                    of.buy_pressure_pct * 100.0,
                    warming
                ),
                severity: Some("elevated".to_string()),
            };
        }

        // Absorption: high volume, tight range at wall — market absorbing supply/demand.
        if of.absorption_detected {
            let side = if of.buy_pressure_pct > 0.52 { "demand absorbed" } else { "supply absorbed" };
            return SemanticSignal {
                label: format!("absorption at wall — {side}"),
                confidence: confidence * 0.90,
                explanation: format!(
                    "elevated vol + tight range near wall | CVD slope {:.2} | buy% {:.0}%{}",
                    cvd,
                    of.buy_pressure_pct * 100.0,
                    warming
                ),
                severity: None,
            };
        }

        // Delta momentum: accelerating in one direction across recent candles.
        let momentum_label = if of.delta_momentum > 2.0 {
            Some("delta momentum accelerating bullish")
        } else if of.delta_momentum < -2.0 {
            Some("delta momentum accelerating bearish")
        } else {
            None
        };

        // Base signal from CVD slope + buy pressure.
        let (base_label, base_severity) = if cvd > 5.0 && of.buy_pressure_pct > 0.55 {
            ("aggressive buying pressure", None)
        } else if cvd > 0.0 || of.buy_pressure_pct > 0.53 {
            ("mild buying pressure", None)
        } else if cvd < -5.0 && of.buy_pressure_pct < 0.45 {
            ("aggressive selling pressure", None)
        } else if cvd < 0.0 || of.buy_pressure_pct < 0.47 {
            ("mild selling pressure", None)
        } else {
            ("balanced flow", None)
        };

        let label = momentum_label.unwrap_or(base_label).to_string();
        SemanticSignal {
            label,
            confidence,
            explanation: format!(
                "CVD slope {:.2} | buy% {:.0}% | delta momentum {:.2}{}",
                cvd,
                of.buy_pressure_pct * 100.0,
                of.delta_momentum,
                warming
            ),
            severity: base_severity.map(|s: &str| s.to_string()),
        }
    }

    fn interpret_directional(state: &UnifiedMarketState) -> SemanticSignal {
        let bias = &state.long_short_indicator;
        let confidence: f32 = match bias.as_str() {
            "StrongLong" | "StrongShort" => 0.9,
            "WeakLong" | "WeakShort" => 0.65,
            _ => 0.4,
        };

        SemanticSignal {
            label: bias.clone(),
            confidence,
            explanation: format!(
                "{} | MTF: 5m={} 15m={} 1h={} 4h={}",
                bias,
                state.confluence.tf_5m,
                state.confluence.tf_15m,
                state.confluence.tf_1h,
                state.confluence.tf_4h,
            ),
            severity: None,
        }
    }

    fn interpret_liquidations(liq: &LiquidationState) -> SemanticSignal {
        if !liq.feed_healthy {
            return SemanticSignal::unknown(
                "liquidation feed unavailable",
                "forceOrder stream not connected",
            );
        }

        let total_5m = liq.long_5m + liq.short_5m;
        let confidence: f32 = if total_5m > 0.0 { 0.85 } else { 0.6 };

        if total_5m < 50_000.0 {
            return SemanticSignal {
                label: "liquidation activity low".to_string(),
                confidence,
                explanation: format!("5m total: ${:.0}K — quiet market", total_5m / 1000.0),
                severity: None,
            };
        }

        if liq.imbalance_ratio > 2.0 {
            SemanticSignal {
                label: "trapped longs — cascade active".to_string(),
                confidence,
                explanation: format!(
                    "5m longs ${:.0}K / shorts ${:.0}K liq'd | velocity {:.1}/s",
                    liq.long_5m / 1000.0,
                    liq.short_5m / 1000.0,
                    liq.velocity,
                ),
                severity: Some("high".to_string()),
            }
        } else if liq.short_5m > 0.0 && liq.imbalance_ratio < 0.5 {
            SemanticSignal {
                label: "trapped shorts — squeeze risk".to_string(),
                confidence,
                explanation: format!(
                    "5m shorts ${:.0}K / longs ${:.0}K liq'd | velocity {:.1}/s",
                    liq.short_5m / 1000.0,
                    liq.long_5m / 1000.0,
                    liq.velocity,
                ),
                severity: Some("high".to_string()),
            }
        } else {
            SemanticSignal {
                label: "liquidation cascade active".to_string(),
                confidence,
                explanation: format!(
                    "5m longs ${:.0}K / shorts ${:.0}K | velocity {:.1}/s",
                    liq.long_5m / 1000.0,
                    liq.short_5m / 1000.0,
                    liq.velocity,
                ),
                severity: Some("elevated".to_string()),
            }
        }
    }

    fn interpret_basis(basis: &BasisState) -> SemanticSignal {
        if !basis.feed_healthy {
            return SemanticSignal::unknown("basis unavailable", "spot price feed not yet connected");
        }
        let pct = basis.basis_pct.unwrap_or(0.0);
        match &basis.regime {
            BasisRegime::StrongContango => SemanticSignal {
                label: "strong contango — longs pay heavy premium".to_string(),
                confidence: 0.85,
                explanation: format!(
                    "basis {pct:+.4}% — perp well above spot; short carry advantaged"
                ),
                severity: Some("elevated".to_string()),
            },
            BasisRegime::MildContango => SemanticSignal {
                label: "mild contango".to_string(),
                confidence: 0.80,
                explanation: format!("basis {pct:+.4}% — moderate long premium over spot"),
                severity: None,
            },
            BasisRegime::Neutral => SemanticSignal {
                label: "basis neutral".to_string(),
                confidence: 0.80,
                explanation: format!("basis {pct:+.4}% — perp aligned with spot"),
                severity: None,
            },
            BasisRegime::MildBackwardation => SemanticSignal {
                label: "mild backwardation".to_string(),
                confidence: 0.80,
                explanation: format!("basis {pct:+.4}% — moderate short premium over spot"),
                severity: None,
            },
            BasisRegime::StrongBackwardation => SemanticSignal {
                label: "strong backwardation — shorts pay heavy premium".to_string(),
                confidence: 0.85,
                explanation: format!(
                    "basis {pct:+.4}% — perp below spot; long carry advantaged"
                ),
                severity: Some("elevated".to_string()),
            },
            BasisRegime::Unavailable => {
                SemanticSignal::unknown("basis unavailable", "spot feed unhealthy")
            }
        }
    }

    fn interpret_cross_exchange_oi(global: &GlobalOIState) -> SemanticSignal {
        let exchanges_healthy = [global.binance.healthy, global.bybit.healthy, global.okx.healthy]
            .iter()
            .filter(|&&h| h)
            .count();

        if exchanges_healthy < 2 {
            return SemanticSignal::unknown(
                "cross-exchange OI unavailable",
                "fewer than 2 exchanges reporting — divergence not computable",
            );
        }

        let score = global.divergence_score;
        let label = &global.divergence_label;
        let confidence: f32 = if exchanges_healthy == 3 { 0.85 } else { 0.65 };

        let detail = {
            let parts: Vec<String> = [
                ("Binance", &global.binance),
                ("Bybit", &global.bybit),
                ("OKX", &global.okx),
            ]
            .iter()
            .filter_map(|(name, e)| {
                e.change_pct.map(|pct| format!("{name} {pct:+.2}%"))
            })
            .collect();
            parts.join(" | ")
        };

        if score > 0.7 {
            SemanticSignal {
                label: label.clone(),
                confidence,
                explanation: format!("divergence score {score:.2} (high) — {detail}"),
                severity: Some("elevated".to_string()),
            }
        } else if score > 0.3 {
            SemanticSignal {
                label: label.clone(),
                confidence,
                explanation: format!("divergence score {score:.2} (moderate) — {detail}"),
                severity: None,
            }
        } else {
            SemanticSignal {
                label: label.clone(),
                confidence,
                explanation: format!("exchanges aligned — {detail}"),
                severity: None,
            }
        }
    }

    fn interpret_sentiment(fg: &FearGreedState) -> SemanticSignal {
        if !fg.feed_healthy {
            return SemanticSignal::unknown(
                "sentiment unavailable",
                "Fear & Greed feed not yet polled",
            );
        }
        let v = fg.value;
        let explanation = format!(
            "F&G index {} ({}) — {}",
            v,
            fg.classification,
            if v <= 24 { "market in capitulation zone; contrarian longs historically advantaged" }
            else if v <= 44 { "fear-driven selling may overshoot fundamentals" }
            else if v <= 55 { "balanced sentiment, no strong crowd positioning" }
            else if v <= 74 { "crowd tilting bullish, complacency risk rising" }
            else { "euphoria zone; crowd heavily long, mean-reversion risk elevated" },
        );
        if v <= 24 {
            SemanticSignal {
                label: "extreme fear".to_string(),
                confidence: 0.85,
                explanation,
                severity: Some("elevated".to_string()),
            }
        } else if v <= 44 {
            SemanticSignal {
                label: "fear".to_string(),
                confidence: 0.80,
                explanation,
                severity: None,
            }
        } else if v <= 55 {
            SemanticSignal {
                label: "neutral sentiment".to_string(),
                confidence: 0.75,
                explanation,
                severity: None,
            }
        } else if v <= 74 {
            SemanticSignal {
                label: "greed".to_string(),
                confidence: 0.80,
                explanation,
                severity: None,
            }
        } else {
            SemanticSignal {
                label: "extreme greed".to_string(),
                confidence: 0.85,
                explanation,
                severity: Some("elevated".to_string()),
            }
        }
    }

    fn detect_contradictions(
        leverage: &SemanticSignal,
        orderflow: &SemanticSignal,
        liquidation: &SemanticSignal,
        basis: &SemanticSignal,
        sentiment: &SemanticSignal,
        state: &UnifiedMarketState,
    ) -> Vec<String> {
        let mut out = Vec::new();

        // Buying flow into a crowd of longs → late buyers face squeeze.
        if orderflow.label.contains("buying") && leverage.label.contains("overcrowding — longs") {
            out.push(
                "buying pressure into crowded long positioning — late longs face squeeze risk"
                    .to_string(),
            );
        }

        // Selling flow into a crowd of shorts → late sellers face squeeze.
        if orderflow.label.contains("selling") && leverage.label.contains("overcrowding — shorts") {
            out.push(
                "selling pressure into crowded short positioning — late shorts face squeeze risk"
                    .to_string(),
            );
        }

        // Long cascade active but bias is long → bias may flip.
        if liquidation.label.contains("trapped longs")
            && (state.long_short_indicator == "StrongLong"
                || state.long_short_indicator == "WeakLong")
        {
            out.push(
                "directional bias is long but long cascade active — bias may be invalidated"
                    .to_string(),
            );
        }

        // CVD diverging from price action (distribution / accumulation).
        let cvd = state.confluence.cvd_slope;
        let is_long_bias = state.long_short_indicator.contains("Long");
        if is_long_bias && cvd < -1.0 {
            out.push(
                "price trending up but CVD negative — potential distribution / smart money exiting"
                    .to_string(),
            );
        } else if !is_long_bias
            && state.long_short_indicator.contains("Short")
            && cvd > 1.0
        {
            out.push(
                "price trending down but CVD positive — potential accumulation / smart money entering"
                    .to_string(),
            );
        }

        // Extreme volatility + crowded leverage → liquidation cascade highly probable.
        if matches!(state.volatility.regime, VolatilityRegime::Extreme)
            && leverage.label.contains("overcrowding")
        {
            out.push(
                "extreme volatility with crowded positioning — cascade risk critical; avoid new entries"
                    .to_string(),
            );
        }

        // Compression + crowded leverage → spring-loaded: violent break when range resolves.
        if matches!(state.volatility.regime, VolatilityRegime::Compression)
            && leverage.label.contains("overcrowding")
        {
            out.push(
                "volatility compression with crowded positioning — spring-loaded: large liquidation cascade probable when range breaks"
                    .to_string(),
            );
        }

        // Strong basis deviation contradicts neutral funding (or vice-versa).
        if basis.label.contains("strong contango") && leverage.label.contains("leverage neutral") {
            out.push(
                "strong contango but funding neutral — basis may be leading funding; watch for funding catch-up"
                    .to_string(),
            );
        }
        if basis.label.contains("strong backwardation") && leverage.label.contains("overcrowding — shorts") {
            out.push(
                "strong backwardation with crowded shorts — shorts paying double (basis + funding); elevated squeeze risk"
                    .to_string(),
            );
        }

        // Sentiment extremes compounding or contradicting leverage positioning.
        if sentiment.label == "extreme greed" && leverage.label.contains("overcrowding — longs") {
            out.push(
                "extreme greed sentiment + crowded long leverage — late-cycle euphoria; elevated mean-reversion risk"
                    .to_string(),
            );
        }
        if sentiment.label == "extreme fear" && orderflow.label.contains("buying") {
            out.push(
                "aggressive buying into extreme fear — potential capitulation exhaustion; watch for reversal confirmation"
                    .to_string(),
            );
        }

        out
    }
}
