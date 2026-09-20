<script lang="ts">
  import { apiGet, apiPost } from "../src/store";

  type Tab = "search" | "fetch";

  let tab = $state<Tab>("search");
  let query = $state("rust async");
  let mode = $state<"speed" | "balanced" | "quality">("balanced");
  let limit = $state(8);
  let busy = $state(false);
  let error = $state("");
  let elapsed = $state<number | null>(null);
  let instances = $state<string[]>([]);
  let tickMs = $state(0);
  let tickTimer: ReturnType<typeof setInterval> | undefined;

  function startTick() {
    tickMs = 0;
    tickTimer = setInterval(() => (tickMs += 50), 50);
  }
  function stopTick() {
    if (tickTimer !== undefined) clearInterval(tickTimer);
    tickTimer = undefined;
  }

  type Result = { title: string; url: string; content: string; engine: string; score: number };
  let results = $state<Result[]>([]);

  // fetch playground state
  let fetchUrl = $state("https://example.com");
  let extract = $state<"md" | "text" | "outline" | "select">("md");
  let selector = $state("h2");
  let budget = $state("");
  let output = $state("");
  let fetchMeta = $state("");

  const modes = [
    { id: "speed", label: "speed", hint: "one provider · ≤5 results" },
    { id: "balanced", label: "balanced", hint: "two providers · ≤10 results" },
    { id: "quality", label: "quality", hint: "all providers · ≤20 results" },
  ] as const;

  async function runSearch(event: SubmitEvent) {
    event.preventDefault();
    const q = query.trim();
    if (!q || busy) return;
    busy = true;
    error = "";
    results = [];
    elapsed = null;
    instances = [];
    startTick();
    try {
      const data = (await apiGet(
        `/api/search?q=${encodeURIComponent(q)}&mode=${mode}&limit=${limit}`,
      )) as { data?: { results?: Result[] }; meta?: { ms?: number; instances?: string[] } };
      results = data.data?.results ?? [];
      elapsed = data.meta?.ms ?? tickMs;
      instances = data.meta?.instances ?? [];
      if (results.length === 0) error = "no results";
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      stopTick();
      busy = false;
    }
  }

  async function runFetch(event: SubmitEvent) {
    event.preventDefault();
    const target = fetchUrl.trim();
    if (!target || busy) return;
    busy = true;
    error = "";
    output = "";
    fetchMeta = "";
    const started = performance.now();
    try {
      const params = new URLSearchParams({ url: target });
      if (extract === "text") params.set("text", "1");
      if (extract === "outline") params.set("outline", "1");
      if (extract === "select") {
        params.set("select", selector);
        params.set("limit", "10");
      }
      if (extract === "md" && budget.trim()) params.set("budget", budget.trim());
      const data = (await apiGet(`/api/fetch?${params.toString()}`)) as {
        data: unknown;
        meta?: { status?: number; ms?: number; bytes?: number };
      };
      elapsed = data.meta?.ms ?? Math.round(performance.now() - started);
      fetchMeta = `status ${data.meta?.status ?? "—"} · ${data.meta?.bytes ?? "—"} B · ${elapsed}ms`;
      let text = JSON.stringify(data.data, null, 2);
      if (text.length > 8_000) text = `${text.slice(0, 8_000)}\n… truncated (${text.length} bytes)`;
      output = text;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  // UnoCSS utility strings for the shared chrome.
  const term =
    "border border-zinc-800 rounded-lg bg-zinc-950 shadow-[0_4px_20px_rgba(0,0,0,0.5)] overflow-hidden";
  const termBar =
    "flex items-center gap-2.5 px-3.5 py-2 border-b border-dashed border-zinc-800 text-zinc-400 text-[11.5px]";
  const termBody = "px-4 py-3.5 text-[12.5px] min-h-[150px]";
  const input =
    "flex-1 min-w-0 bg-zinc-900 border border-zinc-800 rounded-md text-zinc-100 font-mono text-[13px] px-2.5 py-[7px] outline-none focus:border-blue-500 placeholder:text-zinc-600";
  const inlineInput =
    "w-auto flex-none bg-zinc-900 border border-zinc-800 rounded-md text-zinc-100 font-mono text-[13px] px-2.5 py-[7px] outline-none focus:border-blue-500 placeholder:text-zinc-600";
  const runBtn =
    "border border-blue-500 bg-blue-500 text-zinc-950 rounded-md font-mono text-[12.5px] font-bold px-3.5 py-[7px] cursor-pointer disabled:opacity-50 disabled:cursor-wait";
  const btnOff =
    "border border-zinc-800 bg-transparent text-zinc-400 rounded px-2 py-[3px] text-[11.5px] font-mono cursor-pointer hover:text-zinc-100 transition-colors";
  const btnOn =
    "border border-blue-500 bg-zinc-900 text-zinc-100 rounded px-2 py-[3px] text-[11.5px] font-mono cursor-pointer transition-colors";
  const dot = "w-2 h-2 rounded-full transition-colors duration-300";
</script>

<div class={term}>
  <div class={termBar}>
    <span
      class={dot}
      class:bg-green-500={results.length > 0 || output !== ""}
      class:bg-zinc-800={results.length === 0 && output === ""}
    ></span>
    <span>darash console · same-origin api</span>
    <div class="flex gap-1 ml-auto">
      <button
        type="button"
        class={tab === "search" ? btnOn : btnOff}
        onclick={() => (tab = "search")}>search</button
      >
      <button
        type="button"
        class={tab === "fetch" ? btnOn : btnOff}
        onclick={() => (tab = "fetch")}>fetch</button
      >
    </div>
  </div>

  <div class={termBody}>
    {#if tab === "search"}
      <form class="flex flex-col gap-3" onsubmit={runSearch}>
        <div class="flex gap-2">
          <span class="text-blue-500 select-none shrink-0">$</span>
          <input
            class={input}
            type="text"
            bind:value={query}
            placeholder="search the web…"
            aria-label="search query"
            maxlength="512"
          />
          <button class={runBtn} type="submit" disabled={busy}>{busy ? "…" : "run"}</button>
        </div>
        <div class="flex flex-wrap items-center gap-2">
          {#each modes as m (m.id)}
            <button
              type="button"
              class={mode === m.id ? btnOn : btnOff}
              title={m.hint}
              onclick={() => (mode = m.id)}>{m.label}</button
            >
          {/each}
          <select class={inlineInput} bind:value={limit} aria-label="result limit">
            <option value={5}>limit 5</option>
            <option value={8}>limit 8</option>
            <option value={10}>limit 10</option>
            <option value={20}>limit 20</option>
          </select>
        </div>
      </form>

      <div class="mt-3 flex flex-col gap-2">
        {#if error}
          <p class="text-red-500">error: {error}</p>
        {/if}
        {#if busy}
          <p class="text-zinc-500" role="status">
            searching{tickMs >= 600 ? `… ${tickMs}ms` : "…"}
          </p>
          {#each Array.from({ length: Math.min(limit, 5) }) as _, i (i)}
            <div
              class="flex flex-col gap-1.5 py-1.5 border-b border-dashed border-zinc-800 animate-pulse"
              aria-hidden="true"
            >
              <div class="h-3.5 w-[45%] bg-zinc-800 rounded"></div>
              <div class="h-2.5 w-[65%] bg-zinc-800/70 rounded"></div>
              <div class="h-2.5 w-[88%] bg-zinc-800/50 rounded"></div>
            </div>
          {/each}
        {/if}
        {#if elapsed !== null}
          <p class="text-zinc-500">
            {results.length} result{results.length === 1 ? "" : "s"} in {elapsed}ms · via
            {instances.length > 0 ? instances.join(", ") : "fallback engines"}
          </p>
        {/if}
        {#each results as r (r.url)}
          <div class="flex flex-col gap-0.5 py-1.5 border-b border-dashed border-zinc-800">
            <a
              class="text-blue-400 hover:underline"
              href={r.url}
              target="_blank"
              rel="noopener noreferrer">{r.title}</a
            >
            <div class="text-zinc-600 truncate">{r.url}</div>
            <div class="text-zinc-400">{r.content}</div>
            <div class="text-zinc-600">
              {r.engine} · score {r.score}
            </div>
          </div>
        {/each}
      </div>
    {:else}
      <form class="flex flex-col gap-3" onsubmit={runFetch}>
        <div class="flex gap-2">
          <span class="text-blue-500 select-none shrink-0">$</span>
          <input
            class={input}
            type="url"
            bind:value={fetchUrl}
            placeholder="https://example.com"
            aria-label="url to fetch"
          />
          <button class={runBtn} type="submit" disabled={busy}>{busy ? "…" : "run"}</button>
        </div>
        <div class="flex flex-wrap items-center gap-2">
          {#each ["md", "text", "outline", "select"] as const as m (m)}
            <button
              type="button"
              class={extract === m ? btnOn : btnOff}
              onclick={() => (extract = m)}>{m}</button
            >
          {/each}
          {#if extract === "select"}
            <input class={inlineInput} bind:value={selector} aria-label="css selector" />
          {/if}
          {#if extract === "md"}
            <input
              class={inlineInput}
              bind:value={budget}
              placeholder="budget (tokens)"
              aria-label="token budget"
            />
          {/if}
        </div>
      </form>

      <div class="mt-3 flex flex-col gap-2">
        {#if error}
          <p class="text-red-500">error: {error}</p>
        {/if}
        {#if fetchMeta}
          <p class="text-zinc-500">{fetchMeta}</p>
        {/if}
        {#if output}
          <pre
            class="m-0 px-3 py-2.5 bg-zinc-900 border border-zinc-800 rounded-md text-xs leading-relaxed overflow-x-auto whitespace-pre-wrap break-words"
          >{output}</pre
          >
        {:else}
          <p class="text-zinc-600">
            markdown · text · outline · selector extraction — same output as the darash crate.
          </p>
        {/if}
      </div>
    {/if}
  </div>
</div>
