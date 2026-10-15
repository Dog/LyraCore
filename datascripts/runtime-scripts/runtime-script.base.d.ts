// Entity fields are snapshots; level-up events carry the attained level in newLevel.
interface Entity {
  readonly name: string;
  readonly is_player: boolean;
  readonly level: number;
  readonly health: number;
  readonly max_health: number;
  readonly map_id: number;
  readonly x: number;
  readonly y: number;
  readonly z: number;
}

interface PlayerEntity extends Entity {
  readonly is_player: true;
}

interface ScriptEvent {
  readonly name: string;
  readonly actor?: Entity;
  readonly target?: Entity;
}

interface PackageEvent extends ScriptEvent {}

interface EventBindingOptions {
  readonly priority?: number;
  readonly enabled?: boolean;
}

declare const event: ScriptEvent;

declare function heal(entity: Entity, amount: number): void;
declare function send_chat(player: Entity, text: string): void;
declare function grant_xp(player: Entity, amount: number): void;
