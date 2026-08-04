# mojo-rs-mojom

Mojom language toolchain: lexer, parser, AST, imports, semantic validation.

**Status: Scaffolded** (structure only; no behavior yet).

## Scope (Phase 7)

* Lexer, parser, AST, imports, module resolution.
* Constants, enums, structs, unions, interfaces, methods, attributes.
* Nullability, version metadata, ordinals, default values.
* Semantic validation with exact error diagnostics.
* Deterministic output and source maps from generated code to Mojom
  definitions.

Compatibility inputs: the upstream `.mojom` corpora at the pinned revision.
