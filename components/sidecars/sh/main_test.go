package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func testManifest() componentManifest {
	var m componentManifest
	m.Identity.Component = "sh"
	m.Identity.Version = "1.2.3"
	m.Identity.FeatureHash = "ab"
	m.Provides = []providedContract{{Contract: "poly", InterfaceVersion: 1, ContractHash: "cd", Implementation: "NewShComponent"}}
	return m
}

func frames(t *testing.T, requests ...string) io.Reader {
	t.Helper()
	var buf bytes.Buffer
	for _, r := range requests {
		if err := writeFrame(&buf, []byte(r)); err != nil {
			t.Fatal(err)
		}
	}
	return &buf
}

func TestRunAnswersEachFrameOnceThenStopsAtEOF(t *testing.T) {
	var out bytes.Buffer
	in := frames(t,
		`{"op":"describe"}`,
		`{"op":"call","contract":"poly","method":"nope","payload":[]}`,
	)
	err := run(in, &out, testManifest())
	if !errors.Is(err, io.EOF) {
		t.Fatalf("err = %v, want EOF at the end of input", err)
	}
	first, err := readFrame(&out)
	if err != nil {
		t.Fatal(err)
	}
	var desc stdioDescriptor
	var envelope reply
	if err := json.Unmarshal(first, &envelope); err != nil || envelope.Status != statusOK {
		t.Fatalf("describe reply: %s %v", first, err)
	}
	if err := json.Unmarshal(envelope.Payload, &desc); err != nil {
		t.Fatal(err)
	}
	want := testManifest().descriptor()
	if desc.ComponentID != want.ComponentID || desc.ComponentVer != "1.2.3" || desc.FeatureSetHash != "ab" ||
		len(desc.Provides) != 1 || desc.Provides[0] != want.Provides[0] {
		t.Errorf("descriptor = %+v", desc)
	}
	second, err := readFrame(&out)
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(second, &envelope); err != nil || envelope.Status != statusFailed {
		t.Fatalf("a bad method must be a status error: %s %v", second, err)
	}
	if _, err := readFrame(&out); !errors.Is(err, io.EOF) {
		t.Fatalf("a third reply was written: %v", err)
	}
}

func TestRunRejectsBrokenInput(t *testing.T) {
	tests := []struct {
		name string
		in   io.Reader
	}{
		{"not json", frames(t, `nope`)},
		{"unknown op", frames(t, `{"op":"other"}`)},
		{"zero-length frame", bytes.NewReader(binary.LittleEndian.AppendUint32(nil, 0))},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := run(tt.in, io.Discard, testManifest())
			if err == nil || errors.Is(err, io.EOF) {
				t.Fatalf("err = %v, want a protocol error", err)
			}
		})
	}
}

func TestReadManifest(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, manifestFile)
	if _, err := readManifest(path); err == nil {
		t.Fatal("want an error for a missing manifest")
	}
	doc := `{"identity":{"component":"sh","version":"0.9.0","feature_hash":"ff"},"provides":[{"contract":"poly","interface_version":1,"contract_hash":"aa","implementation":"X"}]}`
	if err := os.WriteFile(path, []byte(doc), 0o600); err != nil {
		t.Fatal(err)
	}
	m, err := readManifest(path)
	if err != nil {
		t.Fatal(err)
	}
	if m.Identity.Version != "0.9.0" || m.Provides[0].ContractHash != "aa" {
		t.Errorf("manifest = %+v", m)
	}
}

// callFrame is a `call` envelope carrying a Format request for run.sh.
func callFrame(t *testing.T, id uint32, src string) string {
	t.Helper()
	header := mustPack(t, formatRequestFor(id, "run.sh", len(src)))
	payload := binary.LittleEndian.AppendUint32(nil, uint32(len(header))) //nolint:gosec // a test header is small
	payload = append(payload, header...)
	payload = append(payload, src...)
	body, err := json.Marshal(envelope{Op: "call", Contract: "poly", Method: "call", Payload: payload})
	if err != nil {
		t.Fatal(err)
	}
	return string(body)
}

func registerFrame(t *testing.T, id uint32) string {
	t.Helper()
	header := mustPack(t, map[string]any{"RegisterConfig": map[string]any{
		"id": id, "config": wireTab("indent_width", wireInt(2), "options", wireTab()),
	}})
	payload := binary.LittleEndian.AppendUint32(nil, uint32(len(header))) //nolint:gosec // a test header is small
	body, err := json.Marshal(envelope{Op: "call", Contract: "poly", Method: "call", Payload: append(payload, header...)})
	if err != nil {
		t.Fatal(err)
	}
	return string(body)
}

func nextReply(t *testing.T, out io.Reader) reply {
	t.Helper()
	raw, err := readFrame(out)
	if err != nil {
		t.Fatalf("no reply: %v", err)
	}
	var r reply
	if err := json.Unmarshal(raw, &r); err != nil {
		t.Fatal(err)
	}
	return r
}

// A reply over the frame limit would make the host drop the connection; it must
// be a per-call error and the next request must still be served.
func TestRunAnswersAnOversizeReplyWithAnErrorAndKeepsServing(t *testing.T) {
	srv := newServer("1.2.3")
	huge := strings.Repeat("e", maxFrame/4+1024) // four bytes per byte as a JSON number array
	srv.format = func(path string, src []byte, opts formatOptions) (string, bool, error) {
		if strings.Contains(string(src), "echo   a") {
			return huge, true, nil
		}
		return formatSource(path, src, opts)
	}
	in := frames(t, registerFrame(t, 1), callFrame(t, 1, "echo   a\n"), callFrame(t, 1, "echo b\n"))
	var out bytes.Buffer
	if err := serve(in, &out, testManifest(), srv); !errors.Is(err, io.EOF) {
		t.Fatalf("err = %v, want EOF after serving every frame", err)
	}
	if r := nextReply(t, &out); r.Status != statusOK {
		t.Fatalf("RegisterConfig status = %d", r.Status)
	}
	big := nextReply(t, &out)
	if big.Status != statusFailed || !strings.Contains(string(big.Payload), "frame limit") {
		t.Fatalf("oversize reply = status %d %q, want a frame-limit error", big.Status, big.Payload)
	}
	if r := nextReply(t, &out); r.Status != statusOK {
		t.Fatalf("the call after the oversize one: status %d %q", r.Status, r.Payload)
	}
}

// Four megabytes is about the largest file whose request fits one frame, and the
// real formatter must take it.
func TestRunFormatsALargeFile(t *testing.T) {
	src := "# " + strings.Repeat("e", 3_900_000) + "\n"
	in := frames(t, registerFrame(t, 1), callFrame(t, 1, src))
	var out bytes.Buffer
	if err := run(in, &out, testManifest()); !errors.Is(err, io.EOF) {
		t.Fatalf("err = %v", err)
	}
	nextReply(t, &out)
	if r := nextReply(t, &out); r.Status != statusOK {
		t.Fatalf("status %d %q", r.Status, r.Payload)
	}
}

func TestHandleCallRecoversFromAPanic(t *testing.T) {
	srv := newServer("1.2.3")
	srv.format = func(string, []byte, formatOptions) (string, bool, error) { panic("printer exploded") }
	in := frames(t, registerFrame(t, 1), callFrame(t, 1, "echo a\n"), callFrame(t, 1, "echo b\n"))
	var out bytes.Buffer
	if err := serve(in, &out, testManifest(), srv); !errors.Is(err, io.EOF) {
		t.Fatalf("err = %v, want EOF: a panic must not end the process", err)
	}
	nextReply(t, &out)
	for range 2 {
		r := nextReply(t, &out)
		if r.Status != statusFailed || !strings.Contains(string(r.Payload), "printer exploded") {
			t.Fatalf("panicking call = status %d %q, want a failed reply naming the panic", r.Status, r.Payload)
		}
	}
}
