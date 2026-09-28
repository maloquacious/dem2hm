// Copyright (c) 2026 Michael D Henderson. All rights reserved.

package dem2hm

import (
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"unicode/utf8"
)

const (
	magic16Value = 0x0108_AAFE
	header16Size = 8

	// MaxMetadataSize is the largest version 1.1 JSON metadata block in bytes.
	MaxMetadataSize = 1 << 20

	// NoDataPixel16 marks a version 1.1 pixel outside the source data. It
	// does not imply sea; the application decides what such a pixel means.
	NoDataPixel16 int16 = -32768
)

// HeightMap16 is a raster of elevations in meters, stored in top-to-bottom
// row-major order, with the metadata recorded by the converter.
type HeightMap16 struct {
	Height   int
	Width    int
	Metadata Metadata
	Data     []int16
}

// Metadata is the version 1.1 JSON metadata block.
type Metadata struct {
	// Height and Width are the output raster dimensions.
	Height int32 `json:"height"`
	Width  int32 `json:"width"`
	// Elevation summarizes the valid elevations.
	Elevation ElevationMetadata `json:"elevation"`
	// PixelSizeMeters is the nominal north–south ground size of one pixel at
	// the source raster's center latitude on the WGS 84 ellipsoid.
	PixelSizeMeters float64 `json:"pixel_size_m"`
	// Source describes the GeoTIFF the heightmap was converted from.
	Source SourceMetadata `json:"source"`
	// Transform is the rotation and flips applied to the source raster.
	Transform TransformMetadata `json:"transform"`
	// Dem2hmVersion is the version of the converter that wrote the file.
	Dem2hmVersion string `json:"dem2hm_version"`
}

// ElevationMetadata summarizes the valid elevations in meters.
type ElevationMetadata struct {
	Minimum int16 `json:"minimum"`
	Maximum int16 `json:"maximum"`
	// VerticalDatum is the datum elevations are measured from, such as
	// "EGM96" or "EPSG:5773", or nil when unknown.
	VerticalDatum *string `json:"vertical_datum"`
}

// SourceMetadata describes the source GeoTIFF.
type SourceMetadata struct {
	FileName string `json:"file_name"`
	SHA256   string `json:"sha256"`
	Width    int32  `json:"width"`
	Height   int32  `json:"height"`
	// NoData is the source's declared no-data value, or nil if it had none.
	NoData *int64 `json:"no_data"`
	// GeoTransform maps the top-left corner of source pixel (column, row) to
	// degrees: longitude = g[0] + column*g[1] + row*g[2] and
	// latitude = g[3] + column*g[4] + row*g[5].
	GeoTransform [6]float64 `json:"geotransform"`
}

// TransformMetadata is the transformation from source to output pixels:
// a clockwise rotation, followed by flips in output coordinates.
type TransformMetadata struct {
	Rotate         int  `json:"rotate"`
	FlipHorizontal bool `json:"flip_horizontal"`
	FlipVertical   bool `json:"flip_vertical"`
}

// At returns the elevation at column x and row y. It panics if the
// coordinates are outside the heightmap.
func (hm *HeightMap16) At(x, y int) int16 {
	if hm == nil || x < 0 || y < 0 || x >= hm.Width || y >= hm.Height {
		panic(fmt.Sprintf("dem2hm: pixel (%d, %d) is outside the heightmap", x, y))
	}
	return hm.Data[y*hm.Width+x]
}

// Pixel returns the elevation at column x and row y.
func (hm *HeightMap16) Pixel(x, y int) (int16, error) {
	if hm == nil {
		return 0, fmt.Errorf("pixel (%d, %d): %w", x, y, ErrOutOfBounds)
	}
	index, err := pixelIndex(hm.Height, hm.Width, len(hm.Data), x, y)
	if err != nil {
		return 0, err
	}
	return hm.Data[index], nil
}

// Row returns row y as a view into the heightmap's backing data. Changes to
// the returned slice change the heightmap.
func (hm *HeightMap16) Row(y int) ([]int16, error) {
	if hm == nil {
		return nil, fmt.Errorf("row %d: %w", y, ErrOutOfBounds)
	}
	start, end, err := rowRange(hm.Height, hm.Width, len(hm.Data), y)
	if err != nil {
		return nil, err
	}
	return hm.Data[start:end], nil
}

// SourcePixel returns the source GeoTIFF column and row of output pixel (x, y).
func (hm *HeightMap16) SourcePixel(x, y int) (int, int, error) {
	if hm == nil || x < 0 || y < 0 || x >= hm.Width || y >= hm.Height {
		return 0, 0, fmt.Errorf("pixel (%d, %d): %w", x, y, ErrOutOfBounds)
	}
	transform := hm.Metadata.Transform
	if transform.FlipHorizontal {
		x = hm.Width - 1 - x
	}
	if transform.FlipVertical {
		y = hm.Height - 1 - y
	}
	width, height := int(hm.Metadata.Source.Width), int(hm.Metadata.Source.Height)
	switch transform.Rotate {
	case 0:
		return x, y, nil
	case 90:
		return y, height - 1 - x, nil
	case 180:
		return width - 1 - x, height - 1 - y, nil
	case 270:
		return width - 1 - y, x, nil
	}
	return 0, 0, fmt.Errorf("rotation %d: %w", transform.Rotate, ErrInvalidMetadata)
}

// LonLat returns the longitude and latitude in degrees of the center of
// output pixel (x, y).
func (hm *HeightMap16) LonLat(x, y int) (float64, float64, error) {
	column, row, err := hm.SourcePixel(x, y)
	if err != nil {
		return 0, 0, err
	}
	g := hm.Metadata.Source.GeoTransform
	u, v := float64(column)+0.5, float64(row)+0.5
	return g[0] + u*g[1] + v*g[2], g[3] + u*g[4] + v*g[5], nil
}

// ReadHeightMap16 reads a version 1.1 heightmap from r without an
// application-defined resource limit or cancellation context.
func ReadHeightMap16(r io.Reader) (*HeightMap16, error) {
	return ReadHeightMap16WithOptions(r, Options{})
}

// ReadHeightMap16WithOptions reads a version 1.1 heightmap from r using options.
func ReadHeightMap16WithOptions(r io.Reader, options Options) (*HeightMap16, error) {
	ctx, r, err := startRead(r, options)
	if err != nil {
		return nil, err
	}

	var header [header16Size]byte
	if err := readHeader(r, header[:]); err != nil {
		return nil, err
	}
	magic := int32(binary.LittleEndian.Uint32(header[0:4]))
	if magic != magic16Value {
		return nil, fmt.Errorf("got magic %#08x, want %#08x: %w", uint32(magic), uint32(magic16Value), ErrInvalidByteOrder)
	}
	length := int32(binary.LittleEndian.Uint32(header[4:8]))
	if length <= 0 || length > MaxMetadataSize {
		return nil, fmt.Errorf("metadata length %d is outside 1..%d: %w", length, MaxMetadataSize, ErrInvalidHeader)
	}

	block := make([]byte, length)
	if n, err := io.ReadFull(r, block); err != nil {
		if canceled := contextError(err); canceled != nil {
			return nil, canceled
		}
		return nil, fmt.Errorf("read %d of %d metadata bytes: %w: %w", n, length, ErrInvalidMetadata, err)
	}
	metadata, err := decodeMetadata(block)
	if err != nil {
		return nil, err
	}
	pixelCount, err := checkedPixelCount(metadata.Height, metadata.Width, 2, options.MaxPixels)
	if err != nil {
		return nil, err
	}

	hm := &HeightMap16{
		Height:   int(metadata.Height),
		Width:    int(metadata.Width),
		Metadata: metadata,
		Data:     make([]int16, pixelCount),
	}
	err = readPayload(ctx, r, header16Size+int(length), pixelCount, 2, func(first int, batch []byte) error {
		for offset := range len(batch) / 2 {
			hm.Data[first+offset] = int16(binary.LittleEndian.Uint16(batch[offset*2 : offset*2+2]))
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return hm, nil
}

// decodeMetadata decodes and validates the JSON metadata block. Unknown
// fields are ignored so later writers can add metadata.
func decodeMetadata(block []byte) (Metadata, error) {
	var metadata Metadata
	if !utf8.Valid(block) {
		return metadata, fmt.Errorf("metadata is not valid UTF-8: %w", ErrInvalidMetadata)
	}
	if err := json.Unmarshal(block, &metadata); err != nil {
		return metadata, fmt.Errorf("%w: %w", ErrInvalidMetadata, err)
	}
	if metadata.Height <= 0 || metadata.Width <= 0 {
		return metadata, fmt.Errorf("height %d and width %d must both be positive: %w", metadata.Height, metadata.Width, ErrInvalidDimensions)
	}
	transform := metadata.Transform
	sourceWidth, sourceHeight := metadata.Source.Width, metadata.Source.Height
	switch transform.Rotate {
	case 0, 180:
	case 90, 270:
		sourceWidth, sourceHeight = sourceHeight, sourceWidth
	default:
		return metadata, fmt.Errorf("rotation %d is not 0, 90, 180 or 270: %w", transform.Rotate, ErrInvalidMetadata)
	}
	if sourceWidth != metadata.Width || sourceHeight != metadata.Height {
		return metadata, fmt.Errorf("source %dx%d rotated %d does not produce %dx%d: %w",
			metadata.Source.Width, metadata.Source.Height, transform.Rotate, metadata.Width, metadata.Height, ErrInvalidMetadata)
	}
	if metadata.Elevation.Minimum > metadata.Elevation.Maximum {
		return metadata, fmt.Errorf("elevation minimum %d exceeds maximum %d: %w", metadata.Elevation.Minimum, metadata.Elevation.Maximum, ErrInvalidMetadata)
	}
	return metadata, nil
}
