# Design: 128-bit Integer Data Type (`Int128` / `HUGEINT`)

Status: **Proposed** — not yet implemented.
Author: design note, 2026-06-13.
Scope: add a native signed 128-bit integer scalar type end-to-end (parse → store → execute → wire).

## Motivation

The "ZFS 128-bit" framing is about exhaustion-proof width: ZFS chose 128-bit
addressing because 64-bit ceilings are eventually hit. The database analog is a
native **128-bit integer** for values `i64` cannot hold without loss —
high-resolution counters, capacity/byte accounting, nanosecond-since-epoch with
headroom, large monotonic IDs, and exact integer arithmetic beyond
±9.2×10¹⁸.

### What already exists (and why it is not enough)

NodeDB already ships **16-byte (128-bit) fixed-width slots** wired through every
layer, so the storage machinery is proven:

| Existing type | Backing | Width | OID |
|---------------|---------|-------|-----|
| `Decimal { precision, scale }` | `rust_decimal::Decimal` | 16 bytes | 1700 (NUMERIC) |
| `Uuid` | string ↔ 16-byte | 16 bytes | 2950 |
| `Ulid` | string ↔ 16-byte | 16 bytes | 2950 |

What is missing is a **native integer**:

- `Value::Integer` is `i64` — see `nodedb-types/src/value/core.rs:62`.
- `i128`/`u128` appear nowhere in the engine except SIMD intrinsics.
- `Decimal` can *represent* 128-bit-range values but is a base-10
  arbitrary-precision type (precision ≤ 38), not a fixed-width two's-complement
  integer. It has different overflow, comparison, and arithmetic semantics and
  is heavier per-op. For pure integer workloads (bitwise ops, exact counters,
  wraparound-free accumulation) a true `i128` is the right primitive.

**Decision point for the reader:** if your use case is exact *decimal* math,
`Decimal` already covers it — stop here. This doc is for a true fixed-width
integer.

## Design decisions

### D1. Signedness — signed `i128` (with `u128` deferred)

Ship `Int128` backed by Rust `i128` first. It composes with the existing
signed-integer coercion and comparison paths. A `UInt128`/`u128` variant can
follow the same template later if capacity-counter use cases demand the full
unsigned range; flag it as a follow-up rather than blocking on it.

### D2. New `Value` variant — `Value::Int128(i128)` (required)

A 128-bit integer **cannot** be folded into `Value::Integer(i64)` without
truncation, so a new runtime variant is unavoidable. This is the most invasive
part of the change because `Value` is matched in many places (`#[non_exhaustive]`
softens the blow for external arms, but internal exhaustive matches must each
gain an arm).

```rust
/// Signed 128-bit integer (two's complement). Width beyond i64.
Int128(i128),
```

### D3. Wire mapping — NUMERIC (OID 1700)

PostgreSQL has **no native 128-bit integer OID**. Options:

| OID | Type | Client sees | Verdict |
|-----|------|-------------|---------|
| 1700 | NUMERIC | correct exact numeric value | **chosen** |
| 25 | TEXT | decimal string | fallback only |
| 20 | INT8 | — | ✗ truncates / lies about width |

NUMERIC is honest about the value and lets standard pg clients parse it
numerically. Document explicitly that NodeDB stores it as fixed-width `i128`
internally; the NUMERIC OID is a wire-presentation choice, not the storage type.

### D4. On-disk encoding — 16 little-endian bytes

Mirror the `Decimal`/`Uuid` 16-byte fixed path exactly: `i128::to_le_bytes()` /
`i128::from_le_bytes()`. Fixed-size, no offset-table entry.

### D5. SQL keywords — `INT128` and `HUGEINT`

Accept both `INT128` (explicit) and `HUGEINT` (DuckDB-compatible alias). Canonical
`Display` form: `INT128`.

## Touch-points (end-to-end)

Ordered by layer. Each is a single new match arm unless noted.

### Type system — `nodedb-types`

| File | Change |
|------|--------|
| `src/columnar/column_type.rs` | add `ColumnType::Int128` variant (enum is `#[non_exhaustive]`, doc says it "grows with each type system expansion") |
| `src/columnar/column_type.rs` `fixed_size()` (~L77) | add `Int128` to the `Some(16)` arm alongside `Decimal`/`Uuid`/`Ulid` |
| `src/columnar/column_type.rs` `to_pg_oid()` (~L121) | `Self::Int128 => 1700` |
| `src/columnar/column_type.rs` `accepts()` (~L149) | `(Self::Int128, Value::Integer(_) \| Value::String(_))` (and `Int128` once that value exists) |
| `src/columnar/column_parse.rs` `from_str` (~L122) | map `"INT128"` / `"HUGEINT"` → `ColumnType::Int128` |
| `src/columnar/column_parse.rs` `Display` (~L27) | `Int128 => "INT128"` |
| `src/value/core.rs` (~L62) | add `Value::Int128(i128)` variant + any `is_*`/accessor helpers |
| `src/value/msgpack.rs` | new zerompk tag **21** (20 = `Vector` is the current max); encode = tag + 16 LE bytes, decode mirror |
| `src/value/coerce.rs` | extend `eq_coerced` / `cmp_coerced` numeric coercion to include `Int128` (Integer↔Int128↔Float↔Decimal) |

### Storage / strict format — `nodedb-strict`

| File | Change |
|------|--------|
| `src/encode.rs` `encode_fixed` (~L204) | `Int128` → 16 LE bytes |
| `src/decode.rs` `decode_fixed_value` | `Int128` → `i128::from_le_bytes` |
| `src/arrow_extract.rs` | map `Int128` → Arrow `Decimal128` (Arrow has no int128; Decimal128 with scale 0 is the faithful carrier) |

### Execution / coercion — `nodedb`

| File | Change |
|------|--------|
| `src/data/executor/strict_format/coerce.rs` | new arm: coerce from `Integer` (widen), `String` (parse), `Float` (lossy — reject or warn), `Decimal` (if integral) |
| `src/data/executor/handlers/columnar_agg*.rs` | SUM/MIN/MAX/AVG/COUNT dispatch arms |
| `src/data/executor/handlers/columnar_filter/eval.rs` | predicate (`<,>,=,IN,BETWEEN`) arms |
| `src/control/planner/catalog_adapter.rs` `convert_column_type` | `ColumnType::Int128` → `SqlDataType::Int128` |
| `src/control/server/pgwire/types/field.rs` | OID flows through `to_pg_oid()`; verify result-set field descriptor + value text-encoding emit decimal string |

### Query / arithmetic — `nodedb-query`, `nodedb-sql`, `nodedb-array`, `nodedb-columnar`

| File | Change |
|------|--------|
| `nodedb-sql/src/types_expr.rs` (~L139) | add `SqlDataType::Int128` |
| `nodedb-query/src/expr/binary.rs` (~L44) | `eval_binary_op` — define promotion: `Int128 op Integer → Int128`; overflow policy (checked/saturating — see Open Questions) |
| `nodedb-columnar/src/memtable/column_data/{types,push}.rs` | column buffer for 16-byte fixed int |
| `nodedb-columnar/src/writer/block.rs`, `src/reader/block_decode.rs` | compression-codec selection (start uncompressed/bit-pack; reuse integer path) |
| `nodedb/src/engine/timeseries/columnar_segment/codec.rs` | delta/integer codec dispatch (optional; can fall back to raw initially) |

### DDL / schema

| File | Change |
|------|--------|
| `src/control/server/pgwire/ddl/schema_validation.rs` | accept `Int128` in CREATE/ALTER |
| `src/engine/timeseries/schema_evolution.rs` | type-compatibility (e.g. `Int64 → Int128` is a safe widening) |
| `src/engine/timeseries/ts_detect.rs` | auto-detect only on explicit overflow of i64 (avoid silently widening normal ints) |

## Open questions / risks

1. **Arithmetic overflow policy.** `i128 + i128` can still overflow. Decide
   checked (error on overflow) vs. saturating vs. wrapping. Recommend **checked**
   to match SQL exactness expectations; surface as a query error.
2. **`i64 → i128` literal promotion in SQL.** A literal `170141183460469231731687303715884105727`
   exceeds `i64`; the parser/lexer must produce an `Int128` literal rather than
   failing. Check `nodedb-sql` numeric-literal handling — this may be a separate
   sub-task.
3. **Float coercion loss.** `f64 → i128` loses precision beyond 2⁵³. Recommend
   rejecting `Float → Int128` coercion (require explicit cast) rather than silently
   truncating.
4. **Arrow `Decimal128` round-trip.** Arrow carries it as `Decimal128(scale=0)`;
   confirm downstream consumers don't reinterpret scale.
5. **Compression.** Initial impl can store raw 16-byte LE and skip a tuned codec;
   note the perf gap rather than silently shipping uncompressed at scale.

## Test plan

TDD, one failing test per behavior before implementation:

- Round-trip: `encode_fixed`/`decode_fixed_value` for min/max/zero/negative `i128`.
- MessagePack: `Value::Int128` round-trips losslessly (tag 21); JSON path documented-lossy behavior asserted.
- Parse: `INT128` and `HUGEINT` → `ColumnType::Int128`; `Display` → `INT128`.
- DDL: `CREATE TABLE t (n INT128)` then INSERT/SELECT a value > `i64::MAX`.
- pgwire: result-set field OID = 1700; value text-encodes as exact decimal string; psql `SELECT` shows full value.
- Aggregation: SUM/MIN/MAX over `Int128` column, including values > `i64::MAX`.
- Filter: `WHERE n > <big literal>` returns correct rows; BETWEEN/IN.
- Arithmetic: `Int128 + Integer`, overflow → checked error.
- Widening: ALTER `Int64` column → `Int128` preserves values.

## Estimated surface

~6 layers, ~25 files. Mostly mechanical single-arm additions following the
proven `Decimal`/`Uuid` 16-byte template. The genuinely invasive piece is the new
`Value::Int128` variant (many internal exhaustive matches). Low architectural
risk — rides existing rails.

## Follow-ups (out of scope here)

- `UInt128` / `u128` unsigned variant (D1).
- Tuned columnar compression codec for 128-bit integers (D5 / risk 5).
- Explicit `CAST(... AS INT128)` and reverse casts.
