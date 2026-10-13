import assert from "node:assert/strict";
import { test } from "node:test";
import { toUpcoming } from "../src/upcoming.js";

const NOW = 1_800_000_000;
const match = (over: object = {}) => ({
  utcDate: new Date((NOW + 3 * 3600) * 1000).toISOString(),
  status: "TIMED",
  competition: { name: "UEFA Champions League" },
  homeTeam: { shortName: "Arsenal", name: "Arsenal FC", tla: "ARS" },
  awayTeam: { shortName: "Lille", name: "Lille OSC", tla: "LIL" },
  ...over,
});

test("a scheduled match becomes a demo fixture with a short plain name", () => {
  const [m] = toUpcoming({ matches: [match()] }, NOW);
  assert.deepEqual(m, { home: "Arsenal", away: "Lille", kickoff: NOW + 3 * 3600, competition: "UEFA Champions League" });
});

test("accents are removed and over-long names fall back to a shorter one", () => {
  const [m] = toUpcoming({ matches: [match({ homeTeam: { shortName: "Fenerbahçe" }, awayTeam: { shortName: "Paris Saint-Germain FC", tla: "PSG" } })] }, NOW);
  assert.equal(m!.home, "Fenerbahce");
  assert.equal(m!.away, "PSG");
});

test("matches that already started, are about to start, or have no usable names are left out", () => {
  const rows = toUpcoming(
    { matches: [match({ status: "IN_PLAY" }), match({ utcDate: new Date((NOW + 60) * 1000).toISOString() }), match({ homeTeam: { shortName: "" } }), match()] },
    NOW,
  );
  assert.equal(rows.length, 1);
});

test("soonest first, and nothing breaks on a bad answer", () => {
  const later = match({ utcDate: new Date((NOW + 9 * 3600) * 1000).toISOString(), homeTeam: { shortName: "Roma" }, awayTeam: { shortName: "Real Madrid" } });
  const rows = toUpcoming({ matches: [later, match()] }, NOW);
  assert.deepEqual(rows.map((r) => r.home), ["Arsenal", "Roma"]);
  assert.deepEqual(toUpcoming(null, NOW), []);
  assert.deepEqual(toUpcoming({ matches: "no" }, NOW), []);
});
