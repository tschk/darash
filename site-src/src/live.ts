/**
 * Live feed wiring on moonshine signals: WebSocket to /api/live with a
 * /api/stats polling fallback. Islands read the signals through the store.
 *
 * State rules:
 * - The connection lifecycle owns the feed state (live / polling / offline).
 *   pollOnce() only refreshes totals + recent events and never touches state,
 *   otherwise one successful poll would flip a live socket back to "polling".
 * - While live, totals refresh at most every 15 s, and never off of a
 *   self-generated /api/stats event (that would feed back into itself).
 * - On close we poll every 3 s and retry the socket every 5 s behind it.
 */
import { effect } from "@tschk/moonshine/runes";
import { apiGet, events, feed, totals, type LiveEvent } from "./store";

function sortNewestFirst(items: LiveEvent[]): LiveEvent[] {
  if (items.length < 2) return items;
  const first = items[0]?.ts ?? 0;
  const last = items[items.length - 1]?.ts ?? 0;
  const ascending = first <= last;
  return ascending ? [...items].reverse() : items;
}

function pollOnce(): Promise<void> {
  return apiGet("/api/stats", 8_000)
    .then((data) => {
      const payload = (data ?? {}) as {
        totals?: { requests?: number; searches?: number; fetches?: number; errors?: number };
        live?: LiveEvent[];
      };
      const t = payload.totals ?? {};
      totals.set({
        requests: Number(t.requests ?? 0),
        searches: Number(t.searches ?? 0),
        fetches: Number(t.fetches ?? 0),
        errors: Number(t.errors ?? 0),
      });
      const list = Array.isArray(payload.live) ? payload.live : [];
      events.set(sortNewestFirst(list).slice(0, 12));
    })
    .catch(() => {
      if (feed() === "connecting") feed.set("offline");
    });
}

export function startLiveFeed(): void {
  let ws: WebSocket | undefined;
  let pollTimer: ReturnType<typeof setInterval> | undefined;
  let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  let lastTotals = 0;

  const startPolling = () => {
    if (!pollTimer) {
      void pollOnce();
      pollTimer = setInterval(() => void pollOnce(), 3_000);
    }
  };

  const stopPolling = () => {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = undefined;
    }
  };

  const connect = () => {
    if (typeof WebSocket === "undefined") {
      startPolling();
      return;
    }
    try {
      ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/live`);
    } catch {
      ws = undefined;
    }
    if (!ws) {
      startPolling();
      return;
    }

    ws.addEventListener("open", () => {
      feed.set("live");
      stopPolling();
      lastTotals = Date.now();
      void pollOnce();
    });
    ws.addEventListener("message", (event) => {
      try {
        const payload = JSON.parse(String(event.data)) as { type?: string; event?: LiveEvent };
        if (payload.type === "event" && payload.event) {
          const next = [payload.event, ...events()].slice(0, 12);
          events.set(next);
          // Refresh totals at most every 15 s, and never off a stats poll we
          // caused ourselves — otherwise the feed amplifies its own requests.
          const isOwnStatsPoll = payload.event.path.startsWith("/api/stats");
          const due = Date.now() - lastTotals > 15_000;
          if (!isOwnStatsPoll && due) {
            lastTotals = Date.now();
            void pollOnce();
          }
        }
      } catch {
        // ignore malformed frames
      }
    });
    ws.addEventListener("close", () => {
      feed.set("polling");
      startPolling();
      // The socket is the better feed; keep trying it behind the poll fallback.
      if (!reconnectTimer) {
        reconnectTimer = setTimeout(() => {
          reconnectTimer = undefined;
          connect();
        }, 5_000);
      }
    });
    ws.addEventListener("error", () => {
      try {
        ws?.close();
      } catch {
        // already closed
      }
    });
  };

  // Drive the poll loop from a moonshine effect so the kernel owns the schedule.
  let lastState: string | undefined;
  effect(() => {
    const now = feed();
    if (lastState === now) return;
    lastState = now;
  });

  connect();
}
