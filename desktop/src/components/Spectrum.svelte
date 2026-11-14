<script lang="ts">
  import { onMount } from "svelte";
  import { mhz } from "../lib/format";
  import { app } from "../lib/store.svelte";

  let { waterfall = true }: { waterfall?: boolean } = $props();

  const BINS = 512;
  const FLOOR = -100;
  const CEIL = -10;

  let box: HTMLDivElement;
  let plot: HTMLCanvasElement;
  let fall = $state<HTMLCanvasElement>();
  let hover = $state<{ x: number; hz: number } | null>(null);

  const session = $derived(app.session);

  function css(name: string) {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  // Ground to accent to near-white: one hue, so the waterfall belongs to
  // the same system as the rest of the app.
  function palette(): Uint8ClampedArray {
    const parse = (c: string) => {
      const g = document.createElement("canvas").getContext("2d")!;
      g.fillStyle = c;
      g.fillRect(0, 0, 1, 1);
      return g.getImageData(0, 0, 1, 1).data;
    };
    const stops = [parse(css("--bg")), parse(css("--surface-3")), parse(css("--accent")), parse("#fff4d6")];
    const at = [0, 0.35, 0.78, 1];
    const lut = new Uint8ClampedArray(256 * 4);
    for (let i = 0; i < 256; i++) {
      const t = i / 255;
      let s = 0;
      while (s < at.length - 2 && t > at[s + 1]) s++;
      const u = (t - at[s]) / (at[s + 1] - at[s]);
      for (let c = 0; c < 3; c++) lut[i * 4 + c] = stops[s][c] + (stops[s + 1][c] - stops[s][c]) * u;
      lut[i * 4 + 3] = 255;
    }
    return lut;
  }

  onMount(() => {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const pg = plot.getContext("2d")!;
    const fg = fall?.getContext("2d") ?? null;
    const lut = palette();
    const row = new ImageData(BINS, 1);
    let smooth: Float32Array | null = null;
    let latest: number[] | null = null;
    let raf = 0;
    const colours = { line: css("--accent"), grid: css("--line"), text: css("--faint"), mark: css("--muted") };

    function size() {
      plot.width = Math.max(1, Math.round(plot.clientWidth * dpr));
      plot.height = Math.max(1, Math.round(plot.clientHeight * dpr));
      if (fall) {
        fall.width = BINS;
        fall.height = Math.max(1, Math.round(fall.clientHeight));
      }
    }

    function draw() {
      raf = 0;
      const bins = latest;
      if (!bins) return;
      latest = null;
      const n = Math.min(bins.length, BINS);
      if (!smooth) smooth = Float32Array.from(bins.slice(0, n));
      for (let i = 0; i < n; i++) smooth[i] += (bins[i] - smooth[i]) * 0.35;

      const w = plot.width;
      const h = plot.height;
      const y = (db: number) => h - ((Math.max(FLOOR, Math.min(CEIL, db)) - FLOOR) / (CEIL - FLOOR)) * h;
      pg.clearRect(0, 0, w, h);
      pg.lineWidth = 1;
      pg.strokeStyle = colours.grid;
      pg.fillStyle = colours.text;
      pg.font = `${10 * dpr}px ${css("--mono")}`;
      for (let db = -80; db <= -20; db += 20) {
        const yy = Math.round(y(db)) + 0.5;
        pg.beginPath();
        pg.moveTo(0, yy);
        pg.lineTo(w, yy);
        pg.stroke();
        pg.fillText(`${db}`, 4 * dpr, yy - 3 * dpr);
      }
      // Channel markers.
      const s = session;
      if (s) {
        pg.strokeStyle = colours.mark;
        pg.setLineDash([2 * dpr, 4 * dpr]);
        for (const c of s.channelsHz) {
          const x = Math.round(((c - s.centerHz) / s.sampleRate + 0.5) * w) + 0.5;
          pg.beginPath();
          pg.moveTo(x, 0);
          pg.lineTo(x, h);
          pg.stroke();
        }
        pg.setLineDash([]);
      }
      pg.beginPath();
      for (let i = 0; i < n; i++) {
        const x = (i / (n - 1)) * w;
        if (i === 0) pg.moveTo(x, y(smooth[i]));
        else pg.lineTo(x, y(smooth[i]));
      }
      pg.strokeStyle = colours.line;
      pg.lineWidth = 1.5 * dpr;
      pg.stroke();
      pg.lineTo(w, h);
      pg.lineTo(0, h);
      pg.closePath();
      pg.globalAlpha = 0.1;
      pg.fillStyle = colours.line;
      pg.fill();
      pg.globalAlpha = 1;

      if (fg && fall) {
        fg.drawImage(fall, 0, 2);
        for (let i = 0; i < n; i++) {
          const v = Math.round(((Math.max(FLOOR, Math.min(CEIL, bins[i])) - FLOOR) / (CEIL - FLOOR)) * 255);
          row.data.set(lut.subarray(v * 4, v * 4 + 4), i * 4);
        }
        fg.putImageData(row, 0, 0);
        fg.putImageData(row, 0, 1);
      }
    }

    size();
    const ro = new ResizeObserver(size);
    ro.observe(box);
    const off = app.onSpectrum((bins) => {
      latest = bins;
      if (!raf && !document.hidden) raf = requestAnimationFrame(draw);
    });
    return () => {
      off();
      ro.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  });

  function move(e: PointerEvent) {
    const s = session;
    if (!s) return;
    const r = box.getBoundingClientRect();
    const x = e.clientX - r.left;
    hover = { x, hz: s.centerHz + (x / r.width - 0.5) * s.sampleRate };
  }
</script>

<div class="spec" bind:this={box} onpointermove={move} onpointerleave={() => (hover = null)} role="img" aria-label="Spectrum and waterfall of the capture">
  <canvas class="plot" class:solo={!waterfall} bind:this={plot}></canvas>
  {#if waterfall}<canvas class="fall" bind:this={fall}></canvas>{/if}
  {#if hover}
    <div class="cursor" style:left="{hover.x}px"><span class="mono">{mhz(hover.hz)} MHz</span></div>
  {/if}
  {#if session}
    <div class="axis mono">
      <span>{mhz(session.centerHz - session.sampleRate / 2)}</span>
      <span>{mhz(session.centerHz)} MHz</span>
      <span>{mhz(session.centerHz + session.sampleRate / 2)}</span>
    </div>
  {/if}
</div>

<style>
  .spec {
    position: relative;
    display: grid;
    grid-template-rows: minmax(0, 2fr) minmax(0, 3fr) auto;
    height: 100%;
    min-height: 0;
    background: var(--bg);
    overflow: hidden;
  }
  .spec:has(.solo) {
    grid-template-rows: minmax(0, 1fr) auto;
  }
  canvas {
    width: 100%;
    height: 100%;
    display: block;
  }
  .fall {
    image-rendering: pixelated;
    border-top: 1px solid var(--line);
  }
  .axis {
    display: flex;
    justify-content: space-between;
    padding: 4px 8px;
    border-top: 1px solid var(--line);
    font-size: 11px;
    color: var(--faint);
    background: var(--surface);
  }
  .cursor {
    position: absolute;
    top: 0;
    bottom: 24px;
    width: 1px;
    background: var(--ink);
    opacity: 0.6;
    pointer-events: none;
  }
  .cursor span {
    position: absolute;
    top: 6px;
    left: 6px;
    padding: 2px 6px;
    border-radius: var(--r-sm);
    background: var(--surface-3);
    color: var(--ink);
    font-size: 11px;
    white-space: nowrap;
  }
</style>
