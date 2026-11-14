<script lang="ts">
  import { clock, mhz } from "../lib/format";
  import { modeMeta } from "../lib/modes";
  import { app } from "../lib/store.svelte";
  import type { MessageItem } from "../lib/types";
  import Icon from "./Icon.svelte";

  let { compact = false }: { compact?: boolean } = $props();

  const ROW = 30;
  const OVERSCAN = 8;

  let query = $state("");
  let hideBad = $state(false);
  let scroller = $state<HTMLDivElement>();
  let scrollTop = $state(0);
  let height = $state(400);
  /** While the reader is looking at something, the list holds still. */
  let frozen = $state.raw<MessageItem[] | null>(null);

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const src = app.messages;
    if (!q && !hideBad) return src;
    return src.filter(
      (m) => (!hideBad || m.crcOk) && (!q || m.summary.toLowerCase().includes(q) || mhz(m.freq).includes(q)),
    );
  });
  const shown = $derived(frozen ?? filtered);
  /** Matching messages that arrived after the list froze. */
  const pending = $derived.by(() => {
    if (!frozen) return 0;
    const lastSeen = frozen.length ? frozen[frozen.length - 1].id : -1;
    // Ids only grow, so the newer ones are a run at the end.
    let n = 0;
    for (let i = filtered.length - 1; i >= 0 && filtered[i].id > lastSeen; i--) n++;
    return n;
  });

  // Newest first: row i shows shown[len - 1 - i].
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN));
  const last = $derived(Math.min(shown.length, Math.ceil((scrollTop + height) / ROW) + OVERSCAN));
  const rows = $derived.by(() => {
    const out: { i: number; m: MessageItem }[] = [];
    for (let i = first; i < last; i++) out.push({ i, m: shown[shown.length - 1 - i] });
    return out;
  });

  function onScroll() {
    if (!scroller) return;
    scrollTop = scroller.scrollTop;
    if (scrollTop > ROW && !frozen) frozen = filtered;
    else if (scrollTop <= 1 && frozen && app.selectedId == null) frozen = null;
  }

  function select(m: MessageItem) {
    app.select(app.selectedId === m.id ? null : m);
    if (app.selectedId != null && !frozen) frozen = filtered;
  }

  function resume() {
    frozen = null;
    scroller?.scrollTo({ top: 0 });
  }

  function onKey(e: KeyboardEvent) {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    if (!shown.length) return;
    const cur = shown.findIndex((m) => m.id === app.selectedId);
    // Down the screen is older, which is a lower index.
    let next = cur < 0 ? shown.length - 1 : cur + (e.key === "ArrowDown" ? -1 : 1);
    next = Math.max(0, Math.min(shown.length - 1, next));
    app.select(shown[next]);
    if (!frozen) frozen = filtered;
    const y = (shown.length - 1 - next) * ROW;
    if (scroller) {
      if (y < scroller.scrollTop) scroller.scrollTop = y;
      else if (y + ROW > scroller.scrollTop + height) scroller.scrollTop = y + ROW - height;
    }
  }

  // Filters changing while frozen should show the new filter at once.
  $effect(() => {
    void query;
    void hideBad;
    frozen = null;
  });
</script>

<div class="wrap" class:compact>
  <div class="tools">
    <label class="search">
      <Icon name="search" size={14} />
      <span class="sr-only">Filter messages</span>
      <input bind:value={query} placeholder="Filter by flight, tail, text or frequency" spellcheck="false" />
    </label>
    <label class="check"><input type="checkbox" bind:checked={hideBad} /><span>Valid only</span></label>
  </div>

  <div class="listbox">
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      class="scroll"
      bind:this={scroller}
      bind:clientHeight={height}
      onscroll={onScroll}
      onkeydown={onKey}
      tabindex="0"
      role="listbox"
      aria-label="Decoded messages"
    >
      <div class="spacer" style:height="{shown.length * ROW}px">
        {#each rows as { i, m } (m.id)}
          {@const meta = modeMeta(m.mode)}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div
            class="row"
            class:sel={m.id === app.selectedId}
            class:bad={!m.crcOk}
            style:top="{i * ROW}px"
            role="option"
            tabindex="-1"
            aria-selected={m.id === app.selectedId}
            onclick={() => select(m)}
          >
            <span class="t mono">{clock(m.ts)}</span>
            <span class="chip" style:--c="var(--{meta.family})" title={meta.name}>{#if !compact}{meta.name}{/if}</span>
            <span class="f mono">{mhz(m.freq)}</span>
            <span class="s mono">{m.summary}</span>
            {#if !compact}<span class="r mono">{m.rssi != null ? m.rssi.toFixed(1) : ""}</span>{/if}
          </div>
        {/each}
      </div>
    </div>
    {#if !shown.length}
      <div class="none">
        {#if app.messages.length}No messages match this filter.{:else if app.phase === "running"}Listening. Messages appear here as they are decoded.{:else}No messages were decoded.{/if}
      </div>
    {/if}
    {#if frozen && pending > 0}
      <button class="btn sm primary newer" onclick={resume}>{pending.toLocaleString()} newer · show latest</button>
    {/if}
  </div>
</div>

<style>
  .wrap {
    display: grid;
    grid-template-rows: auto 1fr;
    height: 100%;
    min-height: 0;
    min-width: 0;
  }
  .tools {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  .search {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 30px;
    padding: 0 10px;
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    background: var(--bg);
    color: var(--faint);
    min-width: 0;
  }
  .search:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: none;
    color: var(--ink);
    font: 400 13px/1 var(--sans);
  }
  .search input::placeholder {
    color: var(--faint);
  }
  .tools .check {
    align-items: center;
    white-space: nowrap;
    color: var(--muted);
  }
  .tools .check input {
    margin: 0;
  }
  .listbox {
    position: relative;
    min-height: 0;
  }
  .scroll {
    height: 100%;
    overflow-y: auto;
    outline-offset: -2px;
  }
  .spacer {
    position: relative;
  }
  .row {
    position: absolute;
    left: 0;
    right: 0;
    height: 30px;
    display: grid;
    grid-template-columns: 62px 104px 68px 1fr 44px;
    align-items: center;
    gap: 10px;
    padding: 0 12px;
    font-size: 12px;
    border-bottom: 1px solid color-mix(in srgb, var(--line) 55%, transparent);
    cursor: pointer;
  }
  .compact .row {
    grid-template-columns: 58px 6px 60px 1fr;
  }
  .row:hover {
    background: var(--surface-2);
  }
  .row.sel {
    background: var(--accent-soft);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .row.bad .s {
    color: var(--faint);
  }
  .t,
  .f {
    color: var(--muted);
  }
  .f,
  .r {
    text-align: right;
  }
  .r {
    color: var(--faint);
  }
  .s {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    font-weight: 500;
    color: var(--muted);
    overflow: hidden;
    white-space: nowrap;
  }
  .chip::before {
    content: "";
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--c);
  }
  .none {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    padding: 24px;
    text-align: center;
    color: var(--faint);
    pointer-events: none;
  }
  .newer {
    position: absolute;
    top: 10px;
    left: 50%;
    translate: -50% 0;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.35);
  }
</style>
