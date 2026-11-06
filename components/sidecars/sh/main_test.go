package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
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
