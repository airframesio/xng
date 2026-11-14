<script lang="ts">
  import type { Feature } from "geojson";
  import maplibregl from "maplibre-gl";
  import { onMount } from "svelte";
  import { app } from "../lib/store.svelte";
  import type { Entity, EntityState } from "../lib/types";
  import Icon from "./Icon.svelte";

  let el: HTMLDivElement;
  let map: maplibregl.Map | undefined;
  let ready = $state(false);
  let offline = $state(false);
  let fitted = false;
  let picked = $state<{ kind: string; id: string } | null>(null);

  const KINDS: { key: keyof EntityState; kind: string; family: string; heading?: string }[] = [
    { key: "aircraft", kind: "Aircraft", family: "air", heading: "trk" },
    { key: "vessels", kind: "Vessel", family: "sea", heading: "cog" },
    { key: "beacons", kind: "Beacon", family: "ground" },
    { key: "iridium_sats", kind: "Iridium satellite", family: "sat" },
    { key: "iridium_devices", kind: "Iridium terminal", family: "sat" },
  ];

  function css(name: string) {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  function idOf(e: Entity): string {
    return String(e.id ?? e.icao ?? e.mmsi ?? e.sat ?? `${e.lat},${e.lon}`);
  }
  function labelOf(e: Entity): string {
    return String(e.flight ?? e.reg ?? e.name ?? e.icao ?? e.mmsi ?? e.id ?? "").trim();
  }

  const counts = $derived.by(() => {
    const s = app.entities;
    return {
      air: s?.aircraft.length ?? 0,
      sea: s?.vessels.length ?? 0,
      other: (s?.beacons.length ?? 0) + (s?.iridium_sats.length ?? 0) + (s?.iridium_devices.length ?? 0),
      alerts: s?.alerts.length ?? 0,
    };
  });

  const pickedEntity = $derived.by(() => {
    const s = app.entities;
    if (!s || !picked) return null;
    const k = KINDS.find((k) => k.kind === picked!.kind);
    const list = (k ? s[k.key] : []) as Entity[];
    return list.find((e) => idOf(e) === picked!.id) ?? null;
  });
  const pickedFacts = $derived.by(() => {
    const e = pickedEntity;
    if (!e) return [];
    const order = ["flight", "reg", "icao", "actype", "name", "mmsi", "country", "alt", "spd", "trk", "sog", "cog", "squawk", "msgs"];
    const names: Record<string, string> = {
      flight: "Flight", reg: "Registration", icao: "ICAO", actype: "Type", name: "Name", mmsi: "MMSI",
      country: "Flag", alt: picked?.kind === "Iridium satellite" ? "Altitude (km)" : "Altitude (ft)", spd: "Speed (kt)", trk: "Track (°)", sog: "Speed (kt)",
      cog: "Course (°)", squawk: "Squawk", msgs: "Messages",
    };
    return order.filter((k) => e[k] != null && e[k] !== "").map((k) => [names[k], String(e[k])] as const);
  });

  /** A heading marker in one family colour, outlined in the ground colour. */
  function arrow(fill: string, outline: string): ImageData {
    const s = 56;
    const c = document.createElement("canvas");
    c.width = c.height = s;
    const g = c.getContext("2d")!;
    g.translate(s / 2, s / 2);
    g.beginPath();
    g.moveTo(0, -21);
    g.lineTo(14, 18);
    g.lineTo(0, 10);
    g.lineTo(-14, 18);
    g.closePath();
    g.lineJoin = "round";
    g.lineWidth = 5;
    g.strokeStyle = outline;
    g.stroke();
    g.fillStyle = fill;
    g.fill();
    return g.getImageData(0, 0, s, s);
  }

  function addLayers(m: maplibregl.Map) {
    // Runs for the first style and again for the offline fallback style,
    // and both `load` and `styledata` can report the same one. So add only
    // what the current style lacks: a style swap may keep some pieces.
    const source = (id: string) => {
      if (!m.getSource(id)) m.addSource(id, { type: "geojson", data: { type: "FeatureCollection", features: [] } });
    };
    const layer = (spec: maplibregl.AddLayerObject) => {
      if (!m.getLayer(spec.id)) m.addLayer(spec);
    };
    const ink = css("--ink");
    const bg = css("--bg");
    const colour: maplibregl.ExpressionSpecification = [
      "match", ["get", "family"],
      "air", css("--air"), "sea", css("--sea"), "sat", css("--sat"), css("--ground"),
    ];
    for (const fam of ["air", "sea", "sat", "ground"]) {
      if (!m.hasImage(`arrow-${fam}`)) m.addImage(`arrow-${fam}`, arrow(css(`--${fam}`), bg), { pixelRatio: 2 });
    }
    source("trails");
    source("points");
    source("home");
    layer({
      id: "trails", type: "line", source: "trails",
      paint: { "line-color": colour, "line-width": 1.4, "line-opacity": 0.55 },
    });
    layer({
      id: "home", type: "circle", source: "home",
      paint: {
        "circle-radius": 6, "circle-color": bg, "circle-stroke-color": css("--accent"), "circle-stroke-width": 2.5,
      },
    });
    layer({
      id: "dots", type: "circle", source: "points", filter: ["!", ["has", "hdg"]],
      paint: {
        "circle-radius": ["case", ["get", "picked"], 7, 4.5],
        "circle-color": colour, "circle-stroke-color": bg, "circle-stroke-width": 1.5,
      },
    });
    layer({
      id: "arrows", type: "symbol", source: "points", filter: ["has", "hdg"],
      layout: {
        "icon-image": ["concat", "arrow-", ["get", "family"]],
        "icon-size": ["case", ["get", "picked"], 1.05, 0.72],
        "icon-rotate": ["get", "hdg"],
        "icon-rotation-alignment": "map",
        "icon-allow-overlap": true,
        "icon-ignore-placement": true,
      },
    });
    if (m.getStyle().glyphs) {
      layer({
        id: "labels", type: "symbol", source: "points", minzoom: 6.5,
        layout: {
          "text-field": ["get", "label"],
          "text-font": ["Noto Sans Regular"],
          "text-size": 11,
          "text-offset": [0, 1.25],
          "text-anchor": "top",
          "text-optional": true,
        },
        paint: { "text-color": ink, "text-halo-color": bg, "text-halo-width": 1.4 },
      });
    }
    // Layer-scoped listeners are keyed by layer id and outlive a style
    // swap, so wire them once.
    if (!ready) {
      for (const id of ["dots", "arrows"]) {
        m.on("click", id, (e) => {
          const p = e.features?.[0]?.properties;
          if (p) picked = { kind: String(p.kind), id: String(p.id) };
        });
        m.on("mouseenter", id, () => (m.getCanvas().style.cursor = "pointer"));
        m.on("mouseleave", id, () => (m.getCanvas().style.cursor = ""));
      }
    }
    ready = true;
  }

  function render(state: EntityState | null) {
    if (!map || !ready) return;
    const points: Feature[] = [];
    const trails: Feature[] = [];
    const bounds = new maplibregl.LngLatBounds();
    for (const k of KINDS) {
      for (const e of ((state?.[k.key] ?? []) as Entity[])) {
        if (typeof e.lat !== "number" || typeof e.lon !== "number") continue;
        const id = idOf(e);
        const props: Record<string, unknown> = {
          kind: k.kind, family: k.family, id, label: labelOf(e),
          picked: picked?.kind === k.kind && picked.id === id,
        };
        const hdg = k.heading ? e[k.heading] : undefined;
        if (typeof hdg === "number") props.hdg = hdg;
        points.push({ type: "Feature", properties: props, geometry: { type: "Point", coordinates: [e.lon, e.lat] } });
        bounds.extend([e.lon, e.lat]);
        if (e.trail && e.trail.length > 1) {
          trails.push({
            type: "Feature",
            properties: { family: k.family },
            geometry: { type: "LineString", coordinates: e.trail.map(([la, lo]) => [lo, la]) },
          });
        }
      }
    }
    const home: Feature[] = [];
    for (const s of state?.sessions ?? []) {
      const p = s.receiver_pos as [number, number] | undefined;
      if (Array.isArray(p)) {
        home.push({ type: "Feature", properties: {}, geometry: { type: "Point", coordinates: [p[1], p[0]] } });
        bounds.extend([p[1], p[0]]);
      }
    }
    (map.getSource("points") as maplibregl.GeoJSONSource).setData({ type: "FeatureCollection", features: points });
    (map.getSource("trails") as maplibregl.GeoJSONSource).setData({ type: "FeatureCollection", features: trails });
    (map.getSource("home") as maplibregl.GeoJSONSource).setData({ type: "FeatureCollection", features: home });
    if (!fitted && !bounds.isEmpty()) {
      fitted = true;
      map.fitBounds(bounds, { padding: 70, maxZoom: 9, duration: 600 });
    }
  }

  function fit() {
    fitted = false;
    render(app.entities);
  }

  onMount(() => {
    const dark = matchMedia("(prefers-color-scheme: dark)").matches;
    const blank: maplibregl.StyleSpecification = {
      version: 8,
      sources: {},
      layers: [{ id: "bg", type: "background", paint: { "background-color": css("--surface") } }],
    };
    const m = new maplibregl.Map({
      container: el,
      style: `https://tiles.openfreemap.org/styles/${dark ? "dark" : "positron"}`,
      center: [0, 25],
      zoom: 1.4,
      attributionControl: { compact: true },
      dragRotate: false,
      pitchWithRotate: false,
    });
    map = m;
    m.addControl(new maplibregl.NavigationControl({ showCompass: false }), "bottom-right");
    let styled = false;
    m.on("load", () => {
      styled = true;
      addLayers(m);
      render(app.entities);
    });
    // No network (or the tile host is down): keep plotting on a plain ground.
    m.on("error", (e) => {
      if (styled || offline) return;
      const msg = String((e as { error?: Error }).error?.message ?? "");
      if (!/style|fetch|load|network|404|403/i.test(msg)) return;
      offline = true;
      // Replace the style outright (no diff against the half-loaded one),
      // and add the layers once the new one is in place.
      m.setStyle(blank, { diff: false });
      m.once("style.load", () => {
        styled = true;
        addLayers(m);
        render(app.entities);
      });
    });
    const ro = new ResizeObserver(() => m.resize());
    ro.observe(el);
    return () => {
      ro.disconnect();
      m.remove();
      map = undefined;
    };
  });

  $effect(() => {
    void picked;
    render(app.entities);
  });
</script>

<div class="mapwrap">
  <div class="map" bind:this={el}></div>

  <div class="legend">
    <span style:--c="var(--air)"><b class="mono">{counts.air}</b> aircraft</span>
    <span style:--c="var(--sea)"><b class="mono">{counts.sea}</b> vessels</span>
    {#if counts.other}<span style:--c="var(--sat)"><b class="mono">{counts.other}</b> other</span>{/if}
    {#if counts.alerts}<span class="alert"><Icon name="alert" size={13} /> <b class="mono">{counts.alerts}</b> distress</span>{/if}
    <button class="btn sm quiet" onclick={fit}>Fit all</button>
  </div>

  {#if offline}
    <div class="offline">Map tiles are unavailable, so positions are shown without a base map.</div>
  {/if}

  {#if pickedEntity && picked}
    <div class="card">
      <header>
        <div>
          <small>{picked.kind}</small>
          <h3 class="mono">{labelOf(pickedEntity) || picked.id}</h3>
        </div>
        <button class="btn sm quiet" onclick={() => (picked = null)} aria-label="Close"><Icon name="close" size={14} /></button>
      </header>
      <dl>
        {#each pickedFacts as [k, v] (k)}
          <dt>{k}</dt>
          <dd class="mono selectable">{v}</dd>
        {/each}
      </dl>
    </div>
  {/if}
</div>

<style>
  .mapwrap {
    position: relative;
    height: 100%;
    min-height: 0;
    background: var(--surface);
  }
  .map {
    position: absolute;
    inset: 0;
  }
  .legend {
    position: absolute;
    top: 12px;
    left: 12px;
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 4px 4px 4px 12px;
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    background: color-mix(in srgb, var(--surface) 92%, transparent);
    font-size: 12px;
    color: var(--muted);
  }
  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .legend span:not(.alert)::before {
    content: "";
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--c);
  }
  .legend b {
    color: var(--ink);
    font-weight: 500;
  }
  .legend .alert,
  .legend .alert b {
    color: var(--bad);
  }
  .offline {
    position: absolute;
    left: 12px;
    bottom: 12px;
    padding: 6px 10px;
    border: 1px solid var(--line-strong);
    border-radius: var(--r);
    background: var(--surface);
    font-size: 12px;
    color: var(--muted);
  }
  .card {
    position: absolute;
    top: 12px;
    right: 12px;
    width: 232px;
    border: 1px solid var(--line-strong);
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: 0 10px 28px rgba(0, 0, 0, 0.35);
  }
  .card header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    padding: 10px 8px 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  .card small {
    font-size: 11px;
    color: var(--faint);
  }
  .card h3 {
    font-size: 15px;
    font-weight: 500;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 5px 12px;
    margin: 0;
    padding: 10px 12px 12px;
    font-size: 12px;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    text-align: right;
  }
</style>
