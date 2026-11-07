package main

import (
	"bytes"
	"encoding/json"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// TestBuiltExecutableDescribesAndFormats drives the cross-compiled executable
// named by POLY_SH_BINARY over its real stdin and stdout, the way the host
// does. CI builds it natively on macOS and Windows runners with
// mise-tasks/sidecar/build.sh, so a binary that does not start on a platform
// fails there. Without the variable the test is skipped.
func TestBuiltExecutableDescribesAndFormats(t *testing.T) {
	built := os.Getenv("POLY_SH_BINARY")
	if built == "" {
		t.Skip("POLY_SH_BINARY is not set")
	}
	dir := t.TempDir()
	exe := filepath.Join(dir, filepath.Base(built))
	data, err := os.ReadFile(built) //nolint:gosec // the executable under test, named by CI
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(exe, data, 0o700); err != nil { //nolint:gosec // an executable must be runnable
		t.Fatal(err)
	}
	manifest, err := json.Marshal(testManifest())
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, manifestFile), manifest, 0o600); err != nil {
		t.Fatal(err)
	}

	requests := frames(t, `{"op":"describe"}`, registerFrame(t, 1), callFrame(t, 1, "if true; then\necho   hi\nfi\n"))
	input, err := io.ReadAll(requests)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(exe) //nolint:gosec // the executable under test
	cmd.Stdin = bytes.NewReader(input)
	var stdout bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		t.Fatalf("start %s: %v", exe, err)
	}
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("the executable exited with %v", err)
		}
	case <-time.After(30 * time.Second):
		_ = cmd.Process.Kill()
		t.Fatal("the executable did not exit after stdin closed")
	}

	described := nextReply(t, &stdout)
	var desc stdioDescriptor
	if described.Status != statusOK || json.Unmarshal(described.Payload, &desc) != nil || desc.ComponentID != "sh" {
		t.Fatalf("describe = status %d %q", described.Status, described.Payload)
	}
	if registered := nextReply(t, &stdout); registered.Status != statusOK {
		t.Fatalf("RegisterConfig = status %d %q", registered.Status, registered.Payload)
	}
	formatted := nextReply(t, &stdout)
	if formatted.Status != statusOK || !strings.Contains(string(formatted.Payload), "echo hi") {
		t.Fatalf("Format = status %d %q, want the reindented script", formatted.Status, formatted.Payload)
	}
}
