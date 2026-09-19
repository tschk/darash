/**
 * Accounts Durable Object.
 *
 * Single global instance (`ACCOUNTS.idFromName("global")`) that owns:
 *
 * - the API-key table: sha256-hashed keys with tier, name, and revocation
 *   (raw keys are returned once at creation and never stored), and
 * - per-identity usage windows (fixed-hour and fixed-day) used for rate
 *   limiting. Identities are either `key:<keyHash>` or `anon:<pseudonym>`.
 *
 * Concurrency: a Durable Object processes one request at a time, so the
 * get → check → put sequences below are atomic.
 */

export type Tier = "free" | "pro";

export type Account = {
  keyHash: string;
  tier: Tier;
  name: string;
  createdAt: number;
  revokedAt: number | null;
};

export type RateDecision = {
  allowed: boolean;
  tier: Tier | "anon";
  hourUsed: number;
  hourLimit: number;
  dayUsed: number;
  dayLimit: number;
  /** Seconds until the blocking window rolls over (when blocked). */
  retryAfter: number;
};

export const TIER_LIMITS: Record<Tier | "anon", { hour: number; day: number }> = {
  anon: { hour: 30, day: 100 },
  free: { hour: 60, day: 1_000 },
  pro: { hour: 600, day: 25_000 },
};

const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;

function json(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

function hourWindow(ts = Date.now()): { key: string; resetsAt: number } {
  const hour = Math.floor(ts / HOUR_MS);
  return { key: `h${hour}`, resetsAt: (hour + 1) * HOUR_MS };
}

function dayWindow(ts = Date.now()): { key: string; resetsAt: number } {
  const day = Math.floor(ts / DAY_MS);
  return { key: `d${day}`, resetsAt: (day + 1) * DAY_MS };
}

async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export class Accounts {
  private readonly state: DurableObjectState;

  constructor(state: DurableObjectState) {
    this.state = state;
  }

  private async put(key: string, value: unknown): Promise<void> {
    await this.state.storage.put(key, value);
  }

  private async get<T>(key: string): Promise<T | undefined> {
    return this.state.storage.get<T>(key);
  }

  /** Create an API key. Returns the raw key exactly once. */
  async createKey(tier: Tier, name: string): Promise<{ key: string; account: Account }> {
    const raw = `dk_${tier === "pro" ? "p" : "f"}_${crypto.randomUUID().replaceAll("-", "")}${crypto
      .randomUUID()
      .replaceAll("-", "")}`
      .slice(0, 40);
    const account: Account = {
      keyHash: await sha256Hex(raw),
      tier,
      name,
      createdAt: Date.now(),
      revokedAt: null,
    };
    await this.put(`key:${account.keyHash}`, account);
    return { key: raw, account };
  }

  async lookupKey(rawKey: string): Promise<Account | null> {
    const keyHash = await sha256Hex(rawKey);
    const account = await this.get<Account>(`key:${keyHash}`);
    if (!account || account.revokedAt !== null) {
      return null;
    }
    return account;
  }

  async revokeKey(rawKey: string): Promise<boolean> {
    const keyHash = await sha256Hex(rawKey);
    const account = await this.get<Account>(`key:${keyHash}`);
    if (!account) {
      return false;
    }
    if (account.revokedAt === null) {
      account.revokedAt = Date.now();
      await this.put(`key:${keyHash}`, account);
    }
    return true;
  }

  async listKeys(): Promise<Record<string, unknown>[]> {
    const entries = await this.state.storage.list<Account>({ prefix: "key:" });
    const today = dayWindow().key;
    const out: Record<string, unknown>[] = [];
    for (const [fullKey, account] of entries) {
      const usage = await this.get<number>(`usage:${today}:${fullKey.slice(4)}`);
      out.push({
        keyHash: account.keyHash.slice(0, 12) + "…",
        tier: account.tier,
        name: account.name,
        createdAt: account.createdAt,
        revoked: account.revokedAt !== null,
        usageToday: usage ?? 0,
      });
    }
    return out;
  }

  /**
   * Check and (on success) reserve nothing — usage is only counted when the
   * caller later confirms completion, so errors don't burn quota.
   */
  async checkRate(identity: string, tier: Tier | "anon"): Promise<RateDecision> {
    const limits = TIER_LIMITS[tier];
    const ts = Date.now();
    const hour = hourWindow(ts);
    const day = dayWindow(ts);
    const hourUsed = (await this.get<number>(`usage:${hour.key}:${identity}`)) ?? 0;
    const dayUsed = (await this.get<number>(`usage:${day.key}:${identity}`)) ?? 0;

    const decision: RateDecision = {
      allowed: hourUsed < limits.hour && dayUsed < limits.day,
      tier,
      hourUsed,
      hourLimit: limits.hour,
      dayUsed,
      dayLimit: limits.day,
      retryAfter: hourUsed < limits.hour ? Math.ceil((day.resetsAt - ts) / 1000) : Math.ceil((hour.resetsAt - ts) / 1000),
    };
    if (!decision.allowed) {
      decision.retryAfter = Math.max(1, decision.retryAfter);
    }
    return decision;
  }

  /** Record one completed request against the identity's windows. */
  async recordUsage(identity: string): Promise<void> {
    const hour = hourWindow();
    const day = dayWindow();
    await this.put(`usage:${hour.key}:${identity}`, ((await this.get<number>(`usage:${hour.key}:${identity}`)) ?? 0) + 1);
    await this.put(`usage:${day.key}:${identity}`, ((await this.get<number>(`usage:${day.key}:${identity}`)) ?? 0) + 1);
  }

  async accountSnapshot(identity: string, tier: Tier | "anon"): Promise<unknown> {
    const limits = TIER_LIMITS[tier];
    const hour = hourWindow();
    const day = dayWindow();
    return {
      tier,
      usage: {
        hour: (await this.get<number>(`usage:${hour.key}:${identity}`)) ?? 0,
        hourLimit: limits.hour,
        resetHourAt: hour.resetsAt,
        day: (await this.get<number>(`usage:${day.key}:${identity}`)) ?? 0,
        dayLimit: limits.day,
        resetDayAt: day.resetsAt,
      },
    };
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const body = request.method === "POST" ? await request.json<Record<string, unknown>>() : {};

    if (url.pathname === "/create" && request.method === "POST") {
      const tier = body.tier === "pro" ? "pro" : "free";
      const created = await this.createKey(tier, typeof body.name === "string" ? body.name : "");
      return json({ key: created.key, tier: created.account.tier, name: created.account.name, keyHash: created.account.keyHash });
    }
    if (url.pathname === "/lookup" && request.method === "POST") {
      const account = await this.lookupKey(String(body.key ?? ""));
      return json({ account }, account ? 200 : 401);
    }
    if (url.pathname === "/revoke" && request.method === "POST") {
      return json({ revoked: await this.revokeKey(String(body.key ?? "")) });
    }
    if (url.pathname === "/list" && request.method === "GET") {
      return json({ keys: await this.listKeys() });
    }

    const identity = String(body.identity ?? url.searchParams.get("identity") ?? "");
    if (!identity) {
      return json({ error: "identity is required" }, 400);
    }
    const tier = (typeof body.tier === "string" && body.tier !== "" ? body.tier : url.searchParams.get("tier")) as Tier | "anon" | null;
    const resolved: Tier | "anon" = tier === "free" || tier === "pro" || tier === "anon" ? tier : "anon";
    if (url.pathname === "/check" && request.method === "POST") {
      return json(await this.checkRate(identity, resolved));
    }
    if (url.pathname === "/record" && request.method === "POST") {
      await this.recordUsage(identity);
      return json({ ok: true });
    }
    if (url.pathname === "/snapshot" && request.method === "POST") {
      return json(await this.accountSnapshot(identity, resolved));
    }
    return json({ error: "not found" }, 404);
  }
}
