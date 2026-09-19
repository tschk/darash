/**
 * Shared client state on the moonshine signal kernel.
 *
 * The page is authored in `.crepus` (crepuscularity) and the interactive
 * islands are Svelte 5 components; this store is the reactive backbone both
 * islands read from, exposed to Svelte through the readable() store adapter.
 */
import { state, derived } from "@tschk/moonshine/runes";

/** Same-origin when hosted; the public instance when previewing from file://. */
export const apiBase = state(
  typeof location !== "undefined" && location.protocol.startsWith("http")
    ? ""
    : "https://darash.tsc.hk",
);

export type Totals = {
  requests: number;
  searches: number;
  fetches: number;
  errors: number;
};

export type LiveEvent = {
  id: string;
  ts: number;
  method: string;
  path: string;
  kind: string;
  status: number;
  ms: number;
  client: string;
  tier: string;
};

export type FeedState = "connecting" | "live" | "polling" | "offline";

export const totals = state<Totals>({ requests: 0, searches: 0, fetches: 0, errors: 0 });
export const events = state<LiveEvent[]>([]);
export const feed = state<FeedState>("connecting");

export const totalRequests = derived(() => totals().requests);

/** Adapter: a moonshine signal seen as a Svelte readable store. */
export function readable<T>(signal: {
  (): T;
  subscribe(fn: (value: T) => void): () => void;
}): { subscribe(fn: (value: T) => void): () => void } {
  return {
    subscribe(fn) {
      // Svelte's store contract: deliver the current value immediately.
      // Moonshine listeners are pull-based (no value argument), so re-read
      // on every notification.
      fn(signal());
      return signal.subscribe(() => fn(signal()));
    },
  };
}

export const totalsStore = readable(totals);
export const eventsStore = readable(events);
export const feedStore = readable(feed);

/** GET JSON from the hosted API with a timeout. */
export async function apiGet(path: string, timeoutMs = 20_000): Promise<unknown> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(`${apiBase()}${path}`, {
      signal: controller.signal,
      cache: "no-store",
    });
    const body = await response.json();
    if (!response.ok) {
      const message =
        body && typeof body === "object" && "error" in body
          ? String((body as { error: unknown }).error)
          : `HTTP ${response.status}`;
      throw new Error(message);
    }
    return body;
  } finally {
    clearTimeout(timer);
  }
}

/** POST JSON to the hosted API with a timeout. */
export async function apiPost(path: string, payload: unknown, timeoutMs = 30_000): Promise<unknown> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(`${apiBase()}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(payload),
      signal: controller.signal,
    });
    const body = await response.json();
    if (!response.ok) {
      const message =
        body && typeof body === "object" && "error" in body
          ? String((body as { error: unknown }).error)
          : `HTTP ${response.status}`;
      throw new Error(message);
    }
    return body;
  } finally {
    clearTimeout(timer);
  }
}
