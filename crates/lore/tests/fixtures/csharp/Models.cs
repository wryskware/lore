using System;
using System.Collections.Generic;
using System.Linq;

namespace Lexomancy.Model;

/// <summary>A single letter tile.</summary>
/// <param name="Letter">The glyph shown to the player.</param>
/// <param name="Value">Base score contribution.</param>
public readonly record struct Tile(char Letter, int Value)
{
    public string Describe() => $"{Letter}({Value})";
}

public record Word(string Text, IReadOnlyList<Tile> Tiles)
{
    public int Raw => Tiles.Sum(t => t.Value);
}

/// <summary>Scores words against a multiplier table.</summary>
public sealed class Scorer(IReadOnlyDictionary<char, int> table, int bonus = 0)
{
    private readonly IReadOnlyDictionary<char, int> _table = table;

    /// <summary>Flat bonus applied to every scored word.</summary>
    public int Bonus => bonus;

    /// <summary>Scores <paramref name="word"/>, applying per-letter multipliers.</summary>
    public int Score(Word word)
    {
        var total = 0;
        foreach (var tile in word.Tiles)
        {
            var multiplier = _table.TryGetValue(tile.Letter, out var m) ? m : 1;
            total += tile.Value * multiplier;
        }

        return total + bonus;
    }

    public override string ToString() => $"Scorer(bonus: {bonus}, letters: {_table.Count})";
}

public interface IScoreSink
{
    void Publish(Word word, int score);
}

public enum Rarity
{
    Common,
    Rare,
    Mythic,
}
