package main

import (
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
)

// maxFrame is the largest frame alef's stdio transport accepts (16 MiB).
const maxFrame = 16 * 1024 * 1024

// readFrame reads one frame: a 4-byte little-endian length, then that many
// bytes of JSON.
func readFrame(r io.Reader) ([]byte, error) {
	var header [4]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return nil, err
	}
	n := binary.LittleEndian.Uint32(header[:])
	if n == 0 || n > maxFrame {
		return nil, fmt.Errorf("frame length %d is outside 1..=%d", n, maxFrame)
	}
	body := make([]byte, n)
	if _, err := io.ReadFull(r, body); err != nil {
		return nil, fmt.Errorf("read frame body: %w", err)
	}
	return body, nil
}

// writeFrame writes one frame in a single write, so a reader never sees a
// header without its body.
func writeFrame(w io.Writer, body []byte) error {
	if len(body) == 0 || len(body) > maxFrame {
		return fmt.Errorf("frame length %d is outside 1..=%d", len(body), maxFrame)
	}
	frame := make([]byte, 4+len(body))
	binary.LittleEndian.PutUint32(frame, uint32(len(body)))
	copy(frame[4:], body)
	if _, err := w.Write(frame); err != nil {
		return fmt.Errorf("write frame: %w", err)
	}
	return nil
}

// byteArray is a []byte that JSON-encodes as an array of numbers, which is
// how alef's stdio envelopes carry bytes (not base64).
type byteArray []byte

func (b byteArray) MarshalJSON() ([]byte, error) {
	numbers := make([]int, len(b))
	for i, v := range b {
		numbers[i] = int(v)
	}
	return json.Marshal(numbers)
}

func (b *byteArray) UnmarshalJSON(data []byte) error {
	var numbers []int
	if err := json.Unmarshal(data, &numbers); err != nil {
		return err
	}
	out := make([]byte, len(numbers))
	for i, v := range numbers {
		if v < 0 || v > 255 {
			return errors.New("byte value outside 0..=255")
		}
		out[i] = byte(v)
	}
	*b = out
	return nil
}

// envelope is a request from the host.
type envelope struct {
	Op       string    `json:"op"`
	Contract string    `json:"contract"`
	Method   string    `json:"method"`
	Payload  byteArray `json:"payload"`
}

// reply is a response to the host. A non-zero Status carries an error message
// as its payload and leaves the connection usable.
type reply struct {
	Status  int       `json:"status"`
	Payload byteArray `json:"payload"`
}
