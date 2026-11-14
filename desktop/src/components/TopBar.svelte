<script lang="ts">
  import { backend } from "../lib/backend";
  import { count, elapsed, mhz, rate } from "../lib/format";
  import { modeMeta } from "../lib/modes";
  import { app, type View } from "../lib/store.svelte";
  import Icon from "./Icon.svelte";

  const TABS: { id: View; name: string; icon: string }[] = [
    { id: "live", name: "Live", icon: "map" },
    { id: "messages", name: "Messages", icon: "list" },
    { id: "signal", name: "Signal", icon: "wave" },
  ];
  const s = $derived(app.session);
  const active = $derived(app.phase === "running" || app.phase === "ended");
  let exporting = $state(false);
  let exportNote = $state<string | null>(null);

  async function exportAs(format: string, ext: string) {
    exporting = false;
    try {
      const b = await backend();
      const path = await b.pickSave("Export what's on the map", `xng-export.${ext}`);
      if (!path) return;
      await b.exportEntities(format, path);
      exportNote = "Exported";
    } catch (e) {
      exportNote = String(e);
    }
    setTimeout(() => (exportNote = null), 3000);
  }
</script>

<header class="bar">
  <div class="brand">
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2.4" stroke-linecap="round" aria-hidden="true">
      <path d="M4 19v-6M8 19V9M12 19V5M16 19v-8M20 19v-4" />
    </svg>
    <span>xng</span>
  </div>

  {#if active && s}
    <nav aria-label="Views">
      {#each TABS as t (t.id)}
        <button class:on={app.view === t.id} aria-current={app.view === t.id ? "page" : undefined} onclick={() => (app.view = t.id)}>
          <Icon name={t.icon} size={15} />
          {t.name}
        </button>
      {/each}
    </nav>

    <div class="status">
      <span class="live" class:ended={app.phase === "ended"}>
        <i></i>{app.phase === "running" ? "Listening" : "Stopped"}
      </span>
      <span class="what">{modeMeta(s.mode).name}</span>
      <span class="mono dim">{mhz(s.centerHz)} MHz · {rate(s.sampleRate)}</span>
      <span class="mono dim">{elapsed(app.elapsedMs)}</span>
      <span class="mono"><b>{count(app.total)}</b> <span class="dim">msgs · {app.rate.toFixed(1)}/s</span></span>
    </div>

    <div class="actions">
      {#if exportNote}<span class="note">{exportNote}</span>{/if}
      <div class="menu">
        <button class="btn sm quiet" aria-haspopup="menu" aria-expanded={exporting} onclick={() => (exporting = !exporting)}>
          <Icon name="download" size={14} /> Export
        </button>
        {#if exporting}
          <div class="pop" role="menu">
            <button role="menuitem" onclick={() => exportAs("geojson", "geojson")}>GeoJSON</button>
            <button role="menuitem" onclick={() => exportAs("kml", "kml")}>KML</button>
            <button role="menuitem" onclick={() => exportAs("gpx", "gpx")}>GPX</button>
            <button role="menuitem" onclick={() => exportAs("aircraft", "json")}>aircraft.json</button>
          </div>
        {/if}
      </div>
      {#if app.phase === "running"}
        <button class="btn sm danger" onclick={() => app.stop()}><Icon name="stop" size={12} /> Stop</button>
      {:else}
        <button class="btn sm" onclick={() => app.reset()}><Icon name="back" size={14} /> New session</button>
      {/if}
    </div>
  {:else}
    <div class="spacer"></div>
    {#if app.info}<span class="mono dim ver">v{app.info.version}</span>{/if}
  {/if}
</header>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 18px;
    height: 48px;
    padding: 0 14px 0 16px;
    border-bottom: 1px solid var(--line);
    background: var(--surface);
    min-width: 0;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font: 600 16px/1 var(--mono);
    letter-spacing: -0.02em;
  }
  nav {
    display: flex;
    gap: 2px;
  }
  nav button {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 30px;
    padding: 0 11px;
    border: 0;
    border-radius: var(--r);
    background: none;
    color: var(--muted);
    font: 500 13px/1 var(--sans);
    cursor: pointer;
    transition: background 120ms var(--ease), color 120ms var(--ease);
  }
  nav button:hover {
    color: var(--ink);
    background: var(--surface-2);
  }
  nav button.on {
    color: var(--ink);
    background: var(--surface-3);
  }
  .status {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 16px;
    min-width: 0;
    font-size: 12.5px;
    white-space: nowrap;
    overflow: hidden;
  }
  .live {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    font-weight: 500;
    color: var(--ok);
  }
  .live i {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
    animation: pulse 2.4s ease-in-out infinite;
  }
  .live.ended {
    color: var(--muted);
  }
  .live.ended i {
    animation: none;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  .what {
    font-weight: 600;
  }
  .dim {
    color: var(--muted);
  }
  b {
    font-weight: 500;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .note {
    font-size: 12px;
    color: var(--muted);
    max-width: 240px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .menu {
    position: relative;
  }
  .pop {
    position: absolute;
    right: 0;
    top: 34px;
    z-index: 20;
    display: grid;
    min-width: 150px;
    padding: 4px;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: 0 10px 28px rgba(0, 0, 0, 0.35);
  }
  .pop button {
    height: 30px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--ink);
    font: 400 13px/1 var(--sans);
    text-align: left;
    cursor: pointer;
  }
  .pop button:hover {
    background: var(--surface-3);
  }
  .spacer {
    flex: 1;
  }
  .ver {
    font-size: 12px;
  }
  @media (max-width: 1180px) {
    .status .dim:nth-of-type(3) {
      display: none;
    }
  }
</style>
