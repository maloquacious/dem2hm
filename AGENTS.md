# dem2hm contributor instructions

## Purpose

Build a Rust command-line program that reads a digital elevation model (DEM), initially a single-band signed-integer GeoTIFF, and writes a compact heightmap that Go programs can load without a GIS dependency.

Keep the conversion deterministic and streaming or bounded-memory where practical. Preserve the DEM's raster orientation unless an explicit CLI option requests a transformation. Treat declared no-data pixels separately from valid elevations.

## Heightmap binary contract

The output format is version 1 and has no padding:

| Byte offset | Type | Meaning |
| ---: | --- | --- |
| 0 | `int32` | Magic value `0x0108AAFF` |
| 4 | `int32` | Raster height (number of rows) |
| 8 | `int32` | Raster width (number of columns) |
| 12 | `height * width` × `int32` | Pixel values |

All integers are **little-endian two's-complement values**. The magic value therefore appears on disk as bytes `FF AA 08 01`. Little-endian is the native byte order of both Apple Silicon/ARM64 Macs and mainstream AMD64 machines, but readers and writers must still request little-endian explicitly rather than relying on host byte order.

Pixels are **row-major**: write the top row from left to right, followed by each successive row. Pixel `(x, y)` is sample index `y * width + x` after the three-word header.

Normalize valid elevations linearly into the inclusive signed-positive range `0..=i32::MAX`:

```text
normalized = round((elevation - minimum_valid_elevation) * i32::MAX
                   / (maximum_valid_elevation - minimum_valid_elevation))
```

Write `i32::MIN` (`-2147483648`) for a declared no-data pixel. This sentinel is outside the valid normalized range. Define and test sensible behavior for a constant-elevation DEM, where the normalization span is zero.

The complete file size must be exactly `12 + height * width * 4` bytes. Do not add row padding, metadata, checksums, or trailers to version 1. Reject dimensions that cannot be represented by positive `int32` values or whose size arithmetic overflows.

## Implementation expectations

- Use the repository's current stable Rust edition and standard formatting.
- Keep format encoding separate from DEM decoding so the writer can be tested with small in-memory rasters.
- Validate that the input has one supported elevation band and report unsupported sample formats clearly.
- Use checked integer arithmetic for dimensions, offsets, and normalization intermediates. Use a sufficiently wide intermediate representation to avoid overflow.
- Write tests that inspect exact bytes for the header, asymmetric raster dimensions, row-major ordering, normalization endpoints and rounding, no-data handling, malformed input, and overflow boundaries.
- Document the CLI and exact binary format in `README.md`; update both this file and the README if the format changes.
- Before finishing a change, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.

The intended upstream repository is `https://github.com/maloquacious/dem2hm.git`.
