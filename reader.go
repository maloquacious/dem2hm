// Copyright (c) 2026 Michael D Henderson. All rights reserved.

// Package dem2hm reads normalized heightmaps produced by the dem2hm converter.
package dem2hm

import (
	"bufio"
	"compress/gzip"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
)

const (
	magicValue       = 0x0108_AAFF
	headerSize       = 12
	decodeBufferSize = 32 * 1024

	// NoDataPixel is the value reserved for a pixel with no elevation data.
	NoDataPixel int32 = -2147483648

	// ErrInvalidByteOrder indicates that the file does not start with the
	// version 1 magic value in little-endian byte order.
	ErrInvalidByteOrder = Error("invalid heightmap byte order or format")
	// ErrInvalidHeader indicates that the 12-byte heightmap header is incomplete.
	ErrInvalidHeader = Error("invalid heightmap header")
	// ErrInvalidDimensions indicates that the height or width is not positive,
	// or that the resulting payload size cannot be represented safely.
	ErrInvalidDimensions = Error("invalid heightmap dimensions")
	// ErrInvalidGzip indicates that the payload is not a valid gzip stream.
	ErrInvalidGzip = Error("invalid heightmap gzip stream")
	// ErrInvalidPayload indicates that the decompressed pixel payload does not
	// conform to the version 1 heightmap format.
	ErrInvalidPayload = Error("invalid heightmap payload")
	// ErrMultipleGzipMembers indicates that a second gzip member follows the
	// required payload member.
	ErrMultipleGzipMembers = Error("multiple gzip members")
	// ErrTrailingData indicates that data follows the single gzip member.
	ErrTrailingData = Error("trailing heightmap data")
	// ErrOutOfBounds indicates that a requested pixel or row is outside the map.
	ErrOutOfBounds = Error("heightmap coordinates out of bounds")
)

// HeightMap is a normalized raster stored in top-to-bottom row-major order.
type HeightMap struct {
	Height int
	Width  int
	Data   []int32
}

// At returns the pixel at column x and row y. It panics if the coordinates
// are outside the heightmap.
func (hm *HeightMap) At(x, y int) int32 {
	if hm == nil || x < 0 || y < 0 || x >= hm.Width || y >= hm.Height {
		panic(fmt.Sprintf("dem2hm: pixel (%d, %d) is outside the heightmap", x, y))
	}
	return hm.Data[y*hm.Width+x]
}

// Pixel returns the pixel at column x and row y.
func (hm *HeightMap) Pixel(x, y int) (int32, error) {
	if hm == nil || x < 0 || y < 0 || x >= hm.Width || y >= hm.Height {
		return 0, fmt.Errorf("pixel (%d, %d): %w", x, y, ErrOutOfBounds)
	}
	if y > int(^uint(0)>>1)/hm.Width {
		return 0, fmt.Errorf("pixel (%d, %d): dimensions overflow: %w", x, y, ErrOutOfBounds)
	}
	index := y*hm.Width + x
	if index < 0 || index >= len(hm.Data) {
		return 0, fmt.Errorf("pixel (%d, %d): backing data has %d pixels: %w", x, y, len(hm.Data), ErrOutOfBounds)
	}
	return hm.Data[index], nil
}

// Row returns row y as a view into the heightmap's backing data. Changes to
// the returned slice change the heightmap.
func (hm *HeightMap) Row(y int) ([]int32, error) {
	if hm == nil || y < 0 || y >= hm.Height {
		return nil, fmt.Errorf("row %d: %w", y, ErrOutOfBounds)
	}
	if y > int(^uint(0)>>1)/hm.Width {
		return nil, fmt.Errorf("row %d: dimensions overflow: %w", y, ErrOutOfBounds)
	}
	start := y * hm.Width
	if hm.Width > int(^uint(0)>>1)-start {
		return nil, fmt.Errorf("row %d: dimensions overflow: %w", y, ErrOutOfBounds)
	}
	end := start + hm.Width
	if start < 0 || end < start || end > len(hm.Data) {
		return nil, fmt.Errorf("row %d: backing data has %d pixels: %w", y, len(hm.Data), ErrOutOfBounds)
	}
	return hm.Data[start:end], nil
}

// Read reads a version 1 heightmap from r.
func Read(r io.Reader) (*HeightMap, error) {
	var header [headerSize]byte
	n, err := io.ReadFull(r, header[:])
	if err != nil {
		return nil, fmt.Errorf("%w: read %d of %d bytes: %w", ErrInvalidHeader, n, headerSize, err)
	}

	magic := int32(binary.LittleEndian.Uint32(header[0:4]))
	if magic != 0x0108AAFF {
		return nil, fmt.Errorf("got magic %#08x, want %#08x: %w", uint32(magic), uint32(magicValue), ErrInvalidByteOrder)
	}
	height := int32(binary.LittleEndian.Uint32(header[4:8]))
	width := int32(binary.LittleEndian.Uint32(header[8:12]))
	pixelCount, err := checkedPixelCount(height, width)
	if err != nil {
		return nil, err
	}

	buffered := bufio.NewReader(r)
	compressed, err := gzip.NewReader(buffered)
	if err != nil {
		return nil, fmt.Errorf("open payload after byte %d: %w: %w", headerSize, ErrInvalidGzip, err)
	}
	compressed.Multistream(false)

	hm := &HeightMap{
		Height: int(height),
		Width:  int(width),
		Data:   make([]int32, pixelCount),
	}
	var decoded [decodeBufferSize]byte
	for first := 0; first < pixelCount; {
		batchPixels := min(pixelCount-first, len(decoded)/4)
		batch := decoded[:batchPixels*4]
		n, readErr := io.ReadFull(compressed, batch)
		if readErr != nil {
			_ = compressed.Close()
			return nil, fmt.Errorf("pixels %d..%d of %d: read %d of %d bytes: %w: %w", first, first+batchPixels-1, pixelCount, n, len(batch), ErrInvalidPayload, readErr)
		}
		for offset := 0; offset < batchPixels; offset++ {
			value := int32(binary.LittleEndian.Uint32(batch[offset*4 : offset*4+4]))
			if value < 0 && value != NoDataPixel {
				_ = compressed.Close()
				return nil, fmt.Errorf("pixel %d has reserved negative value %d: %w", first+offset, value, ErrInvalidPayload)
			}
			hm.Data[first+offset] = value
		}
		first += batchPixels
	}

	var extra [1]byte
	n, readErr := compressed.Read(extra[:])
	if n != 0 {
		_ = compressed.Close()
		return nil, fmt.Errorf("expected %d decompressed bytes, found more: %w", pixelCount*4, ErrInvalidPayload)
	}
	if !errors.Is(readErr, io.EOF) {
		_ = compressed.Close()
		return nil, fmt.Errorf("verify gzip checksum and size: %w: %w", ErrInvalidGzip, readErr)
	}
	if err := compressed.Close(); err != nil {
		return nil, fmt.Errorf("close payload: %w: %w", ErrInvalidGzip, err)
	}

	first, err := buffered.ReadByte()
	if errors.Is(err, io.EOF) {
		return hm, nil
	}
	if err != nil {
		return nil, fmt.Errorf("check for data after gzip member: %w: %w", ErrTrailingData, err)
	}
	second, secondErr := buffered.ReadByte()
	if secondErr == nil && first == 0x1f && second == 0x8b {
		return nil, fmt.Errorf("gzip member starts immediately after the first: %w", ErrMultipleGzipMembers)
	}
	if secondErr != nil && !errors.Is(secondErr, io.EOF) {
		return nil, fmt.Errorf("check for data after gzip member: %w: %w", ErrTrailingData, secondErr)
	}
	return nil, fmt.Errorf("first trailing byte is %#02x: %w", first, ErrTrailingData)
}

func checkedPixelCount(height, width int32) (int, error) {
	if height <= 0 || width <= 0 {
		return 0, fmt.Errorf("height %d and width %d must both be positive: %w", height, width, ErrInvalidDimensions)
	}
	pixels := uint64(height) * uint64(width)
	maxInt := uint64(^uint(0) >> 1)
	if pixels > maxInt/4 {
		return 0, fmt.Errorf("height %d by width %d exceeds the addressable payload size: %w", height, width, ErrInvalidDimensions)
	}
	return int(pixels), nil
}
