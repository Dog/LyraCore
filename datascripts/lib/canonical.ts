// Must byte-match the Rust Package Delta writer.

import { SPELL_EFFECT_TABLE, SPELL_TABLE, type FieldType } from "./schema.ts";

export const DELTA_VERSION = 1;

export interface FieldValue {
  readonly type: FieldType;
  readonly value: number | bigint | boolean | string;
}

export type PrimaryKey =
  | { readonly kind: "spell"; readonly spellId: number }
  | {
      readonly kind: "spellEffect";
      readonly spellId: number;
      readonly effectIndex: number;
    };

export interface Claim {
  readonly key: PrimaryKey;
  readonly operation: "insert" | "update";
  readonly fields: Map<string, FieldValue>;
}

export function tableOf(key: PrimaryKey): string {
  return key.kind === "spell" ? SPELL_TABLE : SPELL_EFFECT_TABLE;
}

export function describeKey(key: PrimaryKey): string {
  return key.kind === "spell"
    ? `{spell_id=${key.spellId}}`
    : `{spell_id=${key.spellId}, effect_index=${key.effectIndex}}`;
}

export function writeDelta(
  packageId: string,
  sourceHash: string,
  claims: Claim[],
): string {
  const ordered = [...claims].sort(compareKeys);
  const parts = ordered.map(writeClaim).join(",");
  return `{"version":${DELTA_VERSION},"package":${writeString(packageId)},"source_hash":${writeString(sourceHash)},"claims":[${parts}]}`;
}

// Claim ordering must match the Rust PrimaryKey's derived Ord.
export function compareKeys(a: Claim, b: Claim): number {
  const rank = (claim: Claim) => (claim.key.kind === "spell" ? 0 : 1);
  if (rank(a) !== rank(b)) return rank(a) - rank(b);
  if (a.key.spellId !== b.key.spellId) return a.key.spellId - b.key.spellId;
  if (a.key.kind === "spellEffect" && b.key.kind === "spellEffect") {
    return a.key.effectIndex - b.key.effectIndex;
  }
  return 0;
}

function writeClaim(claim: Claim): string {
  const names = [...claim.fields.keys()].sort();
  const fields = names
    .map((name) => `${writeString(name)}:${writeValue(claim.fields.get(name)!)}`)
    .join(",");
  return `{"table":${writeString(tableOf(claim.key))},"key":${writeKey(claim.key)},"operation":${writeString(claim.operation)},"fields":{${fields}}}`;
}

function writeKey(key: PrimaryKey): string {
  return key.kind === "spell"
    ? `{"spell_id":${key.spellId}}`
    : `{"spell_id":${key.spellId},"effect_index":${key.effectIndex}}`;
}

function writeValue(value: FieldValue): string {
  return `{"type":${writeString(value.type)},"value":${scalarLiteral(value)}}`;
}

export function scalarLiteral(value: FieldValue): string {
  switch (value.type) {
    case "u64":
      // JSON numbers lose integer precision above 2^53, so u64 values travel as strings.
      return `"${value.value}"`;
    case "f32":
      return f32Literal(value.value as number);
    case "bool":
      return value.value ? "true" : "false";
    case "string":
      return writeString(value.value as string);
    default:
      return String(value.value);
  }
}

// Match Rust's f32 Display with a decimal point and no exponent.
export function f32Literal(input: number): string {
  const text = shortestF32(Math.fround(input));
  return text.includes(".") ? text : `${text}.0`;
}

// The first f32 round-trip must match Rust's shortest decimal representation.
function shortestF32(value: number): string {
  if (!Number.isFinite(value)) {
    throw new RangeError(`${value} is not a finite f32`);
  }
  if (Object.is(value, -0)) return "-0";
  for (let precision = 1; precision <= 9; precision++) {
    const candidate = value.toPrecision(precision);
    if (Math.fround(Number(candidate)) === value) return plainDecimal(candidate);
  }
  return plainDecimal(value.toPrecision(9));
}

function plainDecimal(text: string): string {
  let sign = "";
  let body = text;
  if (body.startsWith("-")) {
    sign = "-";
    body = body.slice(1);
  }

  let exponent = 0;
  const marker = body.indexOf("e");
  if (marker >= 0) {
    exponent = Number(body.slice(marker + 1));
    body = body.slice(0, marker);
  }

  const dot = body.indexOf(".");
  let digits = dot >= 0 ? body.slice(0, dot) + body.slice(dot + 1) : body;
  let pointAt = (dot >= 0 ? dot : body.length) + exponent;

  while (pointAt <= 0) {
    digits = `0${digits}`;
    pointAt += 1;
  }
  while (pointAt > digits.length) {
    digits = `${digits}0`;
  }

  const whole = digits.slice(0, pointAt).replace(/^0+(?=\d)/, "");
  const fraction = digits.slice(pointAt).replace(/0+$/, "");
  return sign + whole + (fraction ? `.${fraction}` : "");
}

function writeString(text: string): string {
  return JSON.stringify(text);
}
