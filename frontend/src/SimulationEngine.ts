export interface TradeParameters {
  entryPrice: number;
  takeProfit: number;
  stopLoss: number;
}

export class SimulationEngine {
  public isActive = false;
  public hasTriggeredEntry = false;
  public entryPrice = 0;
  public takeProfit = 0;
  public stopLoss = 0;

  public armTrade(parameters: TradeParameters): void {
    this.isActive = true;
    this.hasTriggeredEntry = false;
    this.entryPrice = parameters.entryPrice;
    this.takeProfit = parameters.takeProfit;
    this.stopLoss = parameters.stopLoss;
  }

  public reset(): void {
    this.isActive = false;
    this.hasTriggeredEntry = false;
    this.entryPrice = 0;
    this.takeProfit = 0;
    this.stopLoss = 0;
  }

  public updatePriceTick(lastPrice: number): void {
    if (!this.isActive || !Number.isFinite(lastPrice)) {
      return;
    }

    if (!this.hasTriggeredEntry && this.hasReachedEntry(lastPrice)) {
      this.hasTriggeredEntry = true;
      window.alert(`Trade entry triggered at ${lastPrice.toFixed(2)}`);
      return;
    }

    if (!this.hasTriggeredEntry) {
      return;
    }

    if (this.hasReachedTakeProfit(lastPrice)) {
      this.isActive = false;
      window.alert(`Trade target reached at ${lastPrice.toFixed(2)}`);
      return;
    }

    if (this.hasReachedStopLoss(lastPrice)) {
      this.isActive = false;
      window.alert(`Trade stopped out at ${lastPrice.toFixed(2)}`);
    }
  }

  private hasReachedEntry(lastPrice: number): boolean {
    if (this.takeProfit >= this.entryPrice && this.stopLoss <= this.entryPrice) {
      return lastPrice <= this.entryPrice;
    }

    return lastPrice >= this.entryPrice;
  }

  private hasReachedTakeProfit(lastPrice: number): boolean {
    if (this.takeProfit >= this.entryPrice) {
      return lastPrice >= this.takeProfit;
    }

    return lastPrice <= this.takeProfit;
  }

  private hasReachedStopLoss(lastPrice: number): boolean {
    if (this.stopLoss <= this.entryPrice) {
      return lastPrice <= this.stopLoss;
    }

    return lastPrice >= this.stopLoss;
  }
}
