package main

import (
	"bytes"
	"encoding/binary"
	"io"
	"testing"
)

func TestReadFrame(t *testing.T) {
	withLen := func(n uint32, body string) []byte {
		out := make([]byte, 4)
		binary.LittleEndian.PutUint32(out, n)
		return append(out, body...)
	}
	tests := []struct {
		name    string
		in      []byte
		want    string
		wantErr bool
	}{
		{"one frame", withLen(2, "{}"), "{}", false},
		{"zero length", withLen(0, ""), "", true},
		{"over the limit", withLen(maxFrame+1, ""), "", true},
		{"truncated body", withLen(5, "ab"), "", true},
		{"eof", nil, "", true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := readFrame(bytes.NewReader(tt.in))
			if (err != nil) != tt.wantErr {
				t.Fatalf("err = %v, wantErr %v", err, tt.wantErr)
			}
			if string(got) != tt.want {
				t.Errorf("got %q, want %q", got, tt.want)
			}
		})
	}
}

func TestWriteFrameRoundTrips(t *testing.T) {
	var buf bytes.Buffer
	if err := writeFrame(&buf, []byte(`{"status":0}`)); err != nil {
		t.Fatal(err)
	}
	got, err := readFrame(&buf)
	if err != nil || string(got) != `{"status":0}` {
		t.Fatalf("got %q, %v", got, err)
	}
	if err := writeFrame(io.Discard, nil); err == nil {
		t.Fatal("want an error for an empty frame")
	}
}
