# `nodedb-codec`

> Compression codecs: ALP, FastLanes, FSST, Gorilla, LZ4, vector quantization

Per-column codec implementations used by the columnar / timeseries / spatial engines, plus the vector quantization frontier (SQ8, PQ, OPQ, RaBitQ, BBQ, Ternary BitNet 1.58, Binary). Each codec is a free-standing module — pick what you need.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Apache License, Version 2.0 ([`LICENSE-APACHE`](../LICENSE-APACHE-2.0)).
