<script lang="ts">
  import { backend } from "../lib/backend";
  import { mhz, rate } from "../lib/format";
  import { FAMILIES, MODES } from "../lib/modes";
  import { advancedCount, app, saveSettings, settings } from "../lib/store.svelte";
  import type { ResolvedTune } from "../lib/types";
  import Advanced from "./Advanced.svelte";
  import Icon from "./Icon.svelte";

  let showAdvanced = $state(false);
  let plan = $state<ResolvedTune | null>(null);
  let planError = $state<string | null>(null);

  const available = $derived(new Set(app.info?.modes.map((m) => m.id) ?? MODES.map((m) => m.id)));
  const isFile = $derived(settings.sourceKind === "file");
  const ready = $derived(
    isFile ? settings.filePath !== "" && plan !== null : app.devices.length > 0 && plan !== null,
  );
  const changed = $derived(advancedCount());

  // Preview what Listen will tune to, a beat after the form settles.
  $effect(() => {
    void JSON.stringify($state.snapshot(settings));
    const t = setTimeout(async () => {
      saveSettings();
      if (isFile && !settings.filePath) {
        plan = null;
        planError = null;
        return;
      }
      try {
        plan = await app.resolve();
        planError = null;
      } catch (e) {
        plan = null;
        planError = String(e);
      }
    }, 250);
    return () => clearTimeout(t);
  });

  async function chooseFile() {
    const b = await backend();
    const p = await b.pickFile("Open an IQ recording", [
      { name: "IQ recordings", extensions: ["cf32", "cs16", "cs8", "cu8", "iq", "raw", "bin", "wav"] },
    ]);
    if (p) settings.filePath = p;
  }

  function fileName(p: string) {
    return p.split(/[\\/]/).pop() ?? p;
  }
</script>

<div class="setup">
  <div class="col">
    <header>
      <h1>What do you want to hear?</h1>
      <p>Pick a radio and a signal. xng tunes itself; every setting is under Advanced if you need it.</p>
    </header>

    <section aria-labelledby="src-h">
      <div class="head">
        <h2 id="src-h">Source</h2>
        <div class="seg" role="tablist" aria-label="Source type">
          <button role="tab" aria-selected={!isFile} class:on={!isFile} onclick={() => (settings.sourceKind = "device")}>
            <Icon name="radio" size={15} /> Radio
          </button>
          <button role="tab" aria-selected={isFile} class:on={isFile} onclick={() => (settings.sourceKind = "file")}>
            <Icon name="file" size={15} /> Recording
          </button>
        </div>
      </div>

      {#if !isFile}
        <div class="devices">
          {#each app.devices as d (d.selector)}
            <label class="device" class:on={settings.device === d.selector}>
              <input type="radio" name="device" value={d.selector} bind:group={settings.device} class="sr-only" />
              <span class="dot"></span>
              <span class="dname">{d.label}</span>
              <span class="dmeta mono">{d.backend}</span>
            </label>
          {:else}
            <div class="empty">
              {#if app.devicesLoading}
                Looking for radios…
              {:else if app.devicesError}
                <strong>Couldn't list radios.</strong> <span class="selectable">{app.devicesError}</span>
              {:else}
                <strong>No radio found.</strong> Plug in an SDR and refresh, or open a recording instead.
                {#if app.info?.os === "windows"}SoapySDR devices need the PothosSDR drivers installed.{/if}
              {/if}
            </div>
          {/each}
          <button class="btn sm quiet" onclick={() => app.refreshDevices()} disabled={app.devicesLoading}>
            <Icon name="refresh" size={14} /> Refresh radios
          </button>
        </div>
      {:else}
        <div class="filebox">
          <button class="btn" onclick={chooseFile}><Icon name="file" size={15} /> Choose recording</button>
          {#if settings.filePath}
            <span class="fname mono selectable" title={settings.filePath}>{fileName(settings.filePath)}</span>
          {:else}
            <span class="hint">cf32, cs16, cs8 or cu8 samples</span>
          {/if}
        </div>
        <div class="row3">
          <label class="field">
            <span>Sample rate (Hz)</span>
            <input class="input" bind:value={settings.sampleRate} placeholder="2400000" inputmode="numeric" />
          </label>
          <label class="field">
            <span>Center frequency</span>
            <input class="input" bind:value={settings.centerFreq} placeholder="131.500M" />
          </label>
          <label class="field">
            <span>Sample format</span>
            <select class="input" bind:value={settings.fileFormat}>
              <option value="">From file name</option>
              <option value="cf32">cf32</option>
              <option value="cs16">cs16</option>
              <option value="cs8">cs8</option>
              <option value="cu8">cu8</option>
            </select>
          </label>
        </div>
      {/if}
    </section>

    <section aria-labelledby="mode-h">
      <div class="head"><h2 id="mode-h">Signal</h2></div>
      {#each FAMILIES as fam (fam.id)}
        {@const modes = MODES.filter((m) => m.family === fam.id && available.has(m.id))}
        {#if modes.length}
          <h3 style:--c="var(--{fam.id})">{fam.name}</h3>
          <div class="modes">
            {#each modes as m (m.id)}
              <label class="mode" class:on={settings.mode === m.id} style:--c="var(--{fam.id})">
                <input type="radio" name="mode" value={m.id} bind:group={settings.mode} class="sr-only" />
                <span class="mname">{m.name}</span>
                <span class="mwhat">{m.what}</span>
              </label>
            {/each}
          </div>
        {/if}
      {/each}
    </section>

    <section class="adv" aria-labelledby="adv-h">
      <button class="advtoggle" aria-expanded={showAdvanced} onclick={() => (showAdvanced = !showAdvanced)}>
        <span class="chev" class:open={showAdvanced}><Icon name="chevron" size={15} /></span>
        <h2 id="adv-h">Advanced</h2>
        <span class="advnote">
          {changed ? `${changed} setting${changed === 1 ? "" : "s"} changed` : "Tuning, filters, outputs and feeds"}
        </span>
      </button>
      {#if showAdvanced}<Advanced />{/if}
    </section>
  </div>

  <footer>
    <div class="plan" aria-live="polite">
      {#if app.error}
        <span class="err selectable"><Icon name="alert" size={15} /> {app.error}</span>
      {:else if planError}
        <span class="err selectable"><Icon name="alert" size={15} /> {planError}</span>
      {:else if plan}
        <span class="mono">
          {mhz(plan.centerHz)} MHz · {rate(plan.sampleRate)} · {plan.channelsHz.length}
          channel{plan.channelsHz.length === 1 ? "" : "s"}
        </span>
        <span class="chans mono">{plan.channelsHz.slice(0, 8).map((c) => mhz(c)).join("  ")}{plan.channelsHz.length > 8 ? "  …" : ""}</span>
      {:else if isFile}
        <span class="hint">Choose a recording to continue.</span>
      {/if}
    </div>
    <button class="btn primary lg" disabled={!ready || app.phase === "starting"} onclick={() => app.start()}>
      <Icon name="play" size={16} />
      {app.phase === "starting" ? "Starting…" : isFile ? "Decode recording" : "Start listening"}
    </button>
  </footer>
</div>

<style>
  .setup {
    display: grid;
    grid-template-rows: 1fr auto;
    height: 100%;
    min-height: 0;
  }
  .col {
    overflow-y: auto;
    padding: 36px 32px 28px;
    display: grid;
    gap: 28px;
    align-content: start;
    justify-items: center;
  }
  .col > * {
    width: min(100%, 880px);
  }
  header h1 {
    font-size: 28px;
    font-weight: 600;
    letter-spacing: -0.015em;
    line-height: 1.15;
  }
  header p {
    margin-top: 8px;
    color: var(--muted);
    font-size: 15px;
    max-width: 60ch;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;
  }
  h2 {
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  h3 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 14px 0 8px;
    font-size: 13px;
    font-weight: 500;
    color: var(--ink);
  }
  h3::before {
    content: "";
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--c);
  }
  h3:first-of-type {
    margin-top: 0;
  }

  .seg {
    display: inline-flex;
    padding: 2px;
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    background: var(--surface);
  }
  .seg button {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 28px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--muted);
    font: 500 13px/1 var(--sans);
    cursor: pointer;
    transition: background 120ms var(--ease), color 120ms var(--ease);
  }
  .seg button:hover {
    color: var(--ink);
  }
  .seg button.on {
    background: var(--surface-3);
    color: var(--ink);
  }

  .devices {
    display: grid;
    gap: 8px;
    justify-items: start;
  }
  .device {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 12px;
    width: 100%;
    padding: 12px 14px;
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--surface);
    cursor: pointer;
    transition: border-color 120ms var(--ease), background 120ms var(--ease), transform 120ms var(--ease);
  }
  .device:hover {
    border-color: var(--line-strong);
    transform: translateY(-1px);
  }
  .device:active {
    transform: translateY(1px);
  }
  .device.on {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .device:has(:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .dot {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 1.5px solid var(--faint);
  }
  .device.on .dot {
    border: 4.5px solid var(--accent);
  }
  .dname {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dmeta {
    font-size: 12px;
    color: var(--faint);
  }
  .empty {
    width: 100%;
    padding: 16px;
    border: 1px dashed var(--line-strong);
    border-radius: var(--r-lg);
    color: var(--muted);
  }
  .empty strong {
    color: var(--ink);
    font-weight: 500;
  }

  .filebox {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-bottom: 14px;
    min-width: 0;
  }
  .fname {
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hint {
    color: var(--faint);
    font-size: 13px;
  }
  .row3 {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
  }

  .modes {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(196px, 1fr));
    gap: 8px;
  }
  .mode {
    display: grid;
    gap: 2px;
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--surface);
    cursor: pointer;
    transition: border-color 120ms var(--ease), background 120ms var(--ease), transform 120ms var(--ease);
  }
  .mode:hover {
    border-color: var(--line-strong);
    transform: translateY(-1px);
  }
  .mode:active {
    transform: translateY(1px);
  }
  .mode.on {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .mode:has(:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .mname {
    font-weight: 600;
    font-size: 14px;
  }
  .mwhat {
    font-size: 12px;
    color: var(--muted);
    line-height: 1.35;
  }

  .adv {
    border-top: 1px solid var(--line);
    padding-top: 18px;
  }
  .advtoggle {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 6px 0;
    border: 0;
    background: none;
    color: inherit;
    cursor: pointer;
    text-align: left;
  }
  .advtoggle:hover h2 {
    color: var(--ink);
  }
  .chev {
    display: inline-flex;
    color: var(--muted);
    transition: transform 160ms var(--ease);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .advnote {
    margin-left: auto;
    font-size: 13px;
    color: var(--faint);
  }

  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 14px 32px;
    border-top: 1px solid var(--line);
    background: var(--surface);
  }
  .plan {
    display: grid;
    gap: 2px;
    min-width: 0;
    font-size: 13px;
  }
  .chans {
    color: var(--faint);
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .err {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    color: var(--bad);
  }
</style>
