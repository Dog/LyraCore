import { run } from "../../lib/index.ts";

const PACKAGE = "fire_nova";

const FIREBALL = 133;

const FIRST_RANK_ID = 6_000_100;

const RANKS = 5;

// Matches T_AREA_ENEMY in the Module target taxonomy.
const T_AREA_ENEMY = 4;

const RADIUS_YD = 8;

const LADDER = [
  { level: 12, cost: 40, damage: 33 },
  { level: 22, cost: 80, damage: 71 },
  { level: 32, cost: 145, damage: 129 },
  { level: 42, cost: 240, damage: 224 },
  { level: 52, cost: 355, damage: 340 },
];

await run(PACKAGE, (data) => {
  const fireball = data.spell(FIREBALL);

  for (let rank = 0; rank < RANKS; rank += 1) {
    const tier = LADDER[rank]!;
    const nova = fireball.clone(FIRST_RANK_ID + rank);

    nova.set("name", `Fire Nova (Rank ${rank + 1})`);
    nova.set("castTimeMs", 0);
    nova.set("cooldownMs", 8_000);
    nova.set("rangeYd", 0); // Caster-centered spells have no cast range.
    nova.set("cost", tier.cost);
    nova.set("spellLevel", tier.level);
    nova.set("maxStacks", 0);

    const burst = nova.effect(0);
    burst.set("target", T_AREA_ENEMY);
    burst.set("radiusYd", RADIUS_YD);
    burst.set("basePoints", tier.damage);
    burst.set("dieSides", 0);
    burst.set("chainTargets", 0);
  }
});
