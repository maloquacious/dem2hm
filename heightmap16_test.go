// Copyright (c) 2026 Michael D Henderson. All rights reserved.

package dem2hm

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"reflect"
	"strings"
	"testing"
)

func TestReadHeightMap16AndAccessors(t *testing.T) {
	values := []int16{-37, 0, NoDataPixel16, 20, 3431, math.MaxInt16}
	input := heightmap16Fixture(t, testMetadata(2, 3), values)

	hm, err := ReadHeightMap16(bytes.NewReader(input))
	if err != nil {
		t.Fatalf("ReadHeightMap16: %v", err)
	}
	if hm.Height != 2 || hm.Width != 3 {
		t.Fatalf("dimensions = %dx%d, want 3x2", hm.Width, hm.Height)
	}
	if !reflect.DeepEqual(hm.Data, values) {
		t.Fatalf("Data = %v", hm.Data)
	}
	if hm.Metadata.Source.FileName != "small.tif" || *hm.Metadata.Elevation.VerticalDatum != "EGM96" || *hm.Metadata.Source.NoData != -999 {
		t.Fatalf("Metadata = %+v", hm.Metadata)
	}
	if pixel := hm.At(1, 1); pixel != 3431 {
		t.Fatalf("At(1, 1) = %d, want 3431", pixel)
	}
	pixel, err := hm.Pixel(2, 0)
	if err != nil || pixel != NoDataPixel16 {
		t.Fatalf("Pixel(2, 0) = %d, %v; want %d, nil", pixel, err, NoDataPixel16)
	}
	row, err := hm.Row(1)
	if err != nil || !reflect.DeepEqual(row, []int16{20, 3431, math.MaxInt16}) {
		t.Fatalf("Row(1) = %v, %v", row, err)
	}
	row[0] = 9
	if hm.Data[3] != 9 {
		t.Fatal("Row did not return a view into Data")
	}
}

func TestReadHeightMap16IgnoresUnknownMetadata(t *testing.T) {
	block := strings.Replace(string(metadataJSON(t, testMetadata(1, 1))), "{", `{"future":{"a":[1,2]},`, 1)
	input := heightmap16FixtureRaw(t, magic16Value, []byte(block), []int16{7})

	hm, err := ReadHeightMap16(bytes.NewReader(input))
	if err != nil || hm.At(0, 0) != 7 {
		t.Fatalf("ReadHeightMap16 = %v, %v", hm, err)
	}
}

func TestReadHeightMap16AcrossDecodeBufferBoundary(t *testing.T) {
	values := make([]int16, decodeBufferSize/2+1)
	for index := range values {
		values[index] = int16(index)
	}
	input := heightmap16Fixture(t, testMetadata(1, int32(len(values))), values)

	hm, err := ReadHeightMap16(bytes.NewReader(input))
	if err != nil {
		t.Fatalf("ReadHeightMap16: %v", err)
	}
	if !reflect.DeepEqual(hm.Data, values) {
		t.Fatal("Data differs across decode buffer boundary")
	}
}

func TestReadHeightMap16WithOptions(t *testing.T) {
	input := heightmap16Fixture(t, testMetadata(2, 3), []int16{0, 1, 2, 3, 4, 5})
	if _, err := ReadHeightMap16WithOptions(bytes.NewReader(input), Options{MaxPixels: 5}); !errors.Is(err, ErrLimitExceeded) {
		t.Fatalf("MaxPixels error = %v, want ErrLimitExceeded", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	reader := &cancelingReader{reader: bytes.NewReader(input), cancel: cancel}
	if _, err := ReadHeightMap16WithOptions(reader, Options{Context: ctx}); !errors.Is(err, ErrCanceled) || !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled error = %v, want ErrCanceled and context.Canceled", err)
	}
}

func TestReadersRejectOtherVersion(t *testing.T) {
	v1 := heightmapFixture(t, 1, 1, []int32{7})
	if _, err := ReadHeightMap16(bytes.NewReader(v1)); !errors.Is(err, ErrInvalidByteOrder) {
		t.Errorf("ReadHeightMap16(v1) error = %v, want ErrInvalidByteOrder", err)
	}
	v11 := heightmap16Fixture(t, testMetadata(1, 1), []int16{7})
	if _, err := ReadHeightMap(bytes.NewReader(v11)); !errors.Is(err, ErrInvalidByteOrder) {
		t.Errorf("ReadHeightMap(v1.1) error = %v, want ErrInvalidByteOrder", err)
	}
}

func TestReadHeightMap16RejectsMalformedHeader(t *testing.T) {
	valid := metadataJSON(t, testMetadata(1, 1))
	tests := []struct {
		name  string
		input []byte
		want  error
	}{
		{name: "truncated", input: make([]byte, 7), want: ErrInvalidHeader},
		{name: "magic", input: header16(magicValue, 10), want: ErrInvalidByteOrder},
		{name: "zero length", input: header16(magic16Value, 0), want: ErrInvalidHeader},
		{name: "negative length", input: header16(magic16Value, -1), want: ErrInvalidHeader},
		{name: "oversized length", input: header16(magic16Value, MaxMetadataSize+1), want: ErrInvalidHeader},
		{name: "truncated metadata", input: append(header16(magic16Value, int32(len(valid))), valid[:len(valid)-1]...), want: ErrInvalidMetadata},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := ReadHeightMap16(bytes.NewReader(test.input))
			if !errors.Is(err, test.want) {
				t.Fatalf("ReadHeightMap16 error = %v, want %v", err, test.want)
			}
		})
	}
}

func TestReadHeightMap16RejectsMalformedMetadata(t *testing.T) {
	modify := func(change func(*Metadata)) []byte {
		metadata := testMetadata(2, 3)
		change(&metadata)
		return metadataJSON(t, metadata)
	}
	valid := string(metadataJSON(t, testMetadata(2, 3)))
	tests := []struct {
		name  string
		block []byte
		want  error
	}{
		{name: "not json", block: []byte("not json"), want: ErrInvalidMetadata},
		{name: "invalid utf-8", block: []byte(strings.Replace(valid, "small.tif", "small\xff.tif", 1)), want: ErrInvalidMetadata},
		{name: "trailing json", block: []byte(valid + "{}"), want: ErrInvalidMetadata},
		{name: "array", block: []byte("[]"), want: ErrInvalidMetadata},
		{name: "height overflows int32", block: []byte(strings.Replace(valid, `"height":2`, `"height":2147483648`, 1)), want: ErrInvalidMetadata},
		{name: "missing dimensions", block: []byte("{}"), want: ErrInvalidDimensions},
		{name: "zero height", block: modify(func(m *Metadata) { m.Height, m.Source.Height = 0, 0 }), want: ErrInvalidDimensions},
		{name: "negative width", block: modify(func(m *Metadata) { m.Width, m.Source.Width = -3, -3 }), want: ErrInvalidDimensions},
		{name: "size overflow", block: modify(func(m *Metadata) {
			m.Height, m.Width, m.Source.Height, m.Source.Width = math.MaxInt32, math.MaxInt32, math.MaxInt32, math.MaxInt32
		}), want: ErrInvalidDimensions},
		{name: "rotation", block: modify(func(m *Metadata) { m.Transform.Rotate = 45 }), want: ErrInvalidMetadata},
		{name: "source dimensions", block: modify(func(m *Metadata) { m.Transform.Rotate = 90 }), want: ErrInvalidMetadata},
		{name: "elevation range", block: modify(func(m *Metadata) { m.Elevation.Minimum = 5000 }), want: ErrInvalidMetadata},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			input := heightmap16FixtureRaw(t, magic16Value, test.block, []int16{0, 1, 2, 3, 4, 5})
			_, err := ReadHeightMap16(bytes.NewReader(input))
			if !errors.Is(err, test.want) {
				t.Fatalf("ReadHeightMap16 error = %v, want %v", err, test.want)
			}
		})
	}
}

func TestReadHeightMap16RejectsMalformedPayload(t *testing.T) {
	metadata := testMetadata(1, 2)
	valid := heightmap16Fixture(t, metadata, []int16{7, 8})
	badCRC := append([]byte(nil), valid...)
	badCRC[len(badCRC)-8] ^= 0xff
	tests := []struct {
		name  string
		input []byte
		want  error
	}{
		{name: "missing gzip", input: heightmap16FixtureRaw(t, magic16Value, metadataJSON(t, metadata), nil)[:header16Size+len(metadataJSON(t, metadata))], want: ErrInvalidGzip},
		{name: "truncated decompressed payload", input: heightmap16Fixture(t, metadata, []int16{7}), want: ErrInvalidPayload},
		{name: "odd decompressed payload", input: append(append(header16(magic16Value, int32(len(metadataJSON(t, metadata)))), metadataJSON(t, metadata)...), gzipBytes(t, []byte{7, 0, 8})...), want: ErrInvalidPayload},
		{name: "oversized decompressed payload", input: heightmap16Fixture(t, metadata, []int16{7, 8, 9}), want: ErrInvalidPayload},
		{name: "bad crc", input: badCRC, want: ErrInvalidGzip},
		{name: "second gzip member", input: append(append([]byte(nil), valid...), gzipBytes(t, []byte{9, 0})...), want: ErrMultipleGzipMembers},
		{name: "trailing data", input: append(append([]byte(nil), valid...), 0xaa), want: ErrTrailingData},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := ReadHeightMap16(bytes.NewReader(test.input))
			if !errors.Is(err, test.want) {
				t.Fatalf("ReadHeightMap16 error = %v, want %v", err, test.want)
			}
		})
	}
}

func TestHeightMap16AccessorsRejectOutOfBounds(t *testing.T) {
	hm := &HeightMap16{Height: 2, Width: 3, Metadata: testMetadata(2, 3), Data: make([]int16, 6)}
	for _, coordinates := range [][2]int{{-1, 0}, {0, -1}, {3, 0}, {0, 2}} {
		x, y := coordinates[0], coordinates[1]
		if _, err := hm.Pixel(x, y); !errors.Is(err, ErrOutOfBounds) {
			t.Errorf("Pixel(%d, %d) error = %v, want ErrOutOfBounds", x, y, err)
		}
		if _, _, err := hm.SourcePixel(x, y); !errors.Is(err, ErrOutOfBounds) {
			t.Errorf("SourcePixel(%d, %d) error = %v, want ErrOutOfBounds", x, y, err)
		}
		if _, _, err := hm.LonLat(x, y); !errors.Is(err, ErrOutOfBounds) {
			t.Errorf("LonLat(%d, %d) error = %v, want ErrOutOfBounds", x, y, err)
		}
		func() {
			defer func() {
				if recover() == nil {
					t.Errorf("At(%d, %d) did not panic", x, y)
				}
			}()
			_ = hm.At(x, y)
		}()
	}
	if _, err := hm.Row(2); !errors.Is(err, ErrOutOfBounds) {
		t.Errorf("Row(2) error = %v, want ErrOutOfBounds", err)
	}
	short := &HeightMap16{Height: 2, Width: 3, Data: make([]int16, 5)}
	if _, err := short.Pixel(2, 1); !errors.Is(err, ErrOutOfBounds) {
		t.Errorf("Pixel on short data error = %v, want ErrOutOfBounds", err)
	}
	if _, err := short.Row(1); !errors.Is(err, ErrOutOfBounds) {
		t.Errorf("Row on short data error = %v, want ErrOutOfBounds", err)
	}
}

// forwardMap mirrors the converter's Transform::map: rotate clockwise, then
// flip in output coordinates.
func forwardMap(transform TransformMetadata, x, y, width, height int) (int, int) {
	outputWidth, outputHeight := width, height
	if transform.Rotate == 90 || transform.Rotate == 270 {
		outputWidth, outputHeight = height, width
	}
	switch transform.Rotate {
	case 90:
		x, y = height-1-y, x
	case 180:
		x, y = width-1-x, height-1-y
	case 270:
		x, y = y, width-1-x
	}
	if transform.FlipHorizontal {
		x = outputWidth - 1 - x
	}
	if transform.FlipVertical {
		y = outputHeight - 1 - y
	}
	return x, y
}

func TestSourcePixelInvertsEveryTransform(t *testing.T) {
	const sourceWidth, sourceHeight = 3, 2
	for _, rotate := range []int{0, 90, 180, 270} {
		for _, flips := range [][2]bool{{false, false}, {true, false}, {false, true}, {true, true}} {
			transform := TransformMetadata{Rotate: rotate, FlipHorizontal: flips[0], FlipVertical: flips[1]}
			t.Run(fmt.Sprintf("%+v", transform), func(t *testing.T) {
				metadata := testMetadata(sourceHeight, sourceWidth)
				metadata.Transform = transform
				if rotate == 90 || rotate == 270 {
					metadata.Height, metadata.Width = sourceWidth, sourceHeight
				}
				hm := &HeightMap16{Height: int(metadata.Height), Width: int(metadata.Width), Metadata: metadata}
				for y := range sourceHeight {
					for x := range sourceWidth {
						outputX, outputY := forwardMap(transform, x, y, sourceWidth, sourceHeight)
						sourceX, sourceY, err := hm.SourcePixel(outputX, outputY)
						if err != nil || sourceX != x || sourceY != y {
							t.Errorf("SourcePixel(%d, %d) = %d, %d, %v; want %d, %d", outputX, outputY, sourceX, sourceY, err, x, y)
						}
					}
				}
			})
		}
	}
}

func TestLonLatReturnsPixelCenter(t *testing.T) {
	metadata := testMetadata(3, 2)
	metadata.Transform.Rotate = 90
	metadata.Source.Width, metadata.Source.Height = 3, 2
	hm := &HeightMap16{Height: 3, Width: 2, Metadata: metadata}

	// Output (0, 0) of a clockwise rotation is the source's bottom-left pixel.
	lon, lat, err := hm.LonLat(0, 0)
	if err != nil || lon != -79.75 || lat != 8.625 {
		t.Fatalf("LonLat(0, 0) = %v, %v, %v; want -79.75, 8.625", lon, lat, err)
	}
}

func testMetadata(height, width int32) Metadata {
	datum := "EGM96"
	noData := int64(-999)
	return Metadata{
		Height:          height,
		Width:           width,
		Elevation:       ElevationMetadata{Minimum: -37, Maximum: 3431, VerticalDatum: &datum},
		PixelSizeMeters: 27650.5,
		Source: SourceMetadata{
			FileName:     "small.tif",
			SHA256:       strings.Repeat("0", 64),
			Width:        width,
			Height:       height,
			NoData:       &noData,
			GeoTransform: [6]float64{-80, 0.5, 0, 9, 0, -0.25},
		},
		Dem2hmVersion: "1.1.0",
	}
}

func metadataJSON(t *testing.T, metadata Metadata) []byte {
	t.Helper()
	block, err := json.Marshal(metadata)
	if err != nil {
		t.Fatalf("marshal metadata: %v", err)
	}
	return block
}

func heightmap16Fixture(t *testing.T, metadata Metadata, values []int16) []byte {
	t.Helper()
	return heightmap16FixtureRaw(t, magic16Value, metadataJSON(t, metadata), values)
}

func heightmap16FixtureRaw(t *testing.T, magic int32, block []byte, values []int16) []byte {
	t.Helper()
	payload := make([]byte, 2*len(values))
	for index, value := range values {
		binary.LittleEndian.PutUint16(payload[index*2:], uint16(value))
	}
	data := append(header16(magic, int32(len(block))), block...)
	return append(data, gzipBytes(t, payload)...)
}

func header16(magic, length int32) []byte {
	data := make([]byte, header16Size)
	binary.LittleEndian.PutUint32(data[0:4], uint32(magic))
	binary.LittleEndian.PutUint32(data[4:8], uint32(length))
	return data
}

func gzipBytes(t *testing.T, payload []byte) []byte {
	t.Helper()
	var data bytes.Buffer
	compressed := gzip.NewWriter(&data)
	if _, err := compressed.Write(payload); err != nil {
		t.Fatalf("write gzip fixture: %v", err)
	}
	if err := compressed.Close(); err != nil {
		t.Fatalf("close gzip fixture: %v", err)
	}
	return data.Bytes()
}
