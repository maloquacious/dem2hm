// Copyright (c) 2026 Michael D Henderson. All rights reserved.

package dem2hm_test

import (
	"bytes"
	_ "embed"
	"fmt"

	"github.com/maloquacious/dem2hm"
)

//go:embed testdata/example.hmz
var exampleHMZ []byte

func ExampleRead() {
	heightmap, err := dem2hm.Read(bytes.NewReader(exampleHMZ))
	if err != nil {
		fmt.Println("error:", err)
		return
	}

	emptyPixels := 0
	for _, pixel := range heightmap.Data {
		if pixel == dem2hm.NoDataPixel {
			emptyPixels++
		}
	}

	fmt.Println("height:", heightmap.Height)
	fmt.Println("width:", heightmap.Width)
	fmt.Println("empty pixels:", emptyPixels)

	// Output:
	// height: 2
	// width: 3
	// empty pixels: 2
}

//go:embed testdata/example16.hmz
var example16HMZ []byte

func ExampleReadHeightMap16() {
	heightmap, err := dem2hm.ReadHeightMap16(bytes.NewReader(example16HMZ))
	if err != nil {
		fmt.Println("error:", err)
		return
	}

	aboveSeaLevel, outsideSource := 0, 0
	for _, elevation := range heightmap.Data {
		switch {
		case elevation == dem2hm.NoDataPixel16:
			outsideSource++
		case elevation > 0:
			aboveSeaLevel++
		}
	}
	lon, lat, err := heightmap.LonLat(1, 1)
	if err != nil {
		fmt.Println("error:", err)
		return
	}

	fmt.Println("height:", heightmap.Height)
	fmt.Println("width:", heightmap.Width)
	fmt.Println("elevation (1, 1):", heightmap.At(1, 1), "m")
	fmt.Println("above sea level:", aboveSeaLevel)
	fmt.Println("outside source:", outsideSource)
	fmt.Println("vertical datum:", *heightmap.Metadata.Elevation.VerticalDatum)
	fmt.Printf("center of (1, 1): %.3f, %.3f\n", lon, lat)

	// Output:
	// height: 2
	// width: 3
	// elevation (1, 1): 1830 m
	// above sea level: 2
	// outside source: 2
	// vertical datum: EGM96
	// center of (1, 1): -79.625, 8.625
}
