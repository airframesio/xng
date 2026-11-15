<script lang="ts">
  // A small hand-picked stroke set, so the app ships no icon font.
  const PATHS: Record<string, string> = {
    radio: "M4 9h16v10H4zM8 9l8-5M8 14h.01M12 14h5",
    file: "M6 3h8l4 4v14H6zM14 3v4h4",
    refresh: "M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7",
    play: "M7 5l12 7-12 7z",
    stop: "M6 6h12v12H6z",
    map: "M9 4 3 6v14l6-2 6 2 6-2V4l-6 2-6-2zM9 4v14M15 6v14",
    list: "M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01",
    wave: "M3 12h2l2-7 4 14 3-10 2 6 2-3h3",
    chevron: "M9 6l6 6-6 6",
    close: "M6 6l12 12M18 6 6 18",
    search: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4",
    copy: "M9 9h11v11H9zM5 15V4h11",
    download: "M12 4v11M7 11l5 5 5-5M5 20h14",
    sliders: "M4 7h10M18 7h2M4 17h2M10 17h10M14 4v6M6 14v6",
    alert: "M12 4 2.5 20h19L12 4zM12 10v5M12 17.5h.01",
    back: "M15 6l-6 6 6 6",
    scan: "M12 12l6-6M21 12a9 9 0 1 1-9-9M17 12a5 5 0 1 1-5-5",
    gauge: "M4 18a8 8 0 1 1 16 0M12 18l4-6",
  };
  let { name, size = 16 }: { name: string; size?: number } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.8"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={PATHS[name] ?? ""} />
</svg>
