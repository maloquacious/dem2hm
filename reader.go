// Copyright (c) 2026 Michael D Henderson. All rights reserved.

// Package dem2hm reads heightmaps produced by the dem2hm converter.
package dem2hm

import (
	"bufio"
	"compress/gzip"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"math"
)

const (
	magicValue       = 0x0108_AAFF
	headerSize       = 12
	decodeBufferSize = 32 * 1024
	// maxPayloadBytes is the largest slice the Go runtime can allocate: the
	// address space on 64-bit platforms, and math.MaxInt on 32-bit ones.
	maxPayloadBytes = min(math.MaxInt, 1<<48)

	// NoDataPixel is the value reserved for a pixel with no elevation data.
	NoDataPixel int32 = -2147483648

	// ErrInvalidByteOrder indicates that the file does not start with the
	// expected magic value in little-endian byte order.
	ErrInvalidByteOrder = Error("invalid heightmap byte order or format")
	// ErrInvalidHeader indicates that the heightmap header is incomplete or,
	// for version 1.1, declares an invalid metadata length.
	ErrInvalidHeader = Error("invalid heightmap header")
	// ErrInvalidMetadata indicates that the version 1.1 JSON metadata block is
	// not valid UTF-8 JSON or does not describe a consistent heightmap.
	ErrInvalidMetadata = Error("invalid heightmap metadata")
	// ErrInvalidDimensions indicates that the height or width is not positive,
	// or that the resulting payload size cannot be represented safely.
	ErrInvalidDimensions = Error("invalid heightmap dimensions")
	// ErrInvalidGzip indicates that the payload is not a valid gzip stream.
	ErrInvalidGzip = Error("invalid heightmap gzip stream")
	// ErrInvalidPayload indicates that the decompressed pixel payload does not
	// conform to the heightmap format.
	ErrInvalidPayload = Error("invalid heightmap payload")
	// ErrMultipleGzipMembers indicates that a second gzip member follows the
	// required payload member.
	ErrMultipleGzipMembers = Error("multiple gzip members")
	// ErrTrailingData indicates that data follows the single gzip member.
	ErrTrailingData = Error("trailing heightmap data")
	// ErrOutOfBounds indicates that a requested pixel or row is outside the map.
	ErrOutOfBounds = Error("heightmap coordinates out of bounds")
	// ErrLimitExceeded indicates that the heightmap exceeds a configured limit.
	ErrLimitExceeded = Error("heightmap resource limit exceeded")
	// ErrCanceled indicates that reading was canceled or its deadline expired.
	ErrCanceled = Error("heightmap read canceled")
)

// Options controls resource use while reading a heightmap.
type Options struct {
	// Context is checked while reading and decoding. A nil Context is treated
	// as context.Background(). Cancellation cannot interrupt an underlying
	// io.Reader blocked in Read unless that reader also honors cancellation,
	// deadlines, or closure.
	Context context.Context
	// MaxPixels is the largest decoded heightmap to accept. Zero disables the
	// additional limit; format and address-space limits still apply.
	MaxPixels uint64
}

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
func (hm *HeightMap) Row(y int) ([]int32, error) {
	if hm == nil {
		return nil, fmt.Errorf("row %d: %w", y, ErrOutOfBounds)
	}
	start, end, err := rowRange(hm.Height, hm.Width, len(hm.Data), y)
	if err != nil {
		return nil, err
	}
	return hm.Data[start:end], nil
}

// pixelIndex returns the index of pixel (x, y) in row-major data of dataLen
// pixels.
func pixelIndex(height, width, dataLen, x, y int) (int, error) {
	if x < 0 || y < 0 || x >= width || y >= height {
		return 0, fmt.Errorf("pixel (%d, %d): %w", x, y, ErrOutOfBounds)
	}
	if y > int(^uint(0)>>1)/width {
		return 0, fmt.Errorf("pixel (%d, %d): dimensions overflow: %w", x, y, ErrOutOfBounds)
	}
	index := y*width + x
	if index < 0 || index >= dataLen {
		return 0, fmt.Errorf("pixel (%d, %d): backing data has %d pixels: %w", x, y, dataLen, ErrOutOfBounds)
	}
	return index, nil
}

// rowRange returns the bounds of row y in row-major data of dataLen pixels.
func rowRange(height, width, dataLen, y int) (int, int, error) {
	if y < 0 || y >= height {
		return 0, 0, fmt.Errorf("row %d: %w", y, ErrOutOfBounds)
	}
	if y > int(^uint(0)>>1)/width {
		return 0, 0, fmt.Errorf("row %d: dimensions overflow: %w", y, ErrOutOfBounds)
	}
	start := y * width
	if width > int(^uint(0)>>1)-start {
		return 0, 0, fmt.Errorf("row %d: dimensions overflow: %w", y, ErrOutOfBounds)
	}
	end := start + width
	if start < 0 || end < start || end > dataLen {
		return 0, 0, fmt.Errorf("row %d: backing data has %d pixels: %w", y, dataLen, ErrOutOfBounds)
	}
	return start, end, nil
}

// Read reads a version 1 heightmap from r. It forwards to ReadHeightMap.
func Read(r io.Reader) (*HeightMap, error) {
	return ReadHeightMap(r)
}

// ReadWithOptions reads a version 1 heightmap from r using options. It
// forwards to ReadHeightMapWithOptions.
func ReadWithOptions(r io.Reader, options Options) (*HeightMap, error) {
	return ReadHeightMapWithOptions(r, options)
}

// ReadHeightMap reads a version 1 heightmap from r without an
// application-defined resource limit or cancellation context.
func ReadHeightMap(r io.Reader) (*HeightMap, error) {
	return ReadHeightMapWithOptions(r, Options{})
}

// ReadHeightMapWithOptions reads a version 1 heightmap from r using options.
func ReadHeightMapWithOptions(r io.Reader, options Options) (*HeightMap, error) {
	ctx, r, err := startRead(r, options)
	if err != nil {
		return nil, err
	}

	var header [headerSize]byte
	if err := readHeader(r, header[:]); err != nil {
		return nil, err
	}
	magic := int32(binary.LittleEndian.Uint32(header[0:4]))
	if magic != magicValue {
		return nil, fmt.Errorf("got magic %#08x, want %#08x: %w", uint32(magic), uint32(magicValue), ErrInvalidByteOrder)
	}
	height := int32(binary.LittleEndian.Uint32(header[4:8]))
	width := int32(binary.LittleEndian.Uint32(header[8:12]))
	pixelCount, err := checkedPixelCount(height, width, 4, options.MaxPixels)
	if err != nil {
		return nil, err
	}

	hm := &HeightMap{
		Height: int(height),
		Width:  int(width),
		Data:   make([]int32, pixelCount),
	}
	err = readPayload(ctx, r, headerSize, pixelCount, 4, func(first int, batch []byte) error {
		for offset := range len(batch) / 4 {
			value := int32(binary.LittleEndian.Uint32(batch[offset*4 : offset*4+4]))
			if value < 0 && value != NoDataPixel {
				return fmt.Errorf("pixel %d has reserved negative value %d: %w", first+offset, value, ErrInvalidPayload)
			}
			hm.Data[first+offset] = value
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return hm, nil
}

// startRead applies the options' context to r.
func startRead(r io.Reader, options Options) (context.Context, io.Reader, error) {
	ctx := options.Context
	if ctx == nil {
		ctx = context.Background()
	}
	if err := contextError(ctx.Err()); err != nil {
		return nil, nil, err
	}
	return ctx, contextReader{context: ctx, reader: r}, nil
}

// readHeader fills header from r.
func readHeader(r io.Reader, header []byte) error {
	n, err := io.ReadFull(r, header)
	if err != nil {
		if canceled := contextError(err); canceled != nil {
			return canceled
		}
		return fmt.Errorf("%w: read %d of %d bytes: %w", ErrInvalidHeader, n, len(header), err)
	}
	return nil
}

// readPayload decompresses the single gzip member that starts at byte offset
// of r, passing each batch of whole samples to store, and verifies that the
// member holds exactly pixelCount samples and that nothing follows it.
func readPayload(ctx context.Context, r io.Reader, offset, pixelCount, sampleSize int, store func(first int, batch []byte) error) error {
	buffered := bufio.NewReader(r)
	compressed, err := gzip.NewReader(buffered)
	if err != nil {
		if canceled := contextError(err); canceled != nil {
			return canceled
		}
		return fmt.Errorf("open payload after byte %d: %w: %w", offset, ErrInvalidGzip, err)
	}
	compressed.Multistream(false)

	var decoded [decodeBufferSize]byte
	for first := 0; first < pixelCount; {
		batchPixels := min(pixelCount-first, len(decoded)/sampleSize)
		batch := decoded[:batchPixels*sampleSize]
		n, readErr := io.ReadFull(compressed, batch)
		if readErr != nil {
			_ = compressed.Close()
			if canceled := contextError(readErr); canceled != nil {
				return canceled
			}
			return fmt.Errorf("pixels %d..%d of %d: read %d of %d bytes: %w: %w", first, first+batchPixels-1, pixelCount, n, len(batch), ErrInvalidPayload, readErr)
		}
		if canceled := contextError(ctx.Err()); canceled != nil {
			_ = compressed.Close()
			return canceled
		}
		if err := store(first, batch); err != nil {
			_ = compressed.Close()
			return err
		}
		first += batchPixels
	}

	var extra [1]byte
	n, readErr := compressed.Read(extra[:])
	if n != 0 {
		_ = compressed.Close()
		return fmt.Errorf("expected %d decompressed bytes, found more: %w", pixelCount*sampleSize, ErrInvalidPayload)
	}
	if !errors.Is(readErr, io.EOF) {
		_ = compressed.Close()
		if canceled := contextError(readErr); canceled != nil {
			return canceled
		}
		return fmt.Errorf("verify gzip checksum and size: %w: %w", ErrInvalidGzip, readErr)
	}
	if err := compressed.Close(); err != nil {
		return fmt.Errorf("close payload: %w: %w", ErrInvalidGzip, err)
	}

	first, err := buffered.ReadByte()
	if errors.Is(err, io.EOF) {
		return contextError(ctx.Err())
	}
	if err != nil {
		if canceled := contextError(err); canceled != nil {
			return canceled
		}
		return fmt.Errorf("check for data after gzip member: %w: %w", ErrTrailingData, err)
	}
	second, secondErr := buffered.ReadByte()
	if secondErr == nil && first == 0x1f && second == 0x8b {
		return fmt.Errorf("gzip member starts immediately after the first: %w", ErrMultipleGzipMembers)
	}
	if secondErr != nil && !errors.Is(secondErr, io.EOF) {
		if canceled := contextError(secondErr); canceled != nil {
			return canceled
		}
		return fmt.Errorf("check for data after gzip member: %w: %w", ErrTrailingData, secondErr)
	}
	return fmt.Errorf("first trailing byte is %#02x: %w", first, ErrTrailingData)
}

func checkedPixelCount(height, width int32, sampleSize uint64, maxPixels uint64) (int, error) {
	if height <= 0 || width <= 0 {
		return 0, fmt.Errorf("height %d and width %d must both be positive: %w", height, width, ErrInvalidDimensions)
	}
	pixels := uint64(height) * uint64(width)
	if maxPixels != 0 && pixels > maxPixels {
		return 0, fmt.Errorf("height %d by width %d is %d pixels, exceeding the configured limit of %d: %w", height, width, pixels, maxPixels, ErrLimitExceeded)
	}
	if pixels > maxPayloadBytes/sampleSize {
		return 0, fmt.Errorf("height %d by width %d exceeds the addressable payload size: %w", height, width, ErrInvalidDimensions)
	}
	return int(pixels), nil
}

type contextReader struct {
	context context.Context
	reader  io.Reader
}

func (r contextReader) Read(buffer []byte) (int, error) {
	if err := r.context.Err(); err != nil {
		return 0, err
	}
	n, err := r.reader.Read(buffer)
	if err == nil {
		err = r.context.Err()
	}
	return n, err
}

func contextError(err error) error {
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return fmt.Errorf("%w: %w", ErrCanceled, err)
	}
	return nil
}
