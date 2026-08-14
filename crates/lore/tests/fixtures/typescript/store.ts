/**
 * Toy search-store client used as a chunker fixture.
 */

import type { Chunk } from "./types";

export const DEFAULT_LIMIT = 20;

/** A single ranked search result. */
export interface Hit {
  chunk: Chunk;
  score: number;
}

export type Mode = "lexical" | "vector" | "hybrid";

/**
 * Thin client over the daemon's loopback HTTP API.
 */
export class StoreClient {
  private readonly base: string;
  private pending = 0;

  constructor(base: string, private readonly mode: Mode = "hybrid") {
    this.base = base.replace(/\/$/, "");
  }

  /** Runs a search and returns hits ordered by score, descending. */
  async search(query: string, limit: number = DEFAULT_LIMIT): Promise<Hit[]> {
    this.pending += 1;
    try {
      const url = `${this.base}/v1/search?q=${encodeURIComponent(query)}&limit=${limit}&mode=${this.mode}`;
      const res = await fetch(url);
      if (!res.ok) {
        throw new Error(`search failed: ${res.status} ${res.statusText}`);
      }
      const body = (await res.json()) as { hits: Hit[] };
      return body.hits.sort((a, b) => b.score - a.score);
    } finally {
      this.pending -= 1;
    }
  }

  get inFlight(): number {
    return this.pending;
  }
}

export const summarize = (hits: Hit[]): string =>
  hits.map((h) => `${h.chunk.path}:${h.score.toFixed(3)}`).join("\n");

export default StoreClient;
