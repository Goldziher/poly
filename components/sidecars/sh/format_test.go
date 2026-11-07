package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strconv"
	"testing"

	"mvdan.cc/sh/v3/syntax"
)

// The golden files are the output of the host `shfmt -i <width>` (v3.14.1) on
// each fixture read from stdin, so these tests pin byte parity with the tool
// the component replaces.
func TestFormatMatchesShfmtGoldens(t *testing.T) {
	fixtures := []string{"basic.sh", "bash_arrays.bash", "mksh.sh", "already.sh", "bats.bats"}
	for _, fixture := range fixtures {
		for _, indent := range []uint{2, 4, 0} {
			t.Run(fixture+"/i"+strconv.Itoa(int(indent)), func(t *testing.T) {
				src, err := os.ReadFile(filepath.Join("testdata", fixture)) //nolint:gosec // fixed fixture names
				if err != nil {
					t.Fatal(err)
				}
				want, err := os.ReadFile(filepath.Join("testdata", "golden", fixture+".i"+strconv.Itoa(int(indent)))) //nolint:gosec // fixed fixture names
				if err != nil {
					t.Fatal(err)
				}
				got, changed, err := formatSource(fixture, src, formatOptions{indent: indent})
				if err != nil {
					t.Fatalf("formatSource: %v", err)
				}
				if !changed {
					got = string(src)
				}
				if got != string(want) {
					t.Errorf("output differs from shfmt\n got: %q\nwant: %q", got, want)
				}
				if changed == (string(src) == string(want)) {
					t.Errorf("changed = %v, but source equals golden is %v", changed, string(src) == string(want))
				}
			})
		}
	}
}

func TestFormatRejectsUnparsableSource(t *testing.T) {
	src, err := os.ReadFile(filepath.Join("testdata", "broken.sh"))
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := formatSource("broken.sh", src, formatOptions{indent: 2}); err == nil {
		t.Fatal("want a parse error")
	}
}

func TestVariantFor(t *testing.T) {
	tests := []struct {
		name string
		path string
		src  string
		want syntax.LangVariant
	}{
		{"extension bash", "run.bash", "echo\n", syntax.LangBash},
		{"extension mksh", "run.mksh", "echo\n", syntax.LangMirBSDKorn},
		{"extension bats", "t.bats", "echo\n", syntax.LangBats},
		{"sh falls to shebang bash", "run.sh", "#!/usr/bin/env bash\n", syntax.LangBash},
		{"sh falls to shebang posix", "run.sh", "#!/bin/sh\n", syntax.LangPOSIX},
		{"sh with no shebang is bash", "run.sh", "echo\n", syntax.LangBash},
		{"no extension, mksh shebang", "run", "#!/bin/mksh\n", syntax.LangMirBSDKorn},
		{"zshrc", ".zshrc", "echo\n", syntax.LangZsh},
		{"bashrc", ".bashrc", "echo\n", syntax.LangBash},
		{"unknown shebang is bash", "run", "#!/usr/bin/fish\n", syntax.LangBash},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := variantFor(tt.path, []byte(tt.src)); got != tt.want {
				t.Errorf("variantFor(%q) = %v, want %v", tt.path, got, tt.want)
			}
		})
	}
}

// dialects.json is shared with the Rust host test
// host_hooks::shell_dialect::tests, so the two classifiers cannot drift.
func TestDialectForMatchesTheSharedTable(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("testdata", "dialects.json"))
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Path    string `json:"path"`
		Content string `json:"content"`
		Dialect string `json:"dialect"`
	}
	if err := json.Unmarshal(raw, &cases); err != nil {
		t.Fatal(err)
	}
	if len(cases) == 0 {
		t.Fatal("dialects.json has no cases")
	}
	for _, c := range cases {
		if got := dialectFor(c.Path, []byte(c.Content)); got != c.Dialect {
			t.Errorf("dialectFor(%q, %q) = %q, want %q", c.Path, c.Content, got, c.Dialect)
		}
	}
}

func TestIdenticalBytesFormatPerDialectOfThePath(t *testing.T) {
	src := []byte("@test \"adds\" {\nrun   echo hi\n}\n")
	if _, changed, err := formatSource("a.sh", src, formatOptions{indent: 2}); err == nil && changed {
		t.Error("a bats test block is not valid bash, but a.sh formatted it")
	}
	formatted, changed, err := formatSource("a.bats", src, formatOptions{indent: 2})
	if err != nil || !changed {
		t.Fatalf("a.bats: changed=%v err=%v, want it formatted", changed, err)
	}
	if want := "@test \"adds\" {\n  run echo hi\n}\n"; formatted != want {
		t.Errorf("a.bats = %q, want %q", formatted, want)
	}
}
