# dem2hm contributor instructions

## Purpose

Build a Rust command-line program that reads a digital elevation model (DEM), initially a single-band signed-integer GeoTIFF, and writes a compact heightmap that programs can load without a GIS dependency. Maintain a Go module in this repository that provides the canonical reader so downstream teams do not need to implement and test their own.

Keep the conversion deterministic and streaming or bounded-memory where practical. Preserve the DEM's raster orientation unless an explicit CLI option requests a transformation. Treat declared no-data pixels separately from valid elevations.

## Heightmap binary contract, version 1

Version 1 is **frozen**. Writers and readers must preserve this contract. Any incompatible future change requires a new format version and a new magic value; never reinterpret a version 1 file under a changed contract.

Version 1 has a raw header followed by a gzip-compressed payload:

| Byte offset | Type | Meaning |
| ---: | --- | --- |
| 0 | `int32` | Magic value `0x0108AAFF` |
| 4 | `int32` | Raster height (number of rows) |
| 8 | `int32` | Raster width (number of columns) |
| 12 | gzip stream | `height * width` × `int32` pixel values after decompression |

All integers in the raw header and decompressed payload are **little-endian two's-complement values**. The magic value therefore appears on disk as bytes `FF AA 08 01`. The gzip stream begins at byte 12 with bytes `1F 8B`. Little-endian is the native byte order of both Apple Silicon/ARM64 Macs and mainstream AMD64 machines, but readers and writers must still request little-endian explicitly rather than relying on host byte order.

Decompressed pixels are **row-major**: write the top row from left to right, followed by each successive row. Pixel `(x, y)` is decompressed sample index `y * width + x`.

Normalize valid elevations linearly into the inclusive signed-positive range `0..=i32::MAX`:

```text
normalized = round((elevation - minimum_valid_elevation) * i32::MAX
                   / (maximum_valid_elevation - minimum_valid_elevation))
```

Write `i32::MIN` (`-2147483648`) for a declared no-data pixel. This sentinel is outside the valid normalized range. Define and test sensible behavior for a constant-elevation DEM, where the normalization span is zero.

The decompressed payload size must be exactly `height * width * 4` bytes, with no row padding, metadata, or trailing decompressed data. The complete file size varies with compression. Use one gzip member with a zero modification time so output is deterministic; its standard CRC and size trailer are required. Reject dimensions that cannot be represented by positive `int32` values or whose decompressed size arithmetic overflows.

## Heightmap binary contract, version 1.1

Version 1.1 stores unscaled elevations in meters with georeference and provenance metadata. It is the converter's default output. It has its own magic value and does not change version 1. Treat this contract as frozen once released: any incompatible change requires a new version and magic value.

| Byte offset | Type | Meaning |
| ---: | --- | --- |
| 0 | `int32` | Magic value `0x0108AAFE` (bytes `FE AA 08 01`) |
| 4 | `int32` | Metadata length `n`, in `1..=1048576` bytes |
| 8 | UTF-8 JSON | Metadata object of exactly `n` bytes |
| 8 + `n` | gzip stream | `height * width` × `int16` elevations after decompression |

- Integers are little-endian two's-complement. Pixels are row-major, as in version 1. The decompressed payload is exactly `height * width * 2` bytes, in one gzip member with a zero modification time.
- Valid elevations are source values in `-32767..=32767` meters. Reject a source whose valid values do not fit, including a valid `-32768`; never clamp.
- Write `i16::MIN` (`-32768`) for a declared no-data pixel. It means "outside the source data", not "sea".
- The metadata object carries `height`, `width`, `elevation` (`minimum`, `maximum`, `vertical_datum`), `pixel_size_m`, `source` (`file_name`, `sha256`, `width`, `height`, `no_data`, `geotransform`), `transform` (`rotate`, `flip_horizontal`, `flip_vertical`) and `dem2hm_version`. The README defines each key. Write keys in that order in compact form so output is deterministic. Readers must ignore unknown keys; adding a key is compatible, while removing or redefining one is not.
- `source.geotransform` is the GDAL six-value affine transform from the top-left corner of a source pixel to longitude and latitude in degrees. Only geographic, unrotated GeoTIFF georeferences are supported.
- `pixel_size_m` is the north–south ground size of one pixel at the source's center latitude on the WGS 84 ellipsoid.

## Implementation expectations

- Use the repository's current stable Rust edition and standard formatting.
- Keep format encoding separate from DEM decoding so the writer can be tested with small in-memory rasters.
- Validate that the input has one supported elevation band and report unsupported sample formats clearly.
- Use checked integer arithmetic for dimensions, offsets, and normalization intermediates. Use a sufficiently wide intermediate representation to avoid overflow.
- Write tests that inspect exact bytes for the header, asymmetric raster dimensions, row-major ordering, normalization endpoints and rounding, no-data handling, malformed input, and overflow boundaries.
- Keep the Go readers aligned with both binary contracts above. Their tests must cover valid files, malformed headers, metadata and dimensions, truncated or oversized payloads, row-major indexing, and the no-data sentinel.
- Keep the semantic version in `version.go` synchronized with the Rust package version in `Cargo.toml`. Verify this before every code commit, and include both files in any commit that changes the version.
- Document the CLI and exact binary format in `README.md`; update both this file and the README if the format changes.
- Before finishing a change, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.
- Run `gofmt` on changed Go files and `go test ./...` before finishing changes to the Go module.

## GitHub workflow

- Commit routine work directly to `main` unless the work is associated with a GitHub issue.
- For issue-driven work, assign the issue to `@me`, create a branch, and open a pull request instead of committing directly to `main`.
- Assign every pull request to `@me`.
- Reference the relevant issue number in commit messages for issue-driven work.
- You are authorized to push commits and issue branches to the remote after all required tests pass. Do not push when any required test is failing.

The intended upstream repository is `https://github.com/maloquacious/dem2hm.git`.
