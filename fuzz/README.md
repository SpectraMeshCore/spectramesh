# Fuzzing

Fuzz targets for the parsers in `spectramesh-core`, run with
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) on a nightly toolchain.

| Target       | What it feeds                                                        |
|--------------|----------------------------------------------------------------------|
| `frame`      | Raw bytes to the frame, header, TLV and trailer decoders              |
| `modem`      | Bytes to the SLIP modem decoder, checking re-encoded messages match   |
| `key_text`   | Text to the `smk1-` and `smc1-` key parsers, checking round trips     |
| `reassembly` | Fragment sequences to reassembly, checking its size and count limits  |
| `router`     | A whole router, driven by a neighbor that holds the mesh key          |

Random bytes almost never carry a valid link tag, so the `router` target
plays a mesh member: it passes the router's challenge, then turns its input
into correctly tagged control, data and fragment frames, replays, raw bytes,
time passing and the router's own sends. See `fuzz_targets/router.rs`.

```sh
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run router -- -max_total_time=300
```

CI fuzzes each target for 30 seconds on every push, and for 10 minutes
weekly (`.github/workflows/fuzz.yml`). A crash is kept as a workflow artifact;
reproduce it with `cargo +nightly fuzz run <target> <file>`.
