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
