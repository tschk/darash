<script lang="ts">
  import { eventsStore, feedStore, totalsStore } from "../src/store";

  const totals = $derived($totalsStore);
  const events = $derived($eventsStore);
  const feed = $derived($feedStore);

  const methodColor: Record<string, string> = {
    GET: "text-blue-500",
    POST: "text-amber-400",
  };

  function statusColor(status: number): string {
    return status >= 400 ? "text-red-500" : "text-zinc-500";
  }

  const term =
    "border border-zinc-800 rounded-lg bg-zinc-950 shadow-[0_4px_20px_rgba(0,0,0,0.5)] overflow-hidden";
  const termBar =
    "flex items-center gap-2.5 px-3.5 py-2 border-b border-dashed border-zinc-800 text-zinc-400 text-[11.5px]";
  const termBody = "px-4 py-3.5 text-[12.5px] min-h-[150px]";
  const ev =
    "grid grid-cols-[minmax(0,1fr)_2.5rem_4.75rem] gap-2.5 items-baseline whitespace-nowrap text-xs md:grid-cols-[minmax(0,1fr)_2.5rem_4.75rem_9.5rem]";
</script>

<div class={term}>
  <div class={termBar}>
    <span
      class="w-2 h-2 rounded-full transition-colors duration-300"
      class:bg-green-500={feed === "live"}
      class:bg-yellow-500={feed === "polling"}
      class:bg-zinc-800={feed !== "live" && feed !== "polling"}
    ></span>
    <span>darash · /api/live</span>
    <span class="text-zinc-600 ml-auto">
      {feed === "live" ? "websocket" : feed === "polling" ? "polling fallback" : feed}
    </span>
  </div>
  <div class={termBody}>
    <div class="text-zinc-200 mb-3 pb-2 border-b border-dashed border-zinc-800">
      <span class="text-zinc-500">requests</span> {totals.requests}
      <span class="text-zinc-700 px-1">·</span>
      <span class="text-zinc-500">searches</span> {totals.searches}
      <span class="text-zinc-700 px-1">·</span>
      <span class="text-zinc-500">fetches</span> {totals.fetches}
      <span class="text-zinc-700 px-1">·</span>
      <span class="text-zinc-500">errors</span> {totals.errors}
    </div>
    <div class="flex flex-col gap-1.5">
      {#if events.length === 0}
        <p class="text-zinc-600">no events yet — run a search above</p>
      {:else}
        {#each events as e (e.id)}
          <div class={ev}>
            <span class="truncate">
              <span class={methodColor[e.method] ?? "text-blue-500"}>{e.method} </span>
              <span class="text-zinc-300">{e.path}</span>
              <span class="text-zinc-600"> {e.tier !== "anon" ? `· ${e.tier}` : ""}</span>
            </span>
            <span class={`text-right ${statusColor(e.status)}`}>{e.status}</span>
            <span class="text-zinc-500 text-right">{Math.round(e.ms)}ms</span>
            <span class="text-zinc-600 truncate hidden md:inline">client:{e.client}</span>
          </div>
        {/each}
      {/if}
    </div>
  </div>
</div>
