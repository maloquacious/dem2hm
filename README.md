# dem2hm

`dem2hm` will convert a digital elevation model (DEM) into a normalized integer heightmap designed for direct use by Go programs.

The Rust application has been scaffolded, but the converter is not implemented yet.

## Planned input

The initial implementation will read a single-band signed-integer GeoTIFF DEM, including its declared no-data value. The first development fixture is the Panama 30 m DEM in `../dem/Pma_DEM_30m.tif`.

## Heightmap format, version 1

The output is a header followed immediately by one `int32` for every output pixel:

| Byte offset | Type | Meaning |
| ---: | --- | --- |
| 0 | `int32` | Magic value `0x0108AAFF` |
| 4 | `int32` | Height in rows |
| 8 | `int32` | Width in columns |
| 12 | `int32[]` | `height * width` normalized pixels |

Every integer uses **little-endian** byte order. Thus, the first four bytes are `FF AA 08 01`. Both Apple Silicon and common AMD64 computers are little-endian, and Go can decode the format portably with `encoding/binary.LittleEndian`.

Pixel data is **row-major**, with each row written left-to-right from the top of the raster. Pixel `(x, y)` is stored at sample index:

```text
y * width + x
```

Valid elevations are linearly normalized to `0..2147483647`:

```text
round((elevation - min) * 2147483647 / (max - min))
```

The reserved value `-2147483648` marks a no-data pixel. Version 1 has no padding or trailer, so its exact size is:

```text
12 + height * width * 4 bytes
```

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
