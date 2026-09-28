# dem2hm

`dem2hm` converts a digital elevation model (DEM) into a compact integer heightmap designed for direct use by Go programs. By default it writes format version 1.1: elevations in meters with the source georeference and provenance. The frozen version 1 format of normalized pixels remains available.

## Usage

Run the application through Cargo from the repository root:

```text
cargo run --release -- [OPTIONS] <INPUT> <OUTPUT>
```

After installing it with `cargo install --path .`, invoke the executable directly:

```text
dem2hm [OPTIONS] <INPUT> <OUTPUT>
```

The input must be a single-image, single-band GeoTIFF with signed 8-, 16-, 32-, or 64-bit integer samples. If the GeoTIFF declares a `GDAL_NODATA` value, those pixels are encoded with the heightmap no-data sentinel and excluded when finding the elevation range.

Format 1.1 also requires:

- a geographic (longitude and latitude in degrees) GeoTIFF georeference, given by one `ModelTiepoint` plus `ModelPixelScale`, or by an unrotated `ModelTransformation`. Projected rasters, rotated rasters and world files are not supported.
- every valid elevation to fit in `-32767..=32767` meters. `-32768` is reserved for no-data, so a source value that doesn't fit, including a valid `-32768`, is rejected rather than clamped. A declared no-data value may be any integer.

Arguments:

- `<INPUT>` is the source GeoTIFF DEM.
- `<OUTPUT>` is the gzip-compressed heightmap file to create. Use the `.hmz` extension; its parent directory must already exist.

Options:

- `--format <1|1.1>` selects the heightmap format. The default is `1.1`.
- `--rotate <0|90|180|270>` rotates clockwise. The default is `0`.
- `--flip-horizontal` flips the rotated raster left-to-right.
- `--flip-vertical` flips the rotated raster top-to-bottom.
- `--vertical-datum <NAME>` records the vertical datum, for example `EGM96`, when the GeoTIFF keys do not declare one. A datum declared in the GeoTIFF takes precedence. Format 1.1 only.
- `-h`, `--help` prints command-line help.
- `-V`, `--version` prints the application version.

Rotation is applied first. Flips are then applied in output coordinates, so they always describe the final image's horizontal and vertical axes.

### Panama DEM

From the repository root, this exact command reads `../dem/Pma_DEM_30m.tif`, rotates it 90 degrees clockwise and writes `../var/pandemokh.hmz`. The ALOS AW3D30 source measures heights above the EGM96 geoid but does not declare it in its GeoTIFF keys, so the command records it explicitly:

```sh
cargo run --release -- --rotate 90 --vertical-datum EGM96 ../dem/Pma_DEM_30m.tif ../var/pandemokh.hmz
```

The `../var` directory must exist before running the command.

The converter scans the TIFF once to find valid minimum and maximum elevations, hashes the input file, then decodes and transforms one TIFF strip or tile at a time into a temporary payload. It finally streams that payload through gzip into the output file. It does not load the complete raster into memory.

## Heightmap format, version 1.1

Version 1.1 stores elevations in meters with the metadata needed to place them on the Earth. It is a separate format with its own magic value; it does not change how version 1 files are read.

The output is an 8-byte raw header, a UTF-8 JSON metadata block, and one gzip member containing an `int16` for every output pixel:

| Byte offset | Type | Meaning |
| ---: | --- | --- |
| 0 | `int32` | Magic value `0x0108AAFE` |
| 4 | `int32` | Metadata length `n` in bytes, `1..=1048576` |
| 8 | UTF-8 JSON | Metadata object, exactly `n` bytes |
| 8 + `n` | gzip stream | `height * width` `int16` elevations in meters |

Every integer in the raw header and decompressed payload uses **little-endian** two's-complement byte order, so the file starts with bytes `FE AA 08 01`. The gzip stream starts immediately after the metadata with bytes `1F 8B`. Pixel data is row-major, as in version 1: pixel `(x, y)` is sample `y * width + x`. The decompressed payload is exactly `height * width * 2` bytes.

Elevations are the source values, unscaled. Sea level is `0`, so a pixel is above sea level when its value is `> 0`. The reserved value `-32768` marks a no-data pixel: one **outside the source data**. It does not mean sea. A DEM cut to a political border uses no-data for neighboring land too, so each application decides whether no-data is sea, off-map land or something else.

The metadata is one JSON object. The converter writes keys in the order shown, in compact form, making output deterministic. Readers must ignore unknown keys, so later writers can add metadata.

```json
{
  "height": 21307,
  "width": 8868,
  "elevation": {"minimum": -37, "maximum": 3431, "vertical_datum": "EGM96"},
  "pixel_size_m": 30.72168914643369,
  "source": {
    "file_name": "Pma_DEM_30m.tif",
    "sha256": "e00cccc404d88c1f5bd2a57b5e4d78959c9c789173193c475a9e3152a091f42c",
    "width": 21307,
    "height": 8868,
    "no_data": 32767,
    "geotransform": [-83.05972222222222, 0.00027777777777777745, 0.0, 9.650555555555552, 0.0, -0.00027777777777777745]
  },
  "transform": {"rotate": 90, "flip_horizontal": false, "flip_vertical": false},
  "dem2hm_version": "1.1.0"
}
```

| Key | Meaning |
| --- | --- |
| `height`, `width` | Output raster dimensions; positive `int32` values. |
| `elevation.minimum`, `elevation.maximum` | Range of valid elevations in meters. |
| `elevation.vertical_datum` | The GeoTIFF's vertical coordinate system as `EPSG:<code>` or its citation, else the `--vertical-datum` value, else `null`. |
| `pixel_size_m` | Nominal ground size of one pixel in meters: the north–south size at the source's center latitude on the WGS 84 ellipsoid. East–west size is smaller by the cosine of the latitude. |
| `source.file_name`, `source.sha256` | Base name and lowercase hex SHA-256 of the source file. |
| `source.width`, `source.height` | Source raster dimensions. |
| `source.no_data` | Source `GDAL_NODATA` value, or `null` if none was declared. |
| `source.geotransform` | GDAL-style affine transform from the top-left **corner** of source pixel `(column, row)` to degrees: `lon = g[0] + column * g[1] + row * g[2]`, `lat = g[3] + column * g[4] + row * g[5]`. A `PixelIsPoint` GeoTIFF is shifted by half a pixel to this convention. |
| `transform.rotate` | Clockwise rotation in degrees: `0`, `90`, `180` or `270`. |
| `transform.flip_horizontal`, `transform.flip_vertical` | Flips applied after rotation, in output coordinates. |
| `dem2hm_version` | Version of the converter that wrote the file. |

To map output pixel `(x, y)` to the source, undo the flips (`x = width - 1 - x`, `y = height - 1 - y`), then the rotation, with `W` and `H` the source width and height:

| `rotate` | Source column | Source row |
| ---: | --- | --- |
| 0 | `x` | `y` |
| 90 | `y` | `H - 1 - x` |
| 180 | `W - 1 - x` | `H - 1 - y` |
| 270 | `W - 1 - y` | `x` |

Apply the geotransform to `column + 0.5`, `row + 0.5` for the pixel's center.

## Heightmap format, version 1 (frozen)

The version 1 wire format is frozen. Existing version 1 files will not be reinterpreted if the format evolves; an incompatible future format will use a new version and magic value.

The output is a raw 12-byte header followed immediately by one gzip member containing an `int32` for every output pixel:

| Byte offset | Type | Meaning |
| ---: | --- | --- |
| 0 | `int32` | Magic value `0x0108AAFF` |
| 4 | `int32` | Height in rows |
| 8 | `int32` | Width in columns |
| 12 | gzip stream | `height * width` normalized `int32` pixels |

Every integer in both the raw header and decompressed payload uses **little-endian** byte order. Thus, the first four bytes are `FF AA 08 01`. The gzip stream starts at byte 12 with bytes `1F 8B`. Go can decode the payload with its standard `compress/gzip` package and decode the integers portably with `encoding/binary.LittleEndian`.

Pixel data is **row-major**, with each row written left-to-right from the top of the raster. Pixel `(x, y)` is stored at sample index:

```text
y * width + x
```

Valid elevations are linearly normalized to `0..2147483647`:

```text
round((elevation - min) * 2147483647 / (max - min))
```

The reserved value `-2147483648` marks a no-data pixel. The decompressed gzip payload has no padding or trailer, so its exact size is:

```text
height * width * 4 bytes
```

The complete file size depends on the gzip compression ratio. The gzip header uses a zero modification time, making repeated conversions of identical input deterministic.

If every valid pixel has the same elevation, all valid pixels are written as `0` because the normalization span is zero.

## Go module

This repository includes the Go module `github.com/maloquacious/dem2hm`, which provides the canonical heightmap reader so applications do not need to implement and test their own decoder.

- `ReadHeightMap16` reads a version 1.1 stream and returns a `*HeightMap16`: elevations in a flat row-major `[]int16` and the decoded `Metadata`.
- `ReadHeightMap` reads a version 1 stream and returns a `*HeightMap` of normalized pixels in a flat row-major `[]int32`. `Read` forwards to it.

Each reader validates the complete stream and rejects the other version's magic value. Both types provide the same accessors: `Pixel` gives bounds-checked access with an error, `At` is a fast path that panics on out-of-bounds coordinates, and `Row` returns a bounds-checked view into the heightmap's backing data. `HeightMap16` also provides `SourcePixel`, which maps an output pixel to its source column and row, and `LonLat`, which returns the longitude and latitude of its center.

## Reading a heightmap in Go

```go
heightmap, err := dem2hm.ReadHeightMap16(r)
if err != nil {
	return err
}
elevation, err := heightmap.Pixel(x, y)
if err != nil {
	return err
}
switch {
case elevation == dem2hm.NoDataPixel16:
	// Outside the source data: the application decides what this means.
case elevation > 0:
	// Above sea level.
}
lon, lat, err := heightmap.LonLat(x, y)
```

Malformed-input and bounds errors wrap exported constant errors, allowing callers to use `errors.Is` while retaining a detailed error message.

### Malicious inputs

`compress/gzip` validates stream structure, CRC, and decompressed size, but it does not impose decompressed-size, compression-ratio, CPU-time, or elapsed-time limits. The readers validate dimensions and read exactly the declared payload, but their unrestricted APIs assume the input is trusted because declared dimensions determine the final pixel allocation. A version 1.1 metadata block is limited to 1 MiB (`MaxMetadataSize`).

Applications accepting untrusted files should call `ReadHeightMap16WithOptions` or `ReadHeightMapWithOptions`, set `MaxPixels` to an application-appropriate allocation ceiling, and pass the request or operation context:

```go
heightmap, err := dem2hm.ReadHeightMap16WithOptions(r, dem2hm.Options{
	Context:   req.Context(),
	MaxPixels: 100_000_000,
})
```

The context is checked during header, metadata and compressed-stream reads. As with all APIs built on `io.Reader`, cancellation cannot interrupt a reader already blocked inside its `Read` method unless that source also supports cancellation, deadlines, or closure. HTTP handlers should use the request context, wrap the request body with `http.MaxBytesReader` to cap compressed input, and retain the server's normal timeout limits. Non-HTTP callers should apply an equivalent input-byte limit when the source is untrusted.

## Input trust

The Rust converter is intended only for trusted TIFF inputs. It validates the TIFF structure and supported sample format, but it is not a sandbox and does not impose security-oriented file-size, memory, CPU, or elapsed-time limits. Do not expose the converter directly to untrusted uploads without enforcing those limits outside the process.

The intended upstream repository is <https://github.com/maloquacious/dem2hm>.

## Authors

- Michael D. Henderson
- [Amp](https://ampcode.com)
