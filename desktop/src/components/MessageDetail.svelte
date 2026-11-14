<script lang="ts">
  import { clock, mhz } from "../lib/format";
  import { modeMeta } from "../lib/modes";
  import { app } from "../lib/store.svelte";
  import Icon from "./Icon.svelte";

  const m = $derived(app.selected);
  const json = $derived(m ? JSON.stringify(m.detail, null, 2) : "");
  let copied = $state(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(json);
      copied = true;
      setTimeout(() => (copied = false), 1400);
    } catch {
      // Clipboard refused: the text is still selectable.
    }
  }
</script>

{#if m}
  {@const meta = modeMeta(m.mode)}
  <aside class="detail" aria-label="Message detail">
    <header>
      <div>
        <h2>{meta.name} <span class="mono">{mhz(m.freq)} MHz</span></h2>
        <p class="mono">
          {clock(m.ts)} UTC ·
          <span class:bad={!m.crcOk}>{m.crcOk ? "integrity check passed" : "integrity check failed"}</span>
          {#if m.rssi != null}· {m.rssi.toFixed(1)} dBFS{/if}
          {#if m.snr != null}· SNR {m.snr.toFixed(1)} dB{/if}
        </p>
      </div>
      <button class="btn sm quiet" onclick={() => app.select(null)} aria-label="Close detail">
        <Icon name="close" size={15} />
      </button>
    </header>
    <p class="summary mono selectable">{m.summary}</p>
    <div class="jsonhead">
      <span>Decoded message</span>
      <button class="btn sm" onclick={copy}><Icon name="copy" size={13} /> {copied ? "Copied" : "Copy JSON"}</button>
    </div>
    <pre class="mono selectable">{json}</pre>
  </aside>
{/if}

<style>
  .detail {
    display: grid;
    grid-template-rows: auto auto auto 1fr;
    height: 100%;
    min-height: 0;
    min-width: 0;
    background: var(--surface);
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    padding: 14px 14px 10px 16px;
  }
  h2 {
    font-size: 15px;
    font-weight: 600;
  }
  h2 span {
    font-weight: 400;
    color: var(--muted);
    margin-left: 6px;
    font-size: 13px;
  }
  header p {
    margin-top: 3px;
    font-size: 12px;
    color: var(--muted);
  }
  .bad {
    color: var(--bad);
  }
  .summary {
    margin: 0 16px 12px;
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--bg);
    font-size: 12.5px;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .jsonhead {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0 16px 8px;
    font-size: 12px;
    font-weight: 500;
    color: var(--muted);
  }
  pre {
    margin: 0;
    padding: 0 16px 16px;
    overflow: auto;
    font-size: 12px;
    line-height: 1.5;
    color: var(--muted);
  }
</style>
