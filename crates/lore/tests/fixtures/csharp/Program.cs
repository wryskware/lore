using System;
using Lexomancy.Model;

// Top-level entry point: prints a scored word and exits.
Console.WriteLine($"lore fixture — {DateTime.UtcNow:O}");

var tiles = new[] { new Tile('L', 1), new Tile('O', 1), new Tile('R', 2), new Tile('E', 1) };
foreach (var tile in tiles)
{
    Console.WriteLine($"  {tile.Describe()}");
}

return Sum(tiles.Length, 40);

/// <summary>Helper declared after the top-level statements.</summary>
static int Sum(params int[] values)
{
    var total = 0;
    foreach (var value in values)
    {
        total += value;
    }

    return total;
}
