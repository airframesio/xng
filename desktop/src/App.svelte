<script lang="ts">
  import { onMount } from "svelte";
  import Channels from "./components/Channels.svelte";
  import Icon from "./components/Icon.svelte";
  import MapView from "./components/MapView.svelte";
  import MessageDetail from "./components/MessageDetail.svelte";
  import MessageList from "./components/MessageList.svelte";
  import Setup from "./components/Setup.svelte";
  import Spectrum from "./components/Spectrum.svelte";
  import TopBar from "./components/TopBar.svelte";
  import { app } from "./lib/store.svelte";

  let bootError = $state<string | null>(null);
  const active = $derived(app.phase === "running" || app.phase === "ended");

  onMount(() => {
    app.init().catch((e) => (bootError = String(e)));
  });
</script>

<div class="shell">
  <TopBar />

  {#if !app.native}
    <div class="banner">Browser preview with simulated data. Nothing here was received by a radio.</div>
  {/if}
  {#if active && app.error}
    <div class="banner bad selectable"><Icon name="alert" size={14} /> {app.error}</div>
  {/if}
  {#if active && app.dropped > 0}
    <div class="banner">
      {app.dropped.toLocaleString()} messages arrived faster than the list can show and were left out of it. Outputs, feeds and the map still received every one.
    </div>
  {/if}

  <main>
    {#if bootError}
      <div class="fatal selectable"><h1>xng couldn't start</h1><p>{bootError}</p></div>
    {:else if !active}
      <Setup />
    {:else if app.view === "live"}
      <div class="live" class:detail={app.selected}>
        <section class="mapcol">
          <MapView />
          <div class="strip"><Spectrum waterfall={false} /></div>
        </section>
        <section class="side">
          {#if app.selected}
            <MessageDetail />
          {:else}
            <MessageList compact />
          {/if}
        </section>
      </div>
    {:else if app.view === "messages"}
      <div class="msgs" class:detail={app.selected}>
        <MessageList />
        {#if app.selected}<div class="pane"><MessageDetail /></div>{/if}
      </div>
    {:else}
      <div class="signal">
        <Spectrum />
        <div class="pane"><Channels /></div>
      </div>
    {/if}
  </main>
</div>

<style>
  .shell {
    display: grid;
    grid-template-rows: auto;
    grid-auto-rows: auto;
    height: 100%;
    grid-template-rows: auto repeat(3, auto) minmax(0, 1fr);
  }
  main {
    grid-row: -2 / -1;
    min-height: 0;
  }
  .banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 16px;
    border-bottom: 1px solid var(--line);
    background: var(--accent-soft);
    color: var(--ink);
    font-size: 12.5px;
  }
  .banner.bad {
    background: var(--bad-soft);
    color: var(--bad);
  }
  .live {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(380px, 38%);
    height: 100%;
  }
  .mapcol {
    display: grid;
    grid-template-rows: minmax(0, 1fr) 132px;
    min-height: 0;
    min-width: 0;
  }
  .strip {
    border-top: 1px solid var(--line);
    min-height: 0;
  }
  .side {
    border-left: 1px solid var(--line);
    min-height: 0;
    min-width: 0;
    background: var(--surface);
  }
  .msgs {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    height: 100%;
    background: var(--surface);
  }
  .msgs.detail {
    grid-template-columns: minmax(0, 1fr) minmax(360px, 36%);
  }
  .signal {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(340px, 30%);
    height: 100%;
  }
  .pane {
    border-left: 1px solid var(--line);
    min-height: 0;
    min-width: 0;
    background: var(--surface);
  }
  .fatal {
    display: grid;
    place-content: center;
    gap: 10px;
    height: 100%;
    text-align: center;
    color: var(--muted);
  }
  .fatal h1 {
    color: var(--ink);
    font-size: 22px;
  }
</style>
