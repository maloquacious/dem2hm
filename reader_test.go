// Copyright (c) 2026 Michael D Henderson. All rights reserved.

package dem2hm

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"math"
	"reflect"
	"testing"
)

func TestReadAndAccessors(t *testing.T) {
	input := heightmapFixture(t, 2, 3, []int32{0, 1, NoDataPixel, 3, 4, math.MaxInt32})

	hm, err := Read(bytes.NewReader(input))
	if err != nil {
		t.Fatalf("Read: %v", err)
	}
	if hm.Height != 2 || hm.Width != 3 {
		t.Fatalf("dimensions = %dx%d, want 3x2", hm.Width, hm.Height)
	}
	if !reflect.DeepEqual(hm.Data, []int32{0, 1, NoDataPixel, 3, 4, math.MaxInt32}) {
		t.Fatalf("Data = %v", hm.Data)
	}
	if pixel := hm.At(1, 1); pixel != 4 {
		t.Fatalf("At(1, 1) = %d, want 4", pixel)
	}
	pixel, err := hm.Pixel(2, 0)
	if err != nil || pixel != NoDataPixel {
		t.Fatalf("Pixel(2, 0) = %d, %v; want %d, nil", pixel, err, NoDataPixel)
	}
	row, err := hm.Row(1)
	if err != nil || !reflect.DeepEqual(row, []int32{3, 4, math.MaxInt32}) {
		t.Fatalf("Row(1) = %v, %v", row, err)
	}
	row[0] = 9
	if hm.Data[3] != 9 {
		t.Fatal("Row did not return a view into Data")
	}
}

func TestReadAcrossDecodeBufferBoundary(t *testing.T) {
	values := make([]int32, decodeBufferSize/4+1)
	for index := range values {
		values[index] = int32(index)
	}
	input := heightmapFixture(t, 1, int32(len(values)), values)

	hm, err := Read(bytes.NewReader(input))
	if err != nil {
		t.Fatalf("Read: %v", err)
	}
	if !reflect.DeepEqual(hm.Data, values) {
		t.Fatal("Data differs across decode buffer boundary")
	}
}

func TestReadWithOptionsRejectsPixelLimit(t *testing.T) {
	input := heightmapFixture(t, 2, 3, []int32{0, 1, 2, 3, 4, 5})

	_, err := ReadWithOptions(bytes.NewReader(input), Options{MaxPixels: 5})
	if !errors.Is(err, ErrLimitExceeded) {
		t.Fatalf("ReadWithOptions error = %v, want ErrLimitExceeded", err)
	}
}

func TestReadWithOptionsHonorsCancellation(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	_, err := ReadWithOptions(bytes.NewReader(nil), Options{Context: ctx})
	if !errors.Is(err, ErrCanceled) || !errors.Is(err, context.Canceled) {
		t.Fatalf("ReadWithOptions error = %v, want ErrCanceled and context.Canceled", err)
	}
}

func TestReadWithOptionsHonorsCancellationDuringRead(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	input := heightmapFixture(t, 1, 1, []int32{7})
	reader := &cancelingReader{reader: bytes.NewReader(input), cancel: cancel}

	_, err := ReadWithOptions(reader, Options{Context: ctx})
	if !errors.Is(err, ErrCanceled) || !errors.Is(err, context.Canceled) {
		t.Fatalf("ReadWithOptions error = %v, want ErrCanceled and context.Canceled", err)
	}
}

func TestAccessorsRejectOutOfBounds(t *testing.T) {
	hm := &HeightMap{Height: 2, Width: 3, Data: make([]int32, 6)}
	for _, coordinates := range [][2]int{{-1, 0}, {0, -1}, {3, 0}, {0, 2}} {
		if _, err := hm.Pixel(coordinates[0], coordinates[1]); !errors.Is(err, ErrOutOfBounds) {
			t.Errorf("Pixel(%d, %d) error = %v, want ErrOutOfBounds", coordinates[0], coordinates[1], err)
		}
	}
	if _, err := hm.Row(2); !errors.Is(err, ErrOutOfBounds) {
		t.Errorf("Row(2) error = %v, want ErrOutOfBounds", err)
	}
	short := &HeightMap{Height: 2, Width: 3, Data: make([]int32, 5)}
	if _, err := short.Pixel(2, 1); !errors.Is(err, ErrOutOfBounds) {
		t.Errorf("Pixel on short data error = %v, want ErrOutOfBounds", err)
	}
	if _, err := short.Row(1); !errors.Is(err, ErrOutOfBounds) {
		t.Errorf("Row on short data error = %v, want ErrOutOfBounds", err)
	}
}

func TestAtPanicsOutOfBounds(t *testing.T) {
	hm := &HeightMap{Height: 2, Width: 3, Data: make([]int32, 6)}
	for _, coordinates := range [][2]int{{-1, 0}, {0, -1}, {3, 0}, {0, 2}} {
		t.Run(fmt.Sprintf("%d,%d", coordinates[0], coordinates[1]), func(t *testing.T) {
			defer func() {
				if recover() == nil {
					t.Errorf("At(%d, %d) did not panic", coordinates[0], coordinates[1])
				}
			}()
			_ = hm.At(coordinates[0], coordinates[1])
		})
	}
}

func TestReadRejectsMalformedHeader(t *testing.T) {
	tests := []struct {
		name  string
		input []byte
		want  error
	}{
		{name: "truncated", input: make([]byte, 11), want: ErrInvalidHeader},
		{name: "magic", input: append([]byte{0, 1, 2, 3}, make([]byte, 8)...), want: ErrInvalidByteOrder},
		{name: "zero height", input: rawHeader(magicValue, 0, 1), want: ErrInvalidDimensions},
		{name: "negative width", input: rawHeader(magicValue, 1, -1), want: ErrInvalidDimensions},
		{name: "size overflow", input: rawHeader(magicValue, math.MaxInt32, math.MaxInt32), want: ErrInvalidDimensions},
		{name: "exceeds allocation limit", input: rawHeader(magicValue, 1<<30, 1<<29), want: ErrInvalidDimensions},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := Read(bytes.NewReader(test.input))
			if !errors.Is(err, test.want) {
				t.Fatalf("Read error = %v, want %v", err, test.want)
			}
		})
	}
}

func TestReadRejectsMalformedPayload(t *testing.T) {
	valid := heightmapFixture(t, 1, 2, []int32{7, 8})
	truncatedPayload := heightmapFixture(t, 1, 2, []int32{7})
	oversizedPayload := heightmapFixture(t, 1, 2, []int32{7, 8, 9})
	invalidValue := heightmapFixture(t, 1, 1, []int32{-1})
	badCRC := append([]byte(nil), valid...)
	badCRC[len(badCRC)-8] ^= 0xff
	secondMember := append(append([]byte(nil), valid...), gzipFixture(t, []int32{9})...)
	trailing := append(append([]byte(nil), valid...), 0xaa)

	tests := []struct {
		name  string
		input []byte
		want  error
	}{
		{name: "missing gzip", input: rawHeader(magicValue, 1, 1), want: ErrInvalidGzip},
		{name: "truncated decompressed payload", input: truncatedPayload, want: ErrInvalidPayload},
		{name: "oversized decompressed payload", input: oversizedPayload, want: ErrInvalidPayload},
		{name: "reserved negative value", input: invalidValue, want: ErrInvalidPayload},
		{name: "bad crc", input: badCRC, want: ErrInvalidGzip},
		{name: "second gzip member", input: secondMember, want: ErrMultipleGzipMembers},
		{name: "trailing data", input: trailing, want: ErrTrailingData},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := Read(bytes.NewReader(test.input))
			if !errors.Is(err, test.want) {
				t.Fatalf("Read error = %v, want %v", err, test.want)
			}
		})
	}
}

func heightmapFixture(t *testing.T, height, width int32, values []int32) []byte {
	t.Helper()
	data := rawHeader(magicValue, height, width)
	return append(data, gzipFixture(t, values)...)
}

func rawHeader(magic, height, width int32) []byte {
	data := make([]byte, headerSize)
	binary.LittleEndian.PutUint32(data[0:4], uint32(magic))
	binary.LittleEndian.PutUint32(data[4:8], uint32(height))
	binary.LittleEndian.PutUint32(data[8:12], uint32(width))
	return data
}

func gzipFixture(t *testing.T, values []int32) []byte {
	t.Helper()
	var data bytes.Buffer
	compressed := gzip.NewWriter(&data)
	var word [4]byte
	for _, value := range values {
		binary.LittleEndian.PutUint32(word[:], uint32(value))
		if _, err := compressed.Write(word[:]); err != nil {
			t.Fatalf("write gzip fixture: %v", err)
		}
	}
	if err := compressed.Close(); err != nil {
		t.Fatalf("close gzip fixture: %v", err)
	}
	return data.Bytes()
}

type cancelingReader struct {
	reader   io.Reader
	cancel   context.CancelFunc
	canceled bool
}

func (r *cancelingReader) Read(buffer []byte) (int, error) {
	n, err := r.reader.Read(buffer)
	if !r.canceled {
		r.canceled = true
		r.cancel()
	}
	return n, err
}
