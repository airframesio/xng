# xng-legacy

The pre-rewrite xng: a session orchestrator that wraps `dumphfdl` as an external
process, with an actix-web control API, SQLite state DB, Elasticsearch output,
and Airframes feeding via dumphfdl-native JSON.

It is **excluded from the workspace build** and kept here as:

1. A working fallback while the native decode cores reach parity.
2. The seed for the future second-class `xng-extern` wrapper module
   (see `docs/ARCHITECTURE.md` §4.1 and milestone M10).

Build it standalone (requires SoapySDR development files):

```bash
cd legacy && cargo build --release   # produces target/release/xng-legacy
```
