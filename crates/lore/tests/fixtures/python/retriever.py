"""Toy hybrid retriever used as a chunker fixture."""

from __future__ import annotations

import math
from dataclasses import dataclass, field

DEFAULT_K = 60


@dataclass
class Hit:
    """One ranked result."""

    doc_id: int
    score: float
    source: str = "lexical"


class Retriever:
    """Fuses a lexical ranking with a vector ranking using RRF."""

    def __init__(self, k: int = DEFAULT_K) -> None:
        self.k = k
        self._lexical: list[int] = []
        self._vector: list[int] = []

    def feed(self, lexical: list[int], vector: list[int]) -> None:
        """Records the two candidate rankings for the next fuse."""
        self._lexical = list(lexical)
        self._vector = list(vector)

    def fuse(self) -> list[Hit]:
        """Reciprocal-rank fusion over the two recorded rankings.

        Ties break toward the lexical ranking, which is cheaper to trust.
        """
        scores: dict[int, float] = {}
        for rank, doc_id in enumerate(self._lexical):
            scores[doc_id] = scores.get(doc_id, 0.0) + 1.0 / (self.k + rank + 1)
        for rank, doc_id in enumerate(self._vector):
            scores[doc_id] = scores.get(doc_id, 0.0) + 1.0 / (self.k + rank + 1)
        ordered = sorted(scores.items(), key=lambda item: (-item[1], item[0]))
        return [Hit(doc_id=d, score=s) for d, s in ordered]

    @staticmethod
    def normalize(values: list[float]) -> list[float]:
        norm = math.sqrt(sum(v * v for v in values)) or 1.0
        return [v / norm for v in values]


def main() -> None:
    r = Retriever()
    r.feed([1, 2, 3], [3, 1, 4])
    for hit in r.fuse():
        print(f"{hit.doc_id}: {hit.score:.4f}")


if __name__ == "__main__":
    main()
