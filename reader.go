// Copyright (c) 2026 Michael D Henderson. All rights reserved.

// Package dem2hm implements a reader for a DEM file that's been converted to a normalized heightmap.
package dem2hm

const (
	// The reserved value `-2147483648` marks a no-data pixel.
	NoDataPixel         = -2147483648
	ErrInvalidByteOrder = Error("invalid heightmap byte order or format")
)

type HeightMap struct {
	Height int
	Width  int
	Data   []int32
}

func Read(r bytes.Reader) (*HeightMap, error) {
	var magic int32
	var hm HeightMap

	if err := binary.Read(r, binary.LittleEndian, &magic); err != nil {
		return nil, err
	}
	if magic != 0x0108AAFF {
		return nil, errors.Join("invalid heightmap byte order or format: %#x", magic)
	}
	if err := binary.Read(r, binary.LittleEndian, &hm.Height); err != nil {
		return nil, err
	}
	if err := binary.Read(r, binary.LittleEndian, &hm.Width); err != nil {
		return nil, err
	}

	// Version 1 has no padding or trailer, so its exact size is:
	// 12 + Height * Width * 4 bytes

	return hm, nil
}
