# `nodedb-fts`

> Block-Max WAND BM25, 16 stemmers, 27-language stopwords, LSM storage

Full-text search engine. BMW-optimized BM25 with 128-doc block pruning, Snowball stemmers, CJK bigram tokenization, posting compression with SIMD unpack, LSM segments, fuzzy + phrase queries, and native hybrid vector + text fusion.

## Status

Pre-1.0. APIs may change between minor versions until 1.0. See the
workspace [`README.md`](../README.md) for the full project overview and
the GitHub [release notes](https://github.com/NodeDB-Lab/nodedb/releases)
for per-version changes.

## License

Licensed under the Apache License, Version 2.0 ([`LICENSE-APACHE`](../LICENSE-APACHE-2.0)).
