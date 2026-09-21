# Type Identity and the Reconciled Type Graph

This page explains how a Rust type acquires its **final identity** as it
travels through typeshare's core pipeline, and how the reconciled type graph
(`typeshare_core::ir`) makes that identity inspectable. Everything described
here is exercised by executable tests: the inline-fixture snapshots in
`core/tests/ir_graph_snapshots.rs` and the cross-backend contract in
`core/tests/language_contract.rs`.

## Pipeline overview

```text
Rust source (syn AST)
   │  parser.rs / visitors.rs      — collect #[typeshare] items, serde attrs
   ▼
ParsedData per crate              — Id { original, renamed, serde_rename }
   │  reconcile::reconcile_aliases — rewrite references after serde renames
   ▼
Reconciled items                  — the only graph backends ever see
   │  ir::IrGraph                  — nodes, edges, SCCs, topo groups (analysis)
   ▼
language::Language::generate_types— topsort + per-language lowering
   ▼
Generated source (TypeScript, Swift, Kotlin, Scala, Go, Python)
```

## Stage 1: parsing — source identity

The visitor walks the `syn` AST and records, for every `#[typeshare]` item:

- `original`: the Rust identifier as written;
- `module`: the nested `mod` path the item was declared in (tracked by
  `TypeShareVisitor`'s module stack);
- `renamed`: the name after applying `rename_all` and an optional explicit
  `#[serde(rename = "...")]`. `serde_rename` is set only for the explicit
  form;
- `generic_types`: declared generic parameters in source order;
- fields/variants that carry their own `Id` triple, with per-field
  `#[serde(rename)]` taking precedence over the container's `rename_all`;
- skipped items: `#[serde(skip)]` / `#[typeshare(skip)]` fields and variants
  are dropped at this stage and never reach the graph;
- `#[serde(flatten)]` is rejected with a parse diagnostic (collected into
  `ParsedData::errors`), because flattening has no faithful representation in
  the target type systems.

The **source identity** of a type is the triple
`(crate, module path, original name)`. Two modules may declare `Account`
independently; their source identities stay distinct
(`module_crate::billing::Account` vs `module_crate::identity::Account`).

## Stage 2: rename — serialized identity

`serde(rename)` changes the *serialized* (wire/generated) name without
changing the source identity. `reconcile_aliases` collects every renamed type
and rewrites all inbound references — in struct fields, enum tuple variants,
alias right-hand sides and const types — so that after reconciliation every
reference uses the target's **final** name. A reference that only matches
because of this rewrite is flagged `via_rename_reconcile` in the IR, which is
how rename-induced cycles are detected (see below).

`rename_all` on a container applies to its fields/variants; an explicit
per-field `rename` wins. Both are pure parse-time rewrites and do not affect
type *references* — only the top-level `serde(rename)` on a type does.

## Stage 3: generic parameters and instantiation

Generic parameters are **not** graph nodes. When the IR collects dependency
edges, any reference whose name matches a generic parameter of the enclosing
item is skipped — `BoxOf<Thing>` with field `item: Thing` has no edge to a
concrete `Thing` type. If a parameter name collides with a real typeshared
type, the parameter shadows it inside that item and the IR emits a
`generic_shadowing` diagnostic.

Generic *instantiation* (`Wrapper<Inner>`) contributes edges to both the
generic type constructor (`Wrapper`) and every concrete argument (`Inner`),
because the generated code for the instantiation mentions both. The
instantiated type itself is not a node: backends expand or erase it during
lowering (e.g. TypeScript emits `Wrapper<Inner>` inline; Go monomorphizes
nothing and forbids generics).

## Stage 4: dependency ordering

Edges are collected from struct fields, enum tuple variants, alias
right-hand sides and const types. Anonymous-struct enum variant payloads are
deliberately **not** edges: the existing emitters lower them inline next to
the enum (or as `{Enum}{Variant}Inner` helpers), so they never constrain
item order. The IR mirrors the behavior of the emitters rather than
inventing a stricter graph.

From the edges the IR derives:

- **SCCs** (Tarjan, iterative): self-recursive types and mutually recursive
  groups. Mutual recursion is *not* a topological error — it is presented as
  a stable strongly connected group, and each backend decides how to break
  the cycle (Swift's `indirect`, forward declarations, boxed references,
  ...). An SCC whose internal edges were introduced by `serde(rename)`
  reconciliation is flagged `induced_by_rename` and reported as a
  `rename_cycle` diagnostic.
- **Topological groups** (Kahn levels over the SCC condensation): group `n`
  only depends on groups `< n`. Ties are broken by **source declaration
  order**, never by a global name sort, so the graph algorithm cannot be
  masked by alphabetizing and the public emission order of existing
  generated files is preserved.

The emitters keep using the pre-existing `topsort` for actual output; the IR
is an analysis view over the same reconciled items, and the contract tests
assert both agree on cross-SCC ordering.

## Diagnostics

All diagnostics are deterministic (sorted by stable node keys) and rendered
in the JSON snapshot:

| Code | Meaning |
| --- | --- |
| `same_name_different_module` | Same original name declared in several modules; source identities stay distinct, but single-file output flattens modules. |
| `serialized_name_conflict` | Two declarations serialize under one wire name; ambiguous references produce **no** fabricated edge. |
| `rename_cycle` | An SCC exists only because `serde(rename)` rewired a reference; emitted as an SCC, not an error. |
| `generic_shadowing` | A generic parameter shadows a named type; occurrences bind to the parameter. |

## Complexity

Let `V` be the number of types and `E` the number of resolved references.

- Parsing and reconcile: `O(V + E)` (one pass; rename lookup is a hash map).
- IR construction and edge collection: `O(V + E)`.
- Tarjan SCC: `O(V + E)`, iterative to avoid stack depth limits.
- Kahn levels with source-order tie-break: `O((V + E) + k log k)` where `k`
  is the number of simultaneously ready SCCs (a `BTreeSet` of ready groups).
- JSON rendering: `O(V + E)`; the output contains only graph data — no file
  paths, timestamps, hash-map iteration order, or toolchain versions.

## Compatibility notes

- The IR module is additive: parsing, reconcile, `topsort` and every
  language generator keep their existing behavior and public API. The only
  change to shared data structures is the additional `module` field on
  parsed items, which defaults to the crate root and does not alter
  generated output.
- `typeshare_core::topsort` is now `pub` so the relationship between the
  emitter ordering and the IR groups is part of the documented surface; its
  algorithm is unchanged.
- The shared-contract tests pin the declaration set and cross-SCC ordering
  for TypeScript, Swift, Kotlin and Go, plus a manifest of known lowering
  differences (anonymous-variant helper types, Swift `indirect`, Go const
  blocks, Kotlin payload classes). Adding a backend means extending the
  extractor and the manifest, not weakening the contract.
