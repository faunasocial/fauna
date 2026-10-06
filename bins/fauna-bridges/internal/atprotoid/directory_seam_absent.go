//go:build !fauna_e2e_fixtures

// The production twin of directory_seam.go: the fake-directory redirect seam
// does not exist in this build. Convention 15 — the tag is the boundary, so a
// release bridge carries neither the `FAUNA_ATPROTO_PLC_DIRECTORY_URL` read nor
// any path that could point it at a directory other than the hard-coded one,
// and the production recipes grep the built artifact for the variable's name to
// prove it (`just atproto-bridge-build`).
//
// The signature below is the contract directory_seam.go implements. Keep them
// in sync.
package atprotoid

// PLCDirectoryBaseURL always returns the production directory: the environment
// is never consulted here.
func PLCDirectoryBaseURL() string {
	return DefaultPLCDirectoryURL
}
