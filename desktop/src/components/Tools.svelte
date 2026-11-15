<script lang="ts">
  import { backend } from "../lib/backend";
  import { mhz, num, list, rate } from "../lib/format";
  import { FAMILIES, MODES, modeMeta } from "../lib/modes";
  import { app, applyProposal, settings, type ToolKind } from "../lib/store.svelte";
  import type { ProposedConfig, ScanReport, SurveyReport, ToolRequest } from "../lib/types";
  import Icon from "./Icon.svelte";

  const TITLES: Record<ToolKind, { name: string; lead: string }> = {
    scan: {
      name: "Scan for signals",
      lead: "Steps the radio across each signal's known channels, listens briefly, and tells you which ones are active here.",
    },
    survey: {
      name: "Survey reception",
      lead: "Listens to one signal for a sustained period and reports how well each channel is received, with advice on gain.",
    },
  };

  // Only signals with a built-in channel plan can be scanned or surveyed.
  const planned = $derived(new Set((app.info?.modes ?? []).filter((m) => m.planChannels.length).map((m) => m.id)));
  const choices = $derived(MODES.filter((m) => planned.has(m.id)));

  let scanModes = $state<string[]>(["acars", "vdl2", "ais"]);
  let dwell = $state("20");
  let surveyMode = $state(settings.mode);
  let duration = $state("900");
  let scanFirst = $state(false);
  let tuneGain = $state(false);
  let showMessages = $state(false);
  let showTiming = $state(false);
  let interim = $state("300");
  let rotateDwell = $state("60");
  let scanDwell = $state("90");
  let tuneDwell = $state("20");
  let saved = $state<string | null>(null);
  let showLog = $state(false);
  let logEl = $state<HTMLElement | null>(null);

  const kind = $derived(app.tool ?? "scan");
  const busy = $derived(app.toolPhase === "running" || app.toolPhase === "stopping");
  const device = $derived(app.devices.find((d) => d.selector === settings.device) ?? null);
  const gain = $derived(num(settings.gain));
  const scanReport = $derived(app.toolReportKind === "scan" ? (app.toolReport as ScanReport | null) : null);
  const surveyReport = $derived(app.toolReportKind === "survey" ? (app.toolReport as SurveyReport | null) : null);
  const heard = $derived(scanReport?.results.filter((r) => r.verdict !== "quiet") ?? []);
  const quiet = $derived((scanReport?.results.length ?? 0) - heard.length);
  const proposals = $derived<ProposedConfig[]>(scanReport?.configs ?? (surveyReport?.config ? [surveyReport.config] : []));

  $effect(() => {
    // A survey mode that has no plan (or is gone) falls back to the first.
    if (choices.length && !planned.has(surveyMode)) surveyMode = choices[0].id;
  });

  // Keep the newest progress line in view.
  $effect(() => {
    void app.toolLines;
    if (logEl) logEl.scrollTop = logEl.scrollHeight;
  });

  function seconds(s: string, fallback: number): number {
    const n = num(s);
    return n && n >= 1 ? Math.round(n) : fallback;
  }

  function toggleMode(id: string) {
    scanModes = scanModes.includes(id) ? scanModes.filter((m) => m !== id) : [...scanModes, id];
  }

  function request(): ToolRequest {
    if (kind === "scan") {
      return { kind: "scan", sdr: settings.device, gain, modes: scanModes, dwellSecs: seconds(dwell, 20) };
    }
    return {
      kind: "survey",
      sdr: settings.device,
      gain,
      mode: surveyMode,
      // Tuning typed under Advanced applies when it is for this signal.
      sampleRate: surveyMode === settings.mode ? num(settings.sampleRate) : null,
      channels: surveyMode === settings.mode ? list(settings.channels) : [],
      durationSecs: seconds(duration, 900),
      interimSecs: seconds(interim, 300),
      scanFirst,
      scanDwellSecs: seconds(scanDwell, 90),
      tuneGain,
      tuneDwellSecs: seconds(tuneDwell, 20),
      rotateDwellSecs: seconds(rotateDwell, 60),
      showMessages,
    };
  }

  function start() {
    saved = null;
    showLog = false;
    app.startTool(request());
  }

  function use(c: ProposedConfig) {
    applyProposal(c);
    app.closeTool();
  }

  async function save() {
    if (!app.toolReport || !app.toolReportKind) return;
    try {
      const b = await backend();
      const path = await b.pickSave("Save the report", `xng-${app.toolReportKind}-report.json`);
      if (!path) return;
      await b.writeText(path, JSON.stringify(app.toolReport, null, 2));
      saved = "Report saved";
    } catch (e) {
      saved = String(e);
    }
  }

  function minutes(secs: number): string {
    return secs >= 90 ? `${Math.round(secs / 60)} min` : `${Math.round(secs)} s`;
  }
</script>

<div class="tools">
  <div class="col">
    <header>
      <button class="btn sm quiet" onclick={() => app.closeTool()}>
        <Icon name="back" size={14} /> Back to setup
      </button>
      <div class="titlerow">
        <h1>{TITLES[kind].name}</h1>
        <div class="seg" role="tablist" aria-label="Tool">
          {#each ["scan", "survey"] as const as k (k)}
            <button role="tab" aria-selected={kind === k} class:on={kind === k} disabled={busy} onclick={() => app.openTool(k)}>
              {k === "scan" ? "Scan" : "Survey"}
            </button>
          {/each}
        </div>
      </div>
      <p>{TITLES[kind].lead}</p>
    </header>

    {#if app.toolPhase === "idle" || app.toolPhase === "failed"}
      {#if app.toolError}
        <div class="note bad selectable"><Icon name="alert" size={15} /> {app.toolError}</div>
      {/if}

      <section aria-labelledby="radio-h">
        <h2 id="radio-h">Radio</h2>
        {#if device}
          <div class="radio">
            <span class="dname">{device.label}</span>
            <span class="mono dim">{gain == null ? "hardware AGC" : `${gain} dB gain`}</span>
          </div>
        {:else}
          <div class="note">No radio found. Plug in an SDR, go back, and refresh the list.</div>
        {/if}
      </section>

      {#if kind === "scan"}
        <section aria-labelledby="what-h">
          <h2 id="what-h">Signals to look for</h2>
          {#each FAMILIES as fam (fam.id)}
            {@const modes = choices.filter((m) => m.family === fam.id)}
            {#if modes.length}
              <div class="chips" style:--c="var(--{fam.id})">
                {#each modes as m (m.id)}
                  <label class="chip" class:on={scanModes.includes(m.id)} title={m.what}>
                    <input type="checkbox" class="sr-only" checked={scanModes.includes(m.id)} onchange={() => toggleMode(m.id)} />
                    {m.name}
                  </label>
                {/each}
              </div>
            {/if}
          {/each}
        </section>
        <section class="row" aria-label="Scan options">
          <label class="field">
            <span>Listen on each group of channels for</span>
            <select class="input" bind:value={dwell}>
              <option value="10">10 seconds</option>
              <option value="20">20 seconds</option>
              <option value="45">45 seconds</option>
              <option value="90">90 seconds</option>
              <option value="300">5 minutes</option>
            </select>
            <small>Longer catches quiet channels. Bursty signals such as ACARS can need a minute or more.</small>
          </label>
        </section>
      {:else}
        <section class="row" aria-label="Survey options">
          <label class="field">
            <span>Signal</span>
            <select class="input" bind:value={surveyMode}>
              {#each choices as m (m.id)}<option value={m.id}>{m.name}</option>{/each}
            </select>
            <small>{modeMeta(surveyMode).what}</small>
          </label>
          <label class="field">
            <span>Run for</span>
            <select class="input" bind:value={duration}>
              <option value="300">5 minutes</option>
              <option value="900">15 minutes</option>
              <option value="1800">30 minutes</option>
              <option value="3600">1 hour</option>
              <option value="14400">4 hours</option>
            </select>
            <small>You can stop early and still get the report.</small>
          </label>
        </section>
        <section class="checks" aria-label="Survey steps">
          <label class="check">
            <input type="checkbox" bind:checked={scanFirst} />
            <span><b>Scan first</b> and survey only the channels that show life, plus the signal's core channels</span>
          </label>
          <label class="check">
            <input type="checkbox" bind:checked={tuneGain} />
            <span><b>Find the best gain</b> by trying several settings before the survey</span>
          </label>
          <label class="check">
            <input type="checkbox" bind:checked={showMessages} />
            <span><b>Show decoded messages</b> in the progress log</span>
          </label>
        </section>
        <section class="adv">
          <button class="advtoggle" aria-expanded={showTiming} onclick={() => (showTiming = !showTiming)}>
            <span class="chev" class:open={showTiming}><Icon name="chevron" size={15} /></span>
            <h2>Timing</h2>
            <span class="advnote">Seconds per step</span>
          </button>
          {#if showTiming}
            <div class="row four">
              <label class="field"><span>Progress table every</span><input class="input" bind:value={interim} inputmode="numeric" /></label>
              <label class="field"><span>Per visit when rotating</span><input class="input" bind:value={rotateDwell} inputmode="numeric" /></label>
              <label class="field"><span>Scan-first dwell</span><input class="input" bind:value={scanDwell} inputmode="numeric" /></label>
              <label class="field"><span>Per gain step</span><input class="input" bind:value={tuneDwell} inputmode="numeric" /></label>
            </div>
          {/if}
        </section>
        {#if surveyMode === settings.mode && (settings.channels.trim() || settings.sampleRate.trim())}
          <div class="note">Using the channels and sample rate you set under Advanced for {modeMeta(surveyMode).name}.</div>
        {/if}
      {/if}
    {:else}
      {#if busy}
        <div class="progress" aria-live="polite">
          <span class="pulse"></span>
          {app.toolPhase === "stopping" ? "Finishing up and writing the report…" : kind === "scan" ? "Scanning…" : "Surveying…"}
        </div>
      {/if}

      {#if app.toolPhase === "done"}
        {#if app.toolStopped}
          <div class="note">Stopped early. This report covers what was measured up to that point.</div>
        {/if}

        {#if proposals.length}
          <section aria-labelledby="prop-h">
            <h2 id="prop-h">Suggested setup</h2>
            <div class="props">
              {#each proposals as c (c.mode)}
                <div class="prop">
                  <div class="pmain">
                    <span class="pname">{modeMeta(c.mode).name}</span>
                    <span class="mono dim">
                      {mhz(c.center_hz)} MHz · {rate(c.sample_rate)} · {c.channels_hz.length}
                      channel{c.channels_hz.length === 1 ? "" : "s"}{c.gain_db != null ? ` · ${c.gain_db} dB` : ""}
                    </span>
                    <span class="mono chans">{c.channels_hz.map((f) => mhz(f)).join("  ")}</span>
                  </div>
                  <button class="btn primary" onclick={() => use(c)}>Use this setup</button>
                </div>
              {/each}
            </div>
          </section>
        {:else}
          <div class="note">
            Nothing decoded{kind === "scan" ? " on any channel" : ""}. Try a longer run, check the antenna and gain, or pick a different signal.
          </div>
        {/if}

        {#if scanReport}
          <section aria-labelledby="res-h">
            <h2 id="res-h">What was heard</h2>
            {#if heard.length}
              <table>
                <thead><tr><th>Signal</th><th class="n">MHz</th><th class="n">Frames</th><th class="n">Good</th><th class="n">Level dBFS</th><th>Verdict</th></tr></thead>
                <tbody>
                  {#each heard as r (r.mode + r.frequency_hz)}
                    <tr>
                      <td>{modeMeta(r.mode).name}</td>
                      <td class="n mono">{mhz(r.frequency_hz)}</td>
                      <td class="n mono">{r.frames}</td>
                      <td class="n mono">{r.crc_ok}</td>
                      <td class="n mono">{r.level_dbfs.toFixed(1)}</td>
                      <td><span class="verdict" class:active={r.verdict === "ACTIVE"}>{r.verdict}</span></td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
            <p class="dim small">{quiet} quiet channel{quiet === 1 ? "" : "s"} left out.</p>
          </section>
        {:else if surveyReport}
          <section aria-labelledby="res-h">
            <h2 id="res-h">Reception by channel</h2>
            <p class="meta">
              {modeMeta(surveyReport.mode).name} for {minutes(surveyReport.duration_secs)} at {rate(surveyReport.sample_rate)}, gain {surveyReport.gain}
            </p>
            <table>
              <thead>
                <tr><th class="n">MHz</th><th class="n">Listened</th><th class="n">Frames</th><th class="n">Good</th><th class="n">Good %</th><th class="n">Per min</th><th class="n">Level dBFS</th><th>Verdict</th></tr>
              </thead>
              <tbody>
                {#each surveyReport.channels as r (r.frequency_hz)}
                  <tr class:faded={r.verdict === "quiet"}>
                    <td class="n mono">{mhz(r.frequency_hz)}</td>
                    <td class="n mono">{minutes(r.listen_secs)}</td>
                    <td class="n mono">{r.frames}</td>
                    <td class="n mono">{r.crc_ok}</td>
                    <td class="n mono">{r.frames ? r.ok_pct.toFixed(0) : "–"}</td>
                    <td class="n mono">{r.frames_per_min.toFixed(1)}</td>
                    <td class="n mono">{r.level_dbfs.toFixed(1)}</td>
                    <td><span class="verdict" class:active={r.verdict === "ACTIVE"}>{r.verdict}</span></td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </section>
          {#if surveyReport.sweep?.length}
            <section aria-labelledby="sweep-h">
              <h2 id="sweep-h">Gain sweep</h2>
              <table class="narrow">
                <thead><tr><th class="n">Gain dB</th><th class="n">Frames</th><th class="n">Good</th><th class="n">Mean level dBFS</th></tr></thead>
                <tbody>
                  {#each surveyReport.sweep as r (r.gain_db)}
                    <tr><td class="n mono">{r.gain_db}</td><td class="n mono">{r.frames}</td><td class="n mono">{r.crc_ok}</td><td class="n mono">{r.mean_level_dbfs.toFixed(1)}</td></tr>
                  {/each}
                </tbody>
              </table>
            </section>
          {/if}
          {#if surveyReport.advice.length}
            <section aria-labelledby="adv-h">
              <h2 id="adv-h">Suggestions</h2>
              <ul class="advice selectable">
                {#each surveyReport.advice as a (a)}<li>{a}</li>{/each}
              </ul>
            </section>
          {/if}
        {/if}
      {/if}

      {#if busy || showLog}
        <pre class="log mono selectable" bind:this={logEl}>{app.toolLines.join("\n")}</pre>
      {/if}
    {/if}
  </div>

  <footer>
    <div class="status" aria-live="polite">
      {#if saved}<span class="dim">{saved}</span>{/if}
    </div>
    {#if busy}
      <button class="btn danger lg" disabled={app.toolPhase === "stopping"} onclick={() => app.stopTool()}>
        <Icon name="stop" size={16} />
        {app.toolPhase === "stopping" ? "Stopping…" : "Stop and report"}
      </button>
    {:else if app.toolPhase === "done"}
      <button class="btn quiet" onclick={() => (showLog = !showLog)}>{showLog ? "Hide log" : "Show log"}</button>
      <button class="btn" onclick={save}><Icon name="download" size={15} /> Save report</button>
      <button class="btn" onclick={() => (app.toolPhase = "idle")}>Run again</button>
    {:else}
      <button class="btn primary lg" disabled={!device || (kind === "scan" && scanModes.length === 0)} onclick={start}>
        <Icon name="play" size={16} />
        {kind === "scan" ? "Start scan" : "Start survey"}
      </button>
    {/if}
  </footer>
</div>

<style>
  .tools {
    display: grid;
    grid-template-rows: 1fr auto;
    height: 100%;
    min-height: 0;
  }
  .col {
    overflow-y: auto;
    padding: 24px 32px 28px;
    display: grid;
    gap: 24px;
    align-content: start;
    justify-items: center;
  }
  .col > * {
    width: min(100%, 880px);
  }
  header {
    display: grid;
    gap: 10px;
    justify-items: start;
  }
  .titlerow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    width: 100%;
    margin-top: 6px;
  }
  header h1 {
    font-size: 28px;
    font-weight: 600;
    letter-spacing: -0.015em;
    line-height: 1.15;
  }
  header p {
    color: var(--muted);
    font-size: 15px;
    max-width: 68ch;
  }
  h2 {
    margin-bottom: 12px;
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .dim {
    color: var(--faint);
  }
  .small {
    margin-top: 8px;
    font-size: 12.5px;
  }
  .meta {
    margin: -4px 0 12px;
    color: var(--muted);
    font-size: 13.5px;
  }

  .seg {
    display: inline-flex;
    padding: 2px;
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    background: var(--surface);
  }
  .seg button {
    height: 28px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--muted);
    font: 500 13px/1 var(--sans);
    cursor: pointer;
    transition: background 120ms var(--ease), color 120ms var(--ease);
  }
  .seg button:hover:not(:disabled) {
    color: var(--ink);
  }
  .seg button.on {
    background: var(--surface-3);
    color: var(--ink);
  }
  .seg button:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .radio {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 16px;
    padding: 12px 14px;
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--surface);
  }
  .dname {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .note {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 14px;
    border: 1px dashed var(--line-strong);
    border-radius: var(--r-lg);
    color: var(--muted);
    font-size: 13.5px;
  }
  .note.bad {
    border-style: solid;
    border-color: var(--bad);
    background: var(--bad-soft);
    color: var(--bad);
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 32px;
    padding: 0 12px;
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--surface);
    font-size: 13.5px;
    font-weight: 500;
    cursor: pointer;
    transition: border-color 120ms var(--ease), background 120ms var(--ease), transform 120ms var(--ease);
  }
  .chip::before {
    content: "";
    width: 8px;
    height: 8px;
    border-radius: 50%;
    border: 1.5px solid var(--c);
  }
  .chip:hover {
    border-color: var(--line-strong);
    transform: translateY(-1px);
  }
  .chip:active {
    transform: translateY(1px);
  }
  .chip.on {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .chip.on::before {
    background: var(--c);
  }
  .chip:has(:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .row {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 12px;
    align-items: start;
  }
  .row.four {
    grid-template-columns: repeat(4, minmax(0, 1fr));
    margin-top: 12px;
  }
  .checks {
    display: grid;
    gap: 10px;
  }
  .check {
    display: flex;
    align-items: baseline;
    gap: 10px;
    color: var(--muted);
    font-size: 14px;
    cursor: pointer;
  }
  .check input {
    accent-color: var(--accent);
    translate: 0 2px;
  }
  .check b {
    color: var(--ink);
    font-weight: 500;
  }

  .adv {
    border-top: 1px solid var(--line);
    padding-top: 14px;
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
  .advtoggle h2 {
    margin: 0;
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

  .progress {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 15px;
    font-weight: 500;
  }
  .pulse {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1.4s var(--ease) infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.25;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .pulse {
      animation: none;
    }
    .chip,
    .chev {
      transition: none;
    }
  }
  .log {
    max-height: 46vh;
    min-height: 160px;
    overflow: auto;
    margin: 0;
    padding: 12px 14px;
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--surface);
    color: var(--muted);
    font-size: 12.5px;
    line-height: 1.55;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .props {
    display: grid;
    gap: 8px;
  }
  .prop {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
    padding: 12px 14px;
    border: 1px solid var(--accent);
    border-radius: var(--r-lg);
    background: var(--accent-soft);
  }
  .pmain {
    display: grid;
    gap: 3px;
    min-width: 0;
    font-size: 13px;
  }
  .pname {
    font-size: 15px;
    font-weight: 600;
  }
  .pmain .dim {
    color: var(--muted);
  }
  .chans {
    color: var(--muted);
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  table.narrow {
    width: auto;
    min-width: 420px;
  }
  th {
    padding: 0 12px 8px 0;
    border-bottom: 1px solid var(--line-strong);
    color: var(--faint);
    font-size: 12px;
    font-weight: 500;
    text-align: left;
    white-space: nowrap;
  }
  td {
    padding: 7px 12px 7px 0;
    border-bottom: 1px solid var(--line);
  }
  .n {
    text-align: right;
  }
  tr.faded td {
    color: var(--faint);
  }
  .verdict {
    font-size: 12px;
    color: var(--muted);
  }
  .verdict.active {
    color: var(--ok);
    font-weight: 600;
  }
  .advice {
    display: grid;
    gap: 8px;
    padding-left: 18px;
    color: var(--muted);
    font-size: 14px;
    max-width: 78ch;
  }

  footer {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 14px 32px;
    border-top: 1px solid var(--line);
    background: var(--surface);
  }
  .status {
    flex: 1;
    min-width: 0;
    font-size: 13px;
  }
</style>
