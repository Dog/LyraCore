import type { Spell, SpellEffect } from "../generated/types";
import type { FieldValue, PrimaryKey } from "./canonical.ts";
import {
  buildClaim,
  narrowValue,
  resolveColumn,
  spellEffectKey,
  spellKey,
} from "./delta.ts";
import { isPackageSpellId } from "./ids.ts";
import { BaseDataError, duplicateClaim, spellIdNotClientSafe } from "./refusals.ts";
import { BaseSnapshot, type BaseRow } from "./snapshot.ts";
import {
  SPELL_EFFECT_TABLE,
  SPELL_TABLE,
  type SpellEffectField,
  type SpellField,
} from "./schema.ts";
import { describeKey, type Claim } from "./canonical.ts";

export type EffectIndex = 0 | 1 | 2;

abstract class RowAuthor {
  protected readonly claimed = new Map<string, FieldValue>();

  protected constructor(
    readonly key: PrimaryKey,
    readonly operation: "insert" | "update",
    protected readonly base: BaseRow,
    protected readonly table: string,
  ) {}

  protected assign(field: string, value: unknown): void {
    const column = resolveColumn(this.table, field);
    this.claimed.set(column.column, narrowValue(column, value));
  }

  protected read(field: string): unknown {
    const column = resolveColumn(this.table, field);
    const value = this.claimed.get(column.column) ?? this.base.get(column.column);
    if (!value) {
      throw new BaseDataError(`no base value for \`${column.column}\``);
    }
    return value.value;
  }

  rowAsClaimed(): BaseRow {
    const row = new Map(this.base);
    for (const [name, value] of this.claimed) row.set(name, value);
    return row;
  }

  claim(): Claim | undefined {
    if (this.operation === "update") {
      if (this.claimed.size === 0) return undefined;
      return buildClaim(this.key, "update", new Map(this.claimed));
    }
    return buildClaim(this.key, "insert", new Map(this.rowAsClaimed()));
  }
}

export class SpellEffectAuthor extends RowAuthor {
  constructor(key: PrimaryKey, operation: "insert" | "update", base: BaseRow) {
    super(key, operation, base, SPELL_EFFECT_TABLE);
  }

  set<K extends SpellEffectField>(field: K, value: SpellEffect[K]): this {
    this.assign(field, value);
    return this;
  }

  get<K extends SpellEffectField>(field: K): SpellEffect[K] {
    return this.read(field) as SpellEffect[K];
  }
}

export class SpellAuthor extends RowAuthor {
  private readonly effects = new Map<number, SpellEffectAuthor>();

  constructor(
    private readonly registry: ClaimRegistry,
    key: PrimaryKey,
    operation: "insert" | "update",
    base: BaseRow,
    baseEffects: ReadonlyMap<number, BaseRow>,
  ) {
    super(key, operation, base, SPELL_TABLE);
    for (const [index, row] of baseEffects) {
      this.effects.set(
        index,
        new SpellEffectAuthor(spellEffectKey(key.spellId, index), operation, row),
      );
    }
  }

  set<K extends SpellField>(field: K, value: Spell[K]): this {
    this.assign(field, value);
    return this;
  }

  get<K extends SpellField>(field: K): Spell[K] {
    return this.read(field) as Spell[K];
  }

  effect(index: EffectIndex): SpellEffectAuthor {
    spellEffectKey(this.key.spellId, index);
    const effect = this.effects.get(index);
    if (!effect) {
      throw new BaseDataError(
        `spell ${this.key.spellId} has no effect ${index} in the Base Snapshot`,
      );
    }
    return effect;
  }

  clone(newId: number): SpellAuthor {
    const effects = new Map<number, BaseRow>();
    for (const [index, effect] of this.effects) {
      effects.set(index, effect.rowAsClaimed());
    }
    return this.registry.insert(newId, this.rowAsClaimed(), effects);
  }

  claims(): Claim[] {
    const claims: Claim[] = [];
    const own = this.claim();
    if (own) claims.push(own);
    for (const effect of this.effects.values()) {
      const claim = effect.claim();
      if (claim) claims.push(claim);
    }
    return claims;
  }
}

export class ClaimRegistry {
  private readonly spells = new Map<number, SpellAuthor>();

  constructor(private readonly snapshot: BaseSnapshot) {}

  spell(spellId: number): SpellAuthor {
    const key = spellKey(spellId);
    const existing = this.spells.get(spellId);
    if (existing) return existing;

    const author = new SpellAuthor(
      this,
      key,
      "update",
      this.snapshot.spell(spellId),
      this.snapshot.effectsOf(spellId),
    );
    this.spells.set(spellId, author);
    return author;
  }

  insert(
    spellId: number,
    base: BaseRow,
    baseEffects: Map<number, BaseRow>,
  ): SpellAuthor {
    const key = spellKey(spellId);
    if (!isPackageSpellId(spellId)) throw spellIdNotClientSafe(spellId);
    if (this.spells.has(spellId)) {
      throw duplicateClaim(SPELL_TABLE, describeKey(key));
    }
    const author = new SpellAuthor(this, key, "insert", base, baseEffects);
    this.spells.set(spellId, author);
    return author;
  }

  claims(): Claim[] {
    return [...this.spells.values()].flatMap((author) => author.claims());
  }
}
