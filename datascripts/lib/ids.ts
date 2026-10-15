export function packedSpellEffectId(spellId: number, effectIndex: number): bigint {
  return (BigInt(spellId) << 2n) | BigInt(effectIndex);
}

// Vanilla spells have three effect slots.
export const MAX_SPELL_EFFECT_INDEX = 2;

export const PACKAGE_SPELL_ID_FLOOR = 6_000_000;
export const PACKAGE_SPELL_ID_CEIL = 6_999_999;

export const FIXTURE_SPELL_ID_FLOOR = 50_000;
export const FIXTURE_SPELL_ID_CEIL = 50_999;

export const FIXTURE_RESERVED_ID_FLOOR = 5_090_000;
export const FIXTURE_RESERVED_ID_CEIL = 5_099_999;

export function isPackageSpellId(spellId: number): boolean {
  return spellId >= PACKAGE_SPELL_ID_FLOOR && spellId <= PACKAGE_SPELL_ID_CEIL;
}

export function isFixtureReservedSpellId(spellId: number): boolean {
  return (
    (spellId >= FIXTURE_SPELL_ID_FLOOR && spellId <= FIXTURE_SPELL_ID_CEIL) ||
    (spellId >= FIXTURE_RESERVED_ID_FLOOR && spellId <= FIXTURE_RESERVED_ID_CEIL)
  );
}
