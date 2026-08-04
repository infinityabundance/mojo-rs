# mojo-rs-casefile

Casefile schema, replay protocol, harness events, and differential comparison.

**Status: Scaffolded** (structure only; no behavior yet).

This crate owns:

* The casefile model (operations, preconditions, expected contract,
  normalizers) — schema: `casefiles/schema/casefile.schema.json`.
* The events model — schema: `casefiles/schema/events.schema.json`.
* The comparison engine: oracle events vs candidate events with the declared
  normalizer set, producing `comparison.schema.json` results.
* The `mojo-rs-casefile` CLI (`baseline` and `compare` subcommands) used by
  the court pipeline.

The oracle driver runs casefiles against the official Mojo C API; the
candidate harness (in `mojo-rs-oracle`... no — the candidate harness is built
on the candidate runtime) runs the same casefiles. This crate stays free of
any dependency on the candidate runtime, preserving physical separation.
