/** Rejects completions from work superseded by a newer request. */
export class LatestRequest {
  #generation = 0;

  begin(): number {
    this.#generation += 1;
    return this.#generation;
  }

  invalidate(): void {
    this.#generation += 1;
  }

  accepts(generation: number): boolean {
    return generation === this.#generation;
  }
}
