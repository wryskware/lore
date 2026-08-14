// Copyright (c) Wryskware. MIT license.
#nullable enable

using System;
using System.Collections.Generic;
using UnityEngine;

namespace Lexomancy.Board
{
    /// <summary>
    /// Drives the letter grid: placement, scoring, and the tile cache.
    /// </summary>
    /// <remarks>Scoring rules live in design doc 3.1.</remarks>
    [DisallowMultipleComponent]
    [RequireComponent(typeof(BoardView))]
    public sealed class BoardController : MonoBehaviour
    {
        public const int Width = 7;
        public const int Height = 9;

        [SerializeField]
        private BoardView _view = null!;

        private readonly Dictionary<Vector2Int, Tile> _tiles = new();

        /// <summary>Raised whenever a word is scored.</summary>
        public event Action<string, int>? WordScored;

        /// <summary>Total score accumulated this run.</summary>
        public int Score { get; private set; }

        private void Awake()
        {
            if (_view == null)
            {
                throw new InvalidOperationException($"{nameof(BoardController)} needs a view");
            }

            _tiles.Clear();
        }

        // Polled every frame; keep this allocation free.
        private void Update()
        {
            if (_tiles.Count == 0)
            {
                return;
            }

            foreach (var pair in _tiles)
            {
                pair.Value.Tick(Time.deltaTime);
            }
        }

        /// <summary>Places <paramref name="tile"/> at <paramref name="cell"/>.</summary>
        /// <returns><c>true</c> when the cell was free.</returns>
        public bool TryPlace(Tile tile, Vector2Int cell)
        {
            if (_tiles.ContainsKey(cell))
            {
                return false;
            }

            _tiles[cell] = tile;
#if UNITY_EDITOR
            Debug.Log($"placed {tile.Letter} at {cell.x},{cell.y} (score {Score})");
#endif
            WordScored?.Invoke(tile.Letter.ToString(), tile.Value);
            Score += tile.Value;
            return true;
        }

        private enum Phase
        {
            Idle,
            Dragging,
            Scoring,
        }

        private sealed class TileComparer : IComparer<Tile>
        {
            public int Compare(Tile? a, Tile? b)
            {
                return (a?.Value ?? 0).CompareTo(b?.Value ?? 0);
            }
        }
    }
}
