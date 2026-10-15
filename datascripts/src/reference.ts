import type { Spell } from "../generated/types";

export function spellRank(base: Spell, rank: number): Spell {
  return {
    ...base,
    spellId: base.spellId + rank,
    name: `${base.name} (Rank ${rank})`,
    cost: base.cost * rank,
    spellLevel: base.spellLevel,
    cooldownMs: base.cooldownMs,
    rangeYd: base.rangeYd,
  };
}

export function describeRank(spell: Spell): string {
  return `${spell.name} #${spell.spellId}: ${spell.cost} power, ${spell.cooldownMs}ms cooldown`;
}
