// Copyright (c) 2026 Michael D Henderson. All rights reserved.

package dem2hm

import (
	"github.com/maloquacious/semver"
)

var (
	version = semver.Version{
		Major: 1,
		Minor: 0,
		Patch: 0,
	}
)

func Version() semver.Version {
	return version
}
