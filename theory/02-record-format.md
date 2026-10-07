# Subproject 2 — Record Format

A page knows how to store *cells*. A cell's payload, so far, has just been
"some bytes." This subproject defines what those bytes mean: a **record** —
the on-disk encoding of one logical row's column values.

## The problem a record format solves

Your table has rows with typed columns: maybe `(id: INTEGER, name: TEXT,
score: FLOAT)`. You need a byte encoding that:

1. Can represent several different types in one contiguous blob.
2. Lets you read back *just* column 2 without necessarily decoding columns
   0 and 1 first (or at least, lets you skip over them cheaply).
3. Handles `NULL` without ambiguity.
4. Doesn't waste space — a `NULL` or a small integer shouldn't cost as much
   as a long string.

SQLite's answer (and the one `FILE.md`/your plan adopts) is a **header +
body** split with **manifest typing**: every value's type is recorded
per-row, per-column (not fixed by a rigid schema at the byte level), using a
small "serial type" code.

## Layout

```
Record bytes:
┌─────────────────────────────┬───────────────────────────────────┐
│          HEADER              │               BODY                 │
├───────────────┬──────────────┼──────────────┬──────────────┬─────┤
│ header length │ serial type  │ value 0 bytes│ value 1 bytes│ ... │
│   (varint)    │  varints...  │              │              │     │
└───────────────┴──────────────┴──────────────┴──────────────┴─────┘
```

A **serial type** is a small integer code that says both "what type is
this" and, implicitly, "how many bytes does it occupy in the body" — e.g.
code `0` = NULL (0 bytes), code `1` = 8-bit signed int (1 byte), code `4` =
big-endian IEEE754 double (8 bytes), code `13` = text of length N (N bytes,
with N derived from the serial type's encoding for strings/blobs). Because
each code implies its own width, decoding the body is just "walk the serial
type list, and for each one, consume exactly that many bytes next."

```mermaid
flowchart LR
    A["header length (varint)"] --> B["serial type[0] (varint)"]
    B --> C["serial type[1] (varint)"]
    C --> D["... serial type[n-1]"]
    D --> E["value[0] bytes"]
    E --> F["value[1] bytes"]
    F --> G["... value[n-1] bytes"]
```

This is why the header is separated from the body at all: to read column 2's
*value*, you still walk the (cheap, small) header to find out where column
2's bytes start in the body — you don't need a separate "offset table,"
because the serial types already tell you each preceding value's width.

## Why "manifest typing" instead of a rigid C-struct-like layout

A more naive design: fix each column's byte width from the `CREATE TABLE`
schema (like a C struct), and just concatenate values at known fixed
offsets. SQLite (and this project) doesn't do that, because:

- It would waste space for every row where a value is smaller than its
  column's worst case (e.g. a `TEXT` column has no fixed width at all).
- It can't represent `NULL` without a separate bitmap or sentinel value.
- It makes the on-disk format load-bearing on the schema never changing
  width — adding a column later becomes much harder.

The cost: you must consult the record's own header to interpret its body;
you can't treat records as a flat C array of columns. That's a fair trade
for a toy (and real) database engine.

## Rust concepts you'll lean on

### Enums as tagged unions (`Value`)

This is *the* natural fit for "a column holds one of several possible
types":

```rust
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}
```

Unlike a C `union`, Rust's `enum` always knows (at runtime, cheaply, via a
hidden discriminant tag) which variant it currently holds — there's no way
to accidentally read a `Float` as `Text`. `match` is exhaustive: if you add
a new `Value` variant later, every `match` on `Value` in your codebase will
fail to compile until you handle it. That's a feature, not friction — it's
the compiler finding every place you need to update.

```rust
fn describe(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Text(_) => "text",
        Value::Blob(_) => "blob",
        // if you add Value::Bool later, this won't compile until you add a case here
    }
}
```

### A worked generic example: TLV (type-length-value) encoding

The record format is a specific instance of a very general pattern: encode
a dynamically-typed value as **(tag, optional length, bytes)**. Here's a
toy, unrelated version of the same idea — a tiny self-describing message
format — to internalize the technique before you build the real one:

```rust
#[derive(Debug, PartialEq)]
enum Msg {
    Ping,
    Code(i32),
    Text(String),
}

fn encode(msg: &Msg) -> Vec<u8> {
    let mut out = Vec::new();
    match msg {
        Msg::Ping => out.push(0), // tag 0, no payload
        Msg::Code(n) => {
            out.push(1); // tag 1
            out.extend_from_slice(&n.to_be_bytes()); // fixed 4-byte payload
        }
        Msg::Text(s) => {
            out.push(2); // tag 2
            let bytes = s.as_bytes();
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes()); // length prefix
            out.extend_from_slice(bytes); // variable payload
        }
    }
    out
}

fn decode(buf: &[u8]) -> anyhow::Result<(Msg, usize)> {
    match buf[0] {
        0 => Ok((Msg::Ping, 1)),
        1 => {
            let n = i32::from_be_bytes(buf[1..5].try_into()?);
            Ok((Msg::Code(n), 5))
        }
        2 => {
            let len = u32::from_be_bytes(buf[1..5].try_into()?) as usize;
            let s = String::from_utf8(buf[5..5 + len].to_vec())?;
            Ok((Msg::Text(s), 5 + len))
        }
        tag => anyhow::bail!("unknown tag {tag}"),
    }
}

#[test]
fn round_trips() {
    for msg in [Msg::Ping, Msg::Code(-7), Msg::Text("hi".into())] {
        let bytes = encode(&msg);
        let (decoded, consumed) = decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(consumed, bytes.len());
    }
}
```

Your real record format differs in two ways worth noticing in advance: (1)
the "tag" (serial type) and "length" are unified — a text/blob's serial type
*encodes* its length directly rather than using a separate fixed-width
length field, and (2) all the tags for a row are grouped into one header up
front rather than interleaved with each value's bytes. Both are just
variations on the same TLV idea, optimized for "many values in one record."

### `String` vs `&str`, and why records decode into owned `String`

`&str` borrows from some existing buffer; `String` owns its heap allocation.
When you decode a `Text` value out of a page's byte buffer, you generally
want an owned `String` (via `String::from_utf8(bytes.to_vec())` or
`str::to_owned()`), because the decoded `Value` needs to outlive the
temporary byte slice you decoded it from — especially once the pager (next
subproject) might evict/overwrite that buffer later. This is a case where
copying is the *correct* choice, not a performance mistake.

### `TryFrom` for your serial-type codes, again

Just like `PageType` in Subproject 1, a serial type code (a small integer)
mapping to "what does this mean and how wide is it" is a textbook
`TryFrom<u64>` (decoding) + `From`/explicit method (encoding) pair. Keep
that symmetry: whatever enum you use for serial types, give it one function
that goes code → meaning and one that goes meaning → code, and test them as
inverses of each other, the same way you tested varint encode/decode.

## What "decoupling payload from page" buys you later

Once records exist as their own concept, a `TableLeafCell`'s payload is just
`&[u8]` that *happens* to decode as a `Record`. This matters directly for
Subproject 7 (overflow pages): a payload that spans multiple pages is still
just bytes once reassembled — the record decoder doesn't need to know or
care that the bytes were physically split across pages to get here.
