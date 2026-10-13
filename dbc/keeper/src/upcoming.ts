/**
 * Real upcoming football matches, for the demo: the match (teams and kickoff) is real; the result is not. A market made
 * from one still settles through the labelled practice feed, so the demo can show a whole market in minutes.
 *
 * Source: football-data.org (needs FOOTBALL_DATA_API_KEY on the worker). The key stays on the server; the browser only
 * ever sees the public match data this returns.
 */

export interface Upcoming {
  home: string;
  away: string;
  /** Unix seconds. */
  kickoff: number;
  competition: string;
}

/** A market title and a token label hold at most 24 bytes, so team names are plain ASCII and at most 18 characters. */
const TEAM = /^[A-Za-z0-9][A-Za-z0-9 .'&-]{1,17}$/;

const plain = (text: string): string =>
  text
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "") // Fenerbahçe -> Fenerbahce
    .replace(/[^A-Za-z0-9 .'&-]/g, "")
    .trim();

/** The best short name the API gives us that fits, or null if none does. */
function teamName(t: { shortName?: string; name?: string; tla?: string } | undefined): string | null {
  for (const candidate of [t?.shortName, t?.name, t?.tla]) {
    if (!candidate) continue;
    const name = plain(candidate);
    if (TEAM.test(name)) return name;
  }
  return null;
}

/** Turns football-data.org's `/v4/matches` answer into the matches the demo can use: not too soon, names that fit. */
export function toUpcoming(body: unknown, nowSecs: number, minLeadSecs = 15 * 60): Upcoming[] {
  const matches = (body as { matches?: unknown[] } | null)?.matches;
  if (!Array.isArray(matches)) return [];
  const out: Upcoming[] = [];
  for (const raw of matches) {
    const m = raw as { utcDate?: string; status?: string; competition?: { name?: string }; homeTeam?: object; awayTeam?: object };
    if (m.status && !["SCHEDULED", "TIMED"].includes(m.status)) continue;
    const kickoff = m.utcDate ? Math.floor(Date.parse(m.utcDate) / 1000) : NaN;
    const home = teamName(m.homeTeam);
    const away = teamName(m.awayTeam);
    if (!Number.isFinite(kickoff) || kickoff < nowSecs + minLeadSecs || !home || !away || home === away) continue;
    out.push({ home, away, kickoff, competition: plain(m.competition?.name ?? "Football").slice(0, 40) });
  }
  return out.sort((a, b) => a.kickoff - b.kickoff).slice(0, 40);
}

let cache: { at: number; matches: Upcoming[] } | null = null;

/** Fetches the next few days of matches (cached for 5 minutes: the free plan allows only a handful of calls a minute). */
export async function fetchUpcoming(apiKey: string, days = 3): Promise<Upcoming[]> {
  const now = Date.now();
  if (cache && now - cache.at < 5 * 60_000) return cache.matches;
  const day = (ms: number) => new Date(ms).toISOString().slice(0, 10);
  const url = `https://api.football-data.org/v4/matches?dateFrom=${day(now)}&dateTo=${day(now + days * 86_400_000)}`;
  const res = await fetch(url, { headers: { "X-Auth-Token": apiKey }, signal: AbortSignal.timeout(15_000) });
  if (!res.ok) throw new Error(`the football data service answered ${res.status}`);
  const matches = toUpcoming(await res.json(), Math.floor(now / 1000));
  cache = { at: now, matches };
  return matches;
}
