export interface TradeParameters {
  entryPrice: number;
  takeProfit: number;
  stopLoss: number;
}

export type TradeOutcome = 'TP_HIT' | 'SL_HIT';
export type TradeDirection = 'long' | 'short';

// Minimum WS ticks to wait after entry before evaluating TP/SL.
// Prevents same-candle fill+outcome on fast moves (e.g. 3-tick spike).
const MIN_TICKS_AFTER_ENTRY = 3;

export class SimulationEngine {
  public isActive = false;
  public hasTriggeredEntry = false;
  public direction: TradeDirection = 'long';
  public entryPrice = 0;
  public entryFilledPrice = 0;
  public takeProfit = 0;
  public stopLoss = 0;
  public onAlert?: (message: string) => void;
  public onOutcome?: (outcome: TradeOutcome) => void;

  private ticksSinceEntry = 0;

  public armTrade(parameters: TradeParameters): void {
    this.isActive = true;
    this.hasTriggeredEntry = false;
    this.ticksSinceEntry = 0;
    this.entryPrice = parameters.entryPrice;
    this.entryFilledPrice = 0;
    this.takeProfit = parameters.takeProfit;
    this.stopLoss = parameters.stopLoss;
    // Direction fixed at arm time — not re-inferred per tick.
    this.direction = parameters.takeProfit >= parameters.entryPrice ? 'long' : 'short';
  }

  public reset(): void {
    this.isActive = false;
    this.hasTriggeredEntry = false;
    this.ticksSinceEntry = 0;
    this.entryPrice = 0;
    this.entryFilledPrice = 0;
    this.takeProfit = 0;
    this.stopLoss = 0;
  }

  public updatePriceTick(lastPrice: number): void {
    if (!this.isActive || !Number.isFinite(lastPrice)) {
      return;
    }

    if (!this.hasTriggeredEntry) {
      if (this.hasReachedEntry(lastPrice)) {
        this.hasTriggeredEntry = true;
        this.entryFilledPrice = lastPrice;
        this.ticksSinceEntry = 0;
        this.onAlert?.(`${this.direction.toUpperCase()} entry triggered at ${lastPrice.toFixed(2)}`);
      }
      return;
    }

    this.ticksSinceEntry++;
    if (this.ticksSinceEntry < MIN_TICKS_AFTER_ENTRY) {
      return;
    }

    if (this.hasReachedTakeProfit(lastPrice)) {
      this.isActive = false;
      this.onAlert?.(`Trade target reached at ${lastPrice.toFixed(2)}`);
      this.onOutcome?.('TP_HIT');
      return;
    }

    if (this.hasReachedStopLoss(lastPrice)) {
      this.isActive = false;
      this.onAlert?.(`Trade stopped out at ${lastPrice.toFixed(2)}`);
      this.onOutcome?.('SL_HIT');
    }
  }

  private hasReachedEntry(lastPrice: number): boolean {
    return this.direction === 'long'
      ? lastPrice <= this.entryPrice
      : lastPrice >= this.entryPrice;
  }

  private hasReachedTakeProfit(lastPrice: number): boolean {
    return this.direction === 'long'
      ? lastPrice >= this.takeProfit
      : lastPrice <= this.takeProfit;
  }

  private hasReachedStopLoss(lastPrice: number): boolean {
    return this.direction === 'long'
      ? lastPrice <= this.stopLoss
      : lastPrice >= this.stopLoss;
  }
}
