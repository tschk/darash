/**
 * Counter Durable Object.
 *
 * A single global instance (`COUNTER.idFromName("global")`) keeps:
 *
 * - cumulative totals ({ requests, searches, fetches, errors, startedAt }),
 *   persisted to Durable Object storage on every update, and
 * - the last {@link RING_SIZE} request events in an in-memory ring that is
 *   never persisted (it is live telemetry, not durable history).
 *
 * The worker talks to it over internal `fetch()` requests and it exposes a
 * WebSocket endpoint that streams each new event to connected clients.
 *
 * Concurrency: Durable Objects serialize event delivery and gate storage
 * writes, so read-modify-write on the in-memory totals is safe as long as the
 * mutation is synchronous and the `put()` is awaited before yielding.
 */

/** A single observed request. Mirrored by the worker in `index.ts`. */
export type RequestEvent = {
  id: string;
  ts: number;
  method: string;
  path: string;
  kind: "search" | "fetch" | "other";
  status: number;
  ms: number;
  /** First 8 hex chars of a salted hash of the client IP. */
  client: string;
};

/** Cumulative, durable counters. */
export type Totals = {
  requests: number;
  searches: number;
  fetches: number;
  errors: number;
  startedAt: number;
};

/** How many recent events the live ring keeps. */
export const RING_SIZE = 30;

const TOTALS_KEY = "totals";

function defaultTotals(): Totals {
  return { requests: 0, searches: 0, fetches: 0, errors: 0, startedAt: Date.now() };
}

function json(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

function normalizeEvent(value: unknown): RequestEvent {
  if (typeof value !== "object" || value === null) {
    throw new Error("event must be an object");
  }
  const record = value as Record<string, unknown>;
  const kind = record.kind;
  if (kind !== "search" && kind !== "fetch" && kind !== "other") {
    throw new Error("event.kind must be search|fetch|other");
  }
  return {
    id: typeof record.id === "string" ? record.id : crypto.randomUUID(),
    ts: typeof record.ts === "number" ? record.ts : Date.now(),
    method: typeof record.method === "string" ? record.method : "GET",
    path: typeof record.path === "string" ? record.path : "/",
    kind,
    status: typeof record.status === "number" ? record.status : 0,
    ms: typeof record.ms === "number" ? record.ms : 0,
    client: typeof record.client === "string" ? record.client : "unknown",
  };
}

export class Counter {
  private readonly state: DurableObjectState;
  private totals: Totals | null = null;
  private readonly events: RequestEvent[] = [];
  private readonly sockets = new Set<WebSocket>();

  constructor(state: DurableObjectState) {
    this.state = state;
    // Load (or initialize) totals before handling any request.
    state.blockConcurrencyWhile(async () => {
      const stored = await state.storage.get<Totals>(TOTALS_KEY);
      this.totals = stored ?? defaultTotals();
      if (!stored) {
        await state.storage.put(TOTALS_KEY, this.totals);
      }
    });
  }

  async fetch(request: Request): Promise<Response> {
    // A WebSocket upgrade passes straight through to the socket handler.
    if (request.headers.get("Upgrade")?.toLowerCase() === "websocket") {
      return this.handleWebSocket();
    }

    const { pathname } = new URL(request.url);
    switch (pathname) {
      case "/add":
        return this.handleAdd(request);
      case "/state":
        return json({ totals: await this.getTotals(), live: this.events });
      case "/totals":
        return json(await this.getTotals());
      case "/events":
        return json(this.events);
      default:
        return json({ error: "not found" }, 404);
    }
  }

  /** Read the current totals (in-memory; loaded once at construction). */
  async getTotals(): Promise<Totals> {
    if (!this.totals) {
      this.totals = (await this.state.storage.get<Totals>(TOTALS_KEY)) ?? defaultTotals();
    }
    return this.totals;
  }

  /** Record one event: bump totals, push to the ring, persist, broadcast. */
  async addEvent(event: RequestEvent): Promise<void> {
    const totals = await this.getTotals();
    totals.requests += 1;
    if (event.kind === "search") {
      totals.searches += 1;
    } else if (event.kind === "fetch") {
      totals.fetches += 1;
    }
    if (event.status >= 400) {
      totals.errors += 1;
    }

    this.events.push(event);
    if (this.events.length > RING_SIZE) {
      this.events.splice(0, this.events.length - RING_SIZE);
    }

    await this.state.storage.put(TOTALS_KEY, totals);
    this.broadcast(JSON.stringify({ type: "event", event }));
  }

  private async handleAdd(request: Request): Promise<Response> {
    let event: RequestEvent;
    try {
      event = normalizeEvent(await request.json());
    } catch (error) {
      return json({ error: "invalid event", detail: (error as Error).message }, 400);
    }
    await this.addEvent(event);
    return json({ ok: true });
  }

  private handleWebSocket(): Response {
    const pair = new WebSocketPair();
    const client = pair[0];
    const server = pair[1];
    server.accept();
    this.sockets.add(server);

    const cleanup = () => {
      this.sockets.delete(server);
    };
    server.addEventListener("close", cleanup);
    server.addEventListener("error", cleanup);
    server.addEventListener("message", (message) => {
      if (message.data === "ping") {
        try {
          server.send("pong");
        } catch {
          this.sockets.delete(server);
        }
      }
    });

    // Prime the client with the current totals and live ring.
    server.send(
      JSON.stringify({
        type: "hello",
        totals: this.totals ?? defaultTotals(),
        live: this.events,
      }),
    );

    return new Response(null, { status: 101, webSocket: client });
  }

  private broadcast(payload: string): void {
    for (const socket of this.sockets) {
      try {
        socket.send(payload);
      } catch {
        this.sockets.delete(socket);
      }
    }
  }
}
