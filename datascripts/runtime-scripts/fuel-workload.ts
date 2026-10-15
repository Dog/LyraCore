// @event on_login
// @id 100999

function script(): number | void {
  const names: string[] = [];
  for (let i = 1; i <= 20; i += 1) {
    names.push("unit" + i);
  }
  const shouted = names.map((name) => name.toUpperCase());
  const roster = shouted.join(",");
  const actor = event.actor;
  if (actor && roster.length > 0) {
    grant_xp(actor, 25);
  }
  return roster.length;
}
