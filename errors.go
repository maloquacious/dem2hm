// Copyright (c) 2026 Michael D Henderson. All rights reserved.

package dem2hm

// Error defines a constant error
type Error string

// Error implements the Errors interface
func (e Error) Error() string { return string(e) }
