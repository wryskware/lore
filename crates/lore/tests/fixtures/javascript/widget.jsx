import React, { useState } from "react";

const MAX_ROWS = 10;

/**
 * Renders a compact list of search hits.
 */
export function HitList({ hits, onPick }) {
  const [selected, setSelected] = useState(null);

  function pick(hit) {
    setSelected(hit.id);
    onPick?.(hit);
  }

  return (
    <ul className="hits">
      {hits.slice(0, MAX_ROWS).map((hit) => (
        <li key={hit.id} onClick={() => pick(hit)} data-active={hit.id === selected}>
          {hit.path} <span className="score">{hit.score.toFixed(2)}</span>
        </li>
      ))}
    </ul>
  );
}

export class HitStore {
  #hits = [];

  add(hit) {
    this.#hits.push(hit);
    return this;
  }

  get all() {
    return [...this.#hits];
  }
}
