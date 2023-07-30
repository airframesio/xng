# xng
[![Rust](https://github.com/airframesio/xng/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/airframesio/xng/actions/workflows/rust.yml)

Next-generation multi-decoder client for SDRs, written in Rust for Linux.

Notable features:
 * Deep integration with [airframes.io](https://airframes.io); feed data using a single flag or use Airframes to seed initial active HFDL frequencies
 * Integration with [SoapySDR](https://github.com/pothosware/SoapySDR) for determining valid sample rates
 * Efficient listening -- no need to burn CPU listening to inactive frequencies; session restarts on detection of new active frequencies to keep listening band up-to-date
 * Web API for controlling session settings and exposing stats + metrics
 * and more...

For Ubuntu/Debian, check out GitHub Actions of `amd64` packages.

## Building
 1. Install a stable [Rust](https://www.rust-lang.org/learn/get-started) toolchain. Make sure the `cargo` command is in `PATH` environment variable after completion.
 2. Make sure to install `SoapySDR` development files either by compiling manually or installing the package provided by your Linux distribution's package manager.
 3. Clone the xng repository:
```bash
git clone https://github.com/airframesio/xng
```
 4. Compile and build `xng`:
```bash
cargo build --release
```
 5. Compiled binary should be built in `./target/release/xng`

## How To Run
Following example starts a HFDL listening session on the 8MHz band (as determined by splitting the `systable.conf` bands into sample rate wide frequency ranges) with the following options:
 * Feed all received HFDL frames to Airframes with a station name of `MY-STATION-ID`
 * Use Airframes active HFDL frequencies API to determine active frequencies
 * Only listen on active HFDL frequencies
 * Store frequencies/aircraft events stats into a local DB file, `xng_state.db`
 * Index the frames into the `xng_acars_db` index on the Elasticsearch server at `https://my-es-server:9200`
 * Use the SoapySDR `airspyhf` driver

**NOTE:** If you want to index received frames to a local ElasticSearch instance, run the following command first:
```bash
xng init_es --elastic "http://my-es-server:9200" --elastic-index xng_acars_db
```

```bash
xng hfdl -vvv --systable /etc/systable.conf --sample-rate 512000 --start-band-contains 8000 --use-airframes-gs-map --method random --only-listen-on-active --state-db xng_state.db --feed-airframes --elastic "https://my-es-server:9200" --elastic-index xng_acars_db  -- --soapysdr driver=airspyhf --station-id "MY-STATION-ID"
```

## Web API Endpoints
Examine which frequencies have been heard from and from which ground stations they were from or meant to go to. 
```bash
curl -H "Content-Type: application/json" "http://localhost:7871/api/frequency/stats/" | jq
```

Examine all non-stale (as determined by timeout value configurable by the user) ground stations 
```bash
curl -H "Content-Type: application/json" "http://localhost:7871/api/ground-station/active/" | jq
```

Delete all aircraft events and ground station change events before a specific time (such as July 1, 2023 at 00:00 UTC in this example)
```bash
curl -H "Content-Type: application/json" -X DELETE "http://localhost:7871/api/cleanup/?before=2023-07-01T00:00:00Z"
```

Examine application settings -- all items in `props` are modifiable via `PATCH` (see next example)
```bash
curl -H "Content-Type: application/json" "http://localhost:7871/api/settings/" | jq
```

Update application settings (such as the next session's frequency band)
```bash
curl -H "Content-Type: application/json" -X PATCH -d '{"prop":"next_session_band","value":17000}' "http://localhost:7871/api/settings/"
```

Update application settings (setting a session schedule that sets the listening band to 21000 at 9am and 8000 at 8pm)
```bash
curl -H "Content-Type: application/json" -X PATCH -d '{"prop":"session_schedule","value":"time=9:00,band_contains=21000;time=20:00,band_contains=8000"}' "http://localhost:7871/api/settings/"
```

Force end session (can be used in conjunction with update application settings to manually force a listening frequencies change)
```bash
curl -H "Content-Type: application/json" -X DELETE "http://localhost:7871/api/session/"
```
## TODO
- [x] Web API endpoint to clean up state DB by clearing aircraft/ground station events older than a certain date
- [x] Web API endpoint to show flight overview (latest position from all callsign/ICAO combinations)
- [x] Web API endpoint to show detailed flight path by ICAO/tail/callsign

- [ ] Fancy verbose frame status messages
- [ ] More documentation detailing advanced session settings like scheduling and next session frequency strategies
- [ ] Simple front-end UI to graphically view aircraft events
- [ ] Add support for `dumpvdl2` via `aoa` subcommand
- [ ] Add support for `acarsdec` via `poa` subcommand
- [ ] Add support for `gr-iridium` via `irdm` subcommand
