<script lang="ts">
  import { backend } from "../lib/backend";
  import { app, resetAdvanced, settings, type Settings } from "../lib/store.svelte";

  type TextKey = { [K in keyof Settings]: Settings[K] extends string ? K : never }[keyof Settings];
  type BoolKey = { [K in keyof Settings]: Settings[K] extends boolean ? K : never }[keyof Settings];

  const isFile = $derived(settings.sourceKind === "file");
  const mode = $derived(settings.mode);

  async function browse(key: TextKey, title: string) {
    const p = await (await backend()).pickFile(title);
    if (p) settings[key] = p as never;
  }
  async function browseSave(key: TextKey, title: string, name: string) {
    const p = await (await backend()).pickSave(title, name);
    if (p) settings[key] = p as never;
  }
</script>

{#snippet field(key: TextKey, label: string, placeholder: string, help: string = "")}
  <label class="field">
    <span>{label}</span>
    <input class="input" bind:value={settings[key]} {placeholder} spellcheck="false" autocomplete="off" />
    {#if help}<small>{help}</small>{/if}
  </label>
{/snippet}

{#snippet check(key: BoolKey, label: string, help: string = "")}
  <label class="check">
    <input type="checkbox" bind:checked={settings[key]} />
    <span>{label}{#if help}<small>{help}</small>{/if}</span>
  </label>
{/snippet}

{#snippet path(key: TextKey, label: string, placeholder: string, help: string, save: string = "")}
  <div class="field">
    <span>{label}</span>
    <div class="pathrow">
      <input class="input" bind:value={settings[key]} {placeholder} spellcheck="false" aria-label={label} />
      <button class="btn sm" onclick={() => (save ? browseSave(key, label, save) : browse(key, label))}>Browse</button>
    </div>
    {#if help}<small>{help}</small>{/if}
  </div>
{/snippet}

<div class="advanced">
  <fieldset>
    <legend>Tuning</legend>
    <p class="lede">Leave anything blank and xng derives it from the signal's built-in frequency plan.</p>
    <div class="grid">
      {#if !isFile}
        {@render field("sampleRate", "Sample rate (Hz)", "auto", "A whole multiple of the channel rate; 2400000 suits most VHF signals.")}
        {@render field("centerFreq", "Center frequency", "auto", "For example 131.500M.")}
        {@render field("gain", "Tuner gain (dB)", "hardware AGC", "")}
      {/if}
      <div class="wide">
        {@render field("channels", "Channels", "auto, from the plan", "Comma separated, in MHz: 131.550, 131.725.")}
      </div>
      {@render field("receiverPos", "Receiver position", "lat,lon", "Enables ADS-B surface positions and pins your station on the map.")}
      <label class="field">
        <span>Demod effort</span>
        <select class="input" bind:value={settings.demodEffort}>
          <option value="">Automatic ({isFile ? "max" : "live"})</option>
          <option value="live">Live: keep up in real time</option>
          <option value="max">Max: try every timing grid</option>
        </select>
      </label>
      {#if mode === "vdl2"}
        {@render field("maxPpm", "Max carrier offset (ppm)", "no limit", "Rejects VDL2 bursts further off frequency than this.")}
      {/if}
      {@render field("decodeThreads", "Decode threads", `auto (${app.info?.cores ?? "all"} cores)`, "Applies from the first session after launch.")}
    </div>
  </fieldset>

  <fieldset>
    <legend>Filters</legend>
    <div class="grid">
      {@render field("filterLabels", "Only these ACARS labels", "all", "Comma separated: H1, Q0.")}
      {@render field("excludeLabels", "Drop these ACARS labels", "none", "")}
      {#if mode === "ais" || mode === "vdes"}
        {@render field("aisIncludeTypes", "Only AIS message types", "all", "")}
        {@render field("aisExcludeTypes", "Drop AIS message types", "none", "")}
        {@render field("aisIncludeMmsi", "Only these MMSIs", "all", "")}
        {@render field("aisExcludeMmsi", "Drop these MMSIs", "none", "")}
        {@render field("aisMinInterval", "Min seconds per vessel", "off", "Thins position reports from each vessel.")}
        {@render field("aisDedupWindow", "Duplicate window (s)", "off", "")}
      {/if}
    </div>
  </fieldset>

  <fieldset>
    <legend>Station and feeds</legend>
    <div class="grid">
      {@render field("stationId", "Station ID", "XNG-DESKTOP", "For example XX-KSEA-ACARS1. Required to feed Airframes.")}
      <div class="checks">
        {@render check("feedAirframes", "Feed Airframes", "Sends decoded messages to feed.airframes.io.")}
      </div>
      {@render field("asf2Grpc", "asf-2.0 over gRPC", "http://127.0.0.1:6001", "")}
      {@render field("asf2Quic", "asf-2.0 over QUIC", "host:port", "")}
      {@render path("asf2QuicCa", "QUIC trusted CA file", "system roots", "PEM certificate to trust for the QUIC ingest.")}
      <div class="checks">
        {@render check("asf2QuicInsecure", "Skip QUIC certificate checks", "Lab use only: the feed can be intercepted or spoofed.")}
      </div>
      {@render field("udp", "acarsdec JSON over UDP", "host:port, host:port", "")}
      {@render field("mqtt", "MQTT broker", "mqtt://user:pass@host:1883", "")}
      {@render field("mqttTopic", "MQTT topic prefix", "xng", "")}
      {@render field("zmq", "ZMQ PUB sockets", "tcp://0.0.0.0:5555", "Prefix with connect: to connect out instead of binding.")}
    </div>
  </fieldset>

  <fieldset>
    <legend>Local outputs and servers</legend>
    <div class="grid">
      {@render path("jsonl", "Log messages to a JSONL file", "off", "", "xng-messages.jsonl")}
      {@render field("http", "Web dashboard", "0.0.0.0:8080", "Serves the browser dashboard and its JSON, GeoJSON, GPX and KML.")}
      {@render field("metrics", "Prometheus metrics", "0.0.0.0:9090", "")}
      {@render field("sbs", "SBS-1 / BaseStation server", "0.0.0.0:30003", "Mode S and ADS-B only.")}
      {@render field("beast", "Beast binary server", "0.0.0.0:30005", "Mode S frames, dump1090 style.")}
      {@render field("nmeaTcp", "NMEA AIVDM over TCP", "0.0.0.0:10110", "")}
      {@render field("nmeaUdp", "NMEA AIVDM over UDP", "host:10110", "")}
      <div class="checks">
        {@render check("nmeaTagBlocks", "Add NMEA tag blocks", "Prefixes each sentence with station and time.")}
      </div>
      {@render field("ownShipMmsi", "Own-ship MMSI", "off", "With a receiver position, reports your own vessel every 30 s.")}
      {@render field("gsmtap", "GSMTAP for Wireshark", "127.0.0.1:4729", "Iridium GSM frames.")}
      {@render field("iridiumSatmap", "Iridium satellite map", "off", "Type auto to fetch TLEs, or give a TLE file path.")}
      {@render path("gsFile", "VDL2 ground-station names", "none", "JSON mapping hex addresses to names.")}
      {@render path("aircraftDb", "Aircraft database", "none", "tar1090 or Mictronics CSV; adds registrations and types.")}
    </div>
  </fieldset>

  <div class="foot">
    <button class="btn sm quiet" onclick={resetAdvanced}>Reset advanced settings</button>
  </div>
</div>

<style>
  .advanced {
    display: grid;
    gap: 22px;
    padding-top: 16px;
  }
  fieldset {
    margin: 0;
    padding: 18px;
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--surface);
    min-width: 0;
  }
  legend {
    padding: 0 8px;
    margin-left: -8px;
    font-size: 14px;
    font-weight: 600;
  }
  .lede {
    margin: -2px 0 14px;
    color: var(--muted);
    font-size: 13px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: 14px 16px;
    align-items: start;
  }
  .wide {
    grid-column: 1 / -1;
  }
  .checks {
    display: grid;
    gap: 10px;
    padding-top: 22px;
  }
  .pathrow {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 6px;
  }
  .pathrow .btn {
    height: 34px;
  }
  .foot {
    display: flex;
    justify-content: flex-end;
  }
</style>
