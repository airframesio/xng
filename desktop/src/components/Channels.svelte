<script lang="ts">
  import { count, mhz } from "../lib/format";
  import { app } from "../lib/store.svelte";

  const rows = $derived(app.channels);
  const level = (db: number) => Math.max(0, Math.min(100, ((db + 70) / 60) * 100));
</script>

<div class="channels">
  <table>
    <thead>
      <tr>
        <th>Channel</th>
        <th class="n">Frames</th>
        <th class="n">Valid</th>
        <th>Level</th>
      </tr>
    </thead>
    <tbody>
      {#each rows as c (c.freq)}
        <tr>
          <td class="mono">{mhz(c.freq)} MHz</td>
          <td class="n mono">{count(c.frames)}</td>
          <td class="n mono">{c.frames ? `${Math.round((c.crcOk / c.frames) * 100)}%` : ""}</td>
          <td>
            <div class="meter" role="meter" aria-valuemin={-70} aria-valuemax={-10} aria-valuenow={Math.round(c.levelDb)} aria-label="Signal level">
              <i style:width="{level(c.levelDb)}%"></i>
            </div>
            <span class="mono db">{Number.isFinite(c.levelDb) ? c.levelDb.toFixed(0) : "–"} dB</span>
          </td>
        </tr>
      {:else}
        <tr><td colspan="4" class="none">Channel statistics appear once samples are flowing.</td></tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .channels {
    height: 100%;
    overflow: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12.5px;
  }
  th {
    position: sticky;
    top: 0;
    padding: 9px 14px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
    text-align: left;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  td {
    padding: 8px 14px;
    border-bottom: 1px solid color-mix(in srgb, var(--line) 55%, transparent);
    white-space: nowrap;
  }
  .n {
    text-align: right;
  }
  td:last-child {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .meter {
    flex: 1;
    min-width: 60px;
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .meter i {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 120ms linear;
  }
  .db {
    width: 52px;
    text-align: right;
    color: var(--muted);
  }
  .none {
    display: table-cell !important;
    color: var(--faint);
    text-align: center;
    padding: 28px;
  }
</style>
