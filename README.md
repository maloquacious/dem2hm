# dem2hm

`dem2hm` converts a digital elevation model (DEM) into a normalized integer heightmap designed for direct use by Go programs.

## Usage

Run the application through Cargo from the repository root:

```text
cargo run --release -- [OPTIONS] <INPUT> <OUTPUT>
```

After installing it with `cargo install --path .`, invoke the executable directly:

```text
dem2hm [OPTIONS] <INPUT> <OUTPUT>
```

The input must be a single-image, single-band GeoTIFF with signed 8-, 16-, 32-, or 64-bit integer samples. If the GeoTIFF declares a `GDAL_NODATA` value, those pixels are encoded with the heightmap no-data sentinel and excluded when finding the normalization range.

Arguments:

- `<INPUT>` is the source GeoTIFF DEM.
- `<OUTPUT>` is the gzip-compressed heightmap file to create. Use the `.hmz` extension; its parent directory must already exist.

Options:

- `--rotate <0|90|180|270>` rotates clockwise. The default is `0`.
- `--flip-horizontal` flips the rotated raster left-to-right.
- `--flip-vertical` flips the rotated raster top-to-bottom.
- `-h`, `--help` prints command-line help.
- `-V`, `--version` prints the application version.

Rotation is applied first. Flips are then applied in output coordinates, so they always describe the final image's horizontal and vertical axes.

### Panama DEM

From the repository root, this exact command reads `../dem/Pma_DEM_30m.tif`, rotates it 90 degrees clockwise, flips the rotated image vertically, and writes `../var/pandemokh.hmz`:

```sh
cargo run --release -- --rotate 90 --flip-vertical ../dem/Pma_DEM_30m.tif ../var/pandemokh.hmz
```

The `../var` directory must exist before running the command.

The converter scans the TIFF once to find valid minimum and maximum elevations, then decodes and transforms one TIFF strip or tile at a time into a temporary payload. It finally streams that payload through gzip into the output file. It does not load the complete raster into memory.

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

This repository includes the Go module `github.com/maloquacious/dem2hm`, which will provide the canonical heightmap reader so applications do not need to implement and test their own decoder. The reader is currently incomplete and does not compile; its API and behavior are not stable yet. It will be completed and tested before version 1 is published.

## Reading the header in Go

```go
var magic, height, width int32

if err := binary.Read(r, binary.LittleEndian, &magic); err != nil {
	return err
}
if magic != 0x0108AAFF {
	return fmt.Errorf("invalid heightmap byte order or format: %#x", magic)
}
if err := binary.Read(r, binary.LittleEndian, &height); err != nil {
	return err
}
if err := binary.Read(r, binary.LittleEndian, &width); err != nil {
	return err
}
```

The intended upstream repository is <https://github.com/maloquacious/dem2hm>.

## Authors

- Michael D. Henderson
- [Amp](https://ampcode.com)
