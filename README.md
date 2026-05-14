# FerrumDB

FerrumDB is an open-source database engine written in Rust, built around typed items, persistent storage, and scalable key-based data access.

## Goal

This repository is the next step after the Rust learning path projects. The purpose is to move from a small educational persistent store into a more realistic database engine with clearer storage layers and better testing discipline.

## Current Foundation

The current codebase already includes:

- an internal `Store` model with typed values
- binary persistence using Protocol Buffers
- conversion logic between internal Rust types and persisted protobuf messages
- a clean separation between `store`, `persistence`, and `errors`

This is the seed, not the final architecture.

## Next Steps

The next major implementation goals are:

1. introduce a Write-Ahead Log (WAL) for durable append-first writes
2. introduce SSTable-style immutable on-disk structures
3. define an engine boundary that is easier to test than the current learning-project store
4. add repeatable tests for recovery, compaction-related behavior, and read/write correctness

## Near-Term Design Direction

FerrumDB is expected to evolve toward:

- durable writes through a WAL
- immutable sorted storage files
- startup recovery from persisted state
- clearer separation between engine logic and storage formats
- stronger automated testing around persistence and correctness

## Testing Goals

Before adding more engine complexity, this project should gain a solid testing base. In practice, that means:

- unit tests for internal conversions and storage primitives
- integration tests for write/read/delete behavior
- recovery tests that simulate restart after persisted writes
- later, tests around WAL replay and SSTable loading

## Repository Intent

FerrumDB is not meant to be another small Rust exercise. It is the start of a standalone database project built with a narrower and more realistic storage-engine focus.

## License

FerrumDB is licensed under the Apache License, Version 2.0. See `LICENSE` for the full license text and `NOTICE` for the required attribution notice.
